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
    /// transcribe only these sentence indexes, comma separated
    #[arg(long)]
    pub only: Option<String>,
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

/// 数字读法：Numeric 按数值（85→八十五），Digits 逐位（270M→二七零M）。
#[derive(Clone, Copy, PartialEq)]
enum NumStyle {
    Numeric,
    Digits,
}

fn cn_digits(int_part: &str) -> String {
    int_part.chars().map(digit_cn).collect()
}

/// 一组四位以内数字，前导零已去掉。
fn cn_group(group: &str) -> String {
    const UNITS: [&str; 4] = ["", "十", "百", "千"];
    let digits: Vec<u32> = group.chars().map(|c| c as u32 - '0' as u32).collect();
    let mut out = String::new();
    let mut zero = false;
    for (i, d) in digits.iter().enumerate() {
        let pos = digits.len() - 1 - i;
        if *d == 0 {
            zero = true;
            continue;
        }
        if zero && !out.is_empty() {
            out.push('零');
        }
        zero = false;
        out.push(digit_cn(char::from_digit(*d, 10).unwrap_or('0')));
        out.push_str(UNITS[pos]);
    }
    out
}

/// 数值读法：85 → 八十五、32000 → 三万二千、13 → 十三。超过 12 位退回逐位。
fn cn_numeric(int_part: &str) -> String {
    let trimmed = int_part.trim_start_matches('0');
    if trimmed.is_empty() {
        return "零".to_string();
    }
    if int_part.starts_with('0') || trimmed.len() > 12 {
        return cn_digits(int_part);
    }
    const GROUPS: [&str; 4] = ["", "万", "亿", "万亿"];
    let mut chunks: Vec<String> = trimmed
        .as_bytes()
        .rchunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect();
    chunks.reverse();
    let total = chunks.len();
    let mut out = String::new();
    let mut pending_zero = false;
    for (i, chunk) in chunks.iter().enumerate() {
        let digits = chunk.trim_start_matches('0');
        if digits.is_empty() {
            if !out.is_empty() {
                pending_zero = true;
            }
            continue;
        }
        if !out.is_empty() && (pending_zero || digits.len() < 4) {
            out.push('零');
        }
        pending_zero = false;
        out.push_str(&cn_group(digits));
        out.push_str(GROUPS[total - 1 - i]);
    }
    if out.starts_with("一十") {
        out.remove(0);
    }
    out
}

fn cn_number(integer: &str, fraction: &str, style: NumStyle) -> String {
    let head = match style {
        NumStyle::Numeric => cn_numeric(integer),
        NumStyle::Digits => cn_digits(integer),
    };
    if fraction.is_empty() {
        head
    } else {
        format!("{head}点{}", cn_digits(fraction))
    }
}

/// 只留字母/数字/汉字：阿拉伯数字转中文读法（`%` → 百分之，小数点 → 点），
/// 避免「8.5%」和「百分之八点五」判成不一致。两种读法各算一次相似度，取较高者。
fn normalize(text: &str, style: NumStyle) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let digit = match c {
            '0'..='9' => Some(c),
            '０'..='９' => char::from_u32('0' as u32 + (c as u32 - '０' as u32)),
            _ => None,
        };
        if let Some(first) = digit {
            let mut integer = String::from(first);
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    d @ '0'..='9' => {
                        integer.push(d);
                        i += 1;
                    }
                    d @ '０'..='９' => {
                        integer
                            .push(char::from_u32('0' as u32 + (d as u32 - '０' as u32)).unwrap());
                        i += 1;
                    }
                    ',' | '，'
                        if i + 3 < chars.len()
                            && chars[i + 1].is_ascii_digit()
                            && chars[i + 2].is_ascii_digit()
                            && chars[i + 3].is_ascii_digit()
                            && !integer.ends_with(',') =>
                    {
                        i += 1;
                    }
                    _ => break,
                }
            }
            let mut fraction = String::new();
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    fraction.push(chars[i]);
                    i += 1;
                }
            }
            let percent = i < chars.len() && chars[i] == '%';
            if percent {
                i += 1;
            }
            let number = cn_number(&integer, &fraction, style);
            if percent {
                out.push_str("百分之");
            }
            out.push_str(&number);
            continue;
        }
        if c == '%' {
            out.push_str("百分之");
        } else if c.is_alphanumeric() {
            out.push(match c.to_ascii_lowercase() {
                '两' => '二',
                '〇' => '零',
                '他' | '她' => '它',
                other => other,
            });
        }
        i += 1;
    }
    out
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

