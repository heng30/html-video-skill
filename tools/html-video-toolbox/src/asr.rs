use crate::text::{self, print_json, read_json, write_json};
use anyhow::{Context, Result, anyhow, bail};
use fun_ast_nano::{FunASRModelConfig, FunAsrNanoGenerateModel, TranscriptionRequest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const DEFAULT_PROMPT: &str = "请将这段音频转写为文字。";
const DEFAULT_MIN_SIMILARITY: f64 = 0.85;

#[derive(clap::Args)]
pub struct Args {
    /// article directory
    pub dir: PathBuf,
    /// Fun-ASR-Nano model directory (model.pt + Qwen3-0.6B/tokenizer.json)
    #[arg(long, env = "FUNASR_MODEL_DIR")]
    pub model_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 512)]
    pub max_tokens: u32,
    #[arg(long, default_value = DEFAULT_PROMPT)]
    pub prompt: String,
    /// sentences below this similarity are queued for re-synthesis
    #[arg(long, default_value_t = DEFAULT_MIN_SIMILARITY)]
    pub min_similarity: f64,
}

fn resolve_model_dir(cli: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = cli {
        return Ok(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    Ok(PathBuf::from(home).join("models/Fun-ASR-Nano-2512"))
}

fn digit_cn(c: char) -> char {
    match c {
        '0' => '零',
        '1' => '一',
        '2' => '二',
        '3' => '三',
        '4' => '四',
        '5' => '五',
        '6' => '六',
        '7' => '七',
        '8' => '八',
        _ => '九',
    }
}

/// 只留字母/数字/汉字：阿拉伯数字逐位转中文，避免「2024 年」和「二零二四年」判成不一致。
/// shortcut: 十 vs 10、大小写英文缩写仍可能误判，靠 --min-similarity 放宽。
fn normalize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '0'..='9' => Some(digit_cn(c)),
            '０'..='９' => char::from_u32('0' as u32 + (c as u32 - '０' as u32)).map(digit_cn),
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .map(|c| match c {
            '两' => '二',
            '〇' => '零',
            other => other,
        })
        .collect()
}

fn edit_distance(left: &[char], right: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, a) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let cost = if a == b { 0 } else { 1 };
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

pub fn similarity(expected: &str, heard: &str) -> f64 {
    let left: Vec<char> = normalize(expected).chars().collect();
    let right: Vec<char> = normalize(heard).chars().collect();
    let longest = left.len().max(right.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - edit_distance(&left, &right) as f64 / longest as f64
}

fn retry_file(root: &Path) -> PathBuf {
    root.join("voice/asr-retry.json")
}

fn load_retry(path: &Path) -> Result<Value> {
    if !path.is_file() {
        return Ok(json!({ "entries": {} }));
    }
    let mut data = read_json(path)?;
    if !data["entries"].is_object() {
        data["entries"] = json!({});
    }
    Ok(data)
}

/// 需要重新合成的句子（`tts --retry` 读它）。
pub fn retry_indexes(root: &Path) -> Result<Vec<usize>> {
    let data = load_retry(&retry_file(root))?;
    let mut indexes: Vec<usize> = data["entries"]
        .as_object()
        .map(|entries| entries.keys().filter_map(|key| key.parse().ok()).collect())
        .unwrap_or_default();
    indexes.sort_unstable();
    Ok(indexes)
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    let tts_path = root.join("speech.tts.md");
    if !tts_path.is_file() {
        bail!("missing file: {}", tts_path.display());
    }
    let lines = text::read_sentences(&tts_path)?;
    if lines.is_empty() {
        bail!("speech.tts.md has no sentence");
    }
    let voice_dir = root.join("voice");
    let model_dir = resolve_model_dir(args.model_dir.clone())?;
    let weights = model_dir.join("model.pt");
    let tokenizer = model_dir.join("Qwen3-0.6B/tokenizer.json");
    for path in [&weights, &tokenizer] {
        if !path.is_file() {
            bail!(
                "invalid asr model directory: {} (missing {}, set --model-dir / FUNASR_MODEL_DIR)",
                model_dir.display(),
                path.display()
            );
        }
    }

    let config = FunASRModelConfig::default()
        .with_model_weights(weights.display().to_string())
        .with_tokenizer_path(tokenizer.display().to_string());
    eprintln!(
        "loading asr model {} ({} sentences to transcribe)",
        model_dir.display(),
        lines.len()
    );
    let mut model = FunAsrNanoGenerateModel::new(config, None, None)
        .map_err(|e| anyhow!("asr load failed: {e}"))?;

    let path = retry_file(&root);
    let mut retry = load_retry(&path)?;
    let mut results: Vec<Value> = Vec::new();
    let mut failed: Vec<usize> = Vec::new();

    for line in &lines {
        let index = line.index;
        let key = index.to_string();
        let wav = voice_dir.join(format!("sent-{index:04}.wav"));
        if !wav.is_file() {
            bail!("missing audio: {} (run tts first)", wav.display());
        }
        eprintln!("[{index}] {}", line.text);
        let audio = fun_ast_nano::load_audio_file(&wav)
            .map_err(|e| anyhow!("cannot read {}: {e}", wav.display()))?;
        let request = TranscriptionRequest::default()
            .with_audio_config(audio)
            .with_prompt(Some(args.prompt.clone()))
            .with_max_tokens(args.max_tokens);
        let response = model
            .generate(request, None, |_| Ok(()))
            .map_err(|e| anyhow!("sentence {index} transcription failed: {e}"))?;
        let heard = response.text.trim().to_string();
        let score = similarity(&line.text, &heard);
        let ok = score >= args.min_similarity;
        eprintln!(
            "    asr: {heard} ({score:.2} {})",
            if ok { "ok" } else { "mismatch" }
        );
        if ok {
            if let Some(entries) = retry["entries"].as_object_mut() {
                entries.remove(&key);
            }
        } else {
            failed.push(index);
            let attempts = retry["entries"][&key]["attempts"].as_u64().unwrap_or(0) + 1;
            let bad = voice_dir.join(format!("bad/sent-{index:04}-{attempts}.wav"));
            if let Some(parent) = bad.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&wav, &bad)
                .with_context(|| format!("cannot keep bad sample {}", bad.display()))?;
            retry["entries"][&key] = json!({
                "tts_text": line.text,
                "asr_text": heard,
                "similarity": score,
                "attempts": attempts,
                "file": bad.strip_prefix(&root).unwrap_or(&bad).display().to_string(),
            });
        }
        results.push(json!({
            "index": index,
            "text": line.text,
            "asr_text": heard,
            "similarity": score,
            "ok": ok,
        }));
    }

    let report = json!({
        "ok": failed.is_empty(),
        "model_dir": model_dir.display().to_string(),
        "min_similarity": args.min_similarity,
        "sentences": results,
        "retry": failed,
    });
    write_json(&voice_dir.join("asr.json"), &report)?;
    write_json(&path, &retry)?;
    print_json(&json!({
        "ok": failed.is_empty(),
        "sentences": lines.len(),
        "mismatched": failed,
        "retry_file": path.display().to_string(),
        "report": voice_dir.join("asr.json").display().to_string(),
    }));
    if !failed.is_empty() {
        bail!(
            "{} sentences do not match the script: {} — run `tts <dir> --retry`, then asr again",
            failed.len(),
            failed
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    Ok(())
}