fn similarity_once(expected: &str, heard: &str, style: NumStyle) -> f64 {
    let left: Vec<char> = normalize(expected, style).chars().collect();
    let right: Vec<char> = normalize(heard, style).chars().collect();
    let longest = left.len().max(right.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - edit_distance(&left, &right) as f64 / longest as f64
}

/// 数量读法与逐位读法各算一次，取较高者：`85` 念八十五、`270M` 念二七零M 都能对上。
pub fn similarity(expected: &str, heard: &str) -> f64 {
    similarity_once(expected, heard, NumStyle::Numeric).max(similarity_once(
        expected,
        heard,
        NumStyle::Digits,
    ))
}

/// `--only` 只写入本次转录的几句，其余句子上次的记录保留。
fn merge_results(mut base: Vec<Value>, fresh: Vec<Value>) -> Vec<Value> {
    base.retain(|old| !fresh.iter().any(|new| new["index"] == old["index"]));
    base.extend(fresh);
    base.sort_by_key(|item| item["index"].as_u64().unwrap_or(0));
    base
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
    let only = crate::tts::parse_only(args.only.as_deref())?;
    if let Some(set) = &only {
        for index in set {
            if !lines.iter().any(|line| line.index == *index) {
                bail!("no sentence {index} in speech.tts.md");
            }
        }
    }
    let selected: Vec<&text::Sentence> = lines
        .iter()
        .filter(|line| only.as_ref().is_none_or(|set| set.contains(&line.index)))
        .collect();
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
        selected.len()
    );
    let mut model = FunAsrNanoGenerateModel::new(config, None, None)
        .map_err(|e| anyhow!("asr load failed: {e}"))?;

    let path = retry_file(&root);
    let mut retry = load_retry(&path)?;
    let mut results: Vec<Value> = Vec::new();

    for line in &selected {
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

    let failed = retry_indexes(&root)?;
    let report_path = voice_dir.join("asr.json");
    let base = if only.is_some() && report_path.is_file() {
        read_json(&report_path)?["sentences"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let report = json!({
        "ok": failed.is_empty(),
        "model_dir": model_dir.display().to_string(),
        "min_similarity": args.min_similarity,
        "sentences": merge_results(base, results),
        "retry": failed,
    });
    write_json(&report_path, &report)?;
    write_json(&path, &retry)?;
    print_json(&json!({
        "ok": failed.is_empty(),
        "sentences": lines.len(),
        "transcribed": selected.len(),
        "mismatched": failed,
        "retry_file": path.display().to_string(),
        "report": report_path.display().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(expected: &str, heard: &str) -> f64 {
        similarity(expected, heard)
    }

    #[test]
    fn only_run_keeps_previous_sentences() {
        let base = vec![json!({ "index": 1, "ok": false }), json!({ "index": 2, "ok": true })];
        let fresh = vec![json!({ "index": 2, "ok": false })];
        let merged = merge_results(base, fresh);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["ok"], json!(false));
        assert_eq!(merged[1]["ok"], json!(false));
    }

    #[test]
    fn numbers_match_across_readings() {
        assert!(sim("八十五道耗尽三万二千个 token", "85道耗尽32000个TOKEN") > 0.9);
        assert!(sim("百分之八点五", "8.5%") > 0.9);
        assert!(sim("百分之零点零一六", "0.016%") > 0.9);
        assert!(sim("一千零二十四局", "1024局") > 0.9);
        assert!(sim("Gemma 三 二七零 M", "Gemma 3 270M") > 0.9);
        assert!(sim("十三点八", "13.8") > 0.9);
        assert!(sim("八亿参数", "800000000 参数") > 0.9);
        assert!(sim("一万七千个权重", "17,000个权重") > 0.95);
        assert!(sim("它找得到差异", "他找得到差异") > 0.99);
        assert!(sim("四猜四中", "试拆示众") < 0.5);
    }
}
