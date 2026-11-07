use crate::text::{self, read_json, write_json};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use voxcpm_rs::{GenerateOptions, Prompt, PromptAudio, VoxCPM};

const SAMPLE_RATE: u32 = 48_000;
const SENTENCE_GAP_MS: u64 = 150;
const PARAGRAPH_GAP_MS: u64 = 400;
const TRIM_FILTER: &str = "silenceremove=start_periods=1:start_silence=0.05:start_threshold=-40dB,\
areverse,silenceremove=start_periods=1:start_silence=0.12:start_threshold=-40dB,areverse";

#[derive(clap::Args)]
pub struct Args {
    /// article directory
    pub dir: PathBuf,
    /// voice clone reference audio
    #[arg(long, env = "VOICE_CLONE_WAV")]
    pub voice: PathBuf,
    /// VoxCPM2 model directory (with config.json)
    #[arg(long, env = "VOXCPM_MODEL_DIR")]
    pub model_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 2.0)]
    pub cfg: f32,
    #[arg(long, default_value_t = 10)]
    pub steps: usize,
    #[arg(long, default_value_t = 2000)]
    pub max_len: usize,
    /// re-synthesize everything
    #[arg(long)]
    pub force: bool,
    /// process only these sentence indexes, comma separated
    #[arg(long)]
    pub only: Option<String>,
}

fn parse_only(only: Option<&str>) -> Result<Option<Vec<usize>>> {
    let Some(only) = only else { return Ok(None) };
    let indexes: Vec<usize> = only
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<usize>()
                .map_err(|_| anyhow!("not a number in --only: {part}"))
        })
        .collect::<Result<_>>()?;
    if indexes.is_empty() {
        bail!("--only has no valid index");
    }
    Ok(Some(indexes))
}

fn resolve_model_dir(cli: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = cli {
        return Ok(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    Ok(PathBuf::from(home).join("models/VoxCPM2"))
}

fn review_passed(root: &Path) -> Result<bool> {
    let progress = root.join("progress.json");
    if !progress.exists() {
        return Ok(true);
    }
    let data = read_json(&progress)?;
    Ok(data["steps"]["review-speech"]["status"].as_str() == Some("done"))
}

fn synthesize(
    model_dir: &Path,
    voice: &Path,
    args: &Args,
    raw_dir: &Path,
    lines: &[text::Sentence],
    wanted: &[usize],
) -> Result<()> {
    eprintln!(
        "loading model {} ({} sentences to synthesize)",
        model_dir.display(),
        wanted.len()
    );
    let device = burn::tensor::Device::flex();
    let model = VoxCPM::from_local(model_dir, &device).map_err(|e| anyhow!("model load failed: {e}"))?;
    let sample_rate = model.sample_rate() as u32;
    let options = GenerateOptions {
        cfg_value: args.cfg,
        inference_timesteps: args.steps,
        max_len: args.max_len,
        prompt: Prompt::Reference {
            audio: PromptAudio::File(voice.to_path_buf()),
        },
        ..Default::default()
    };
    for (done, index) in wanted.iter().enumerate() {
        let sentence = &lines[index - 1].text;
        eprintln!("[{}/{}] sentence {index}: {sentence}", done + 1, wanted.len());
        let samples = model
            .generate(sentence, options.clone())
            .map_err(|e| anyhow!("sentence {index} synthesis failed: {e}"))?;
        let path = raw_dir.join(format!("sent-{index:04}.wav"));
        voxcpm_rs::audio::write_wav(&path, &samples, sample_rate)
            .map_err(|e| anyhow!("write failed {}: {e}", path.display()))?;
    }
    Ok(())
}

fn trim(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args(["-af", TRIM_FILTER, "-ar"])
        .arg(SAMPLE_RATE.to_string())
        .args(["-ac", "1", "-c:a", "pcm_s16le"])
        .arg(target)
        .output()
        .context("failed to run ffmpeg")?;
    if !output.status.success() {
        bail!(
            "trim failed {}: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn read_pcm16(path: &Path) -> Result<(Vec<i16>, u32)> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        bail!("trim output is not pcm_s16le mono: {}", path.display());
    }
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<std::result::Result<_, _>>()?;
    Ok((samples, spec.sample_rate))
}

fn timestamp(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    if !review_passed(&root)? {
        bail!("review-speech not done: check speech.md manually first, then run state done <dir> review-speech");
    }
    let speech_path = root.join("speech.md");
    let tts_path = root.join("speech.tts.md");
    for path in [&speech_path, &tts_path] {
        if !path.is_file() {
            bail!("missing file: {}", path.display());
        }
    }
    let source = text::read_sentences(&speech_path)?;
    let lines = text::read_sentences(&tts_path)?;
    if source.len() != lines.len() {
        bail!(
            "speech.md and speech.tts.md line counts differ ({} vs {})",
            source.len(),
            lines.len()
        );
    }
    if !args.voice.is_file() {
        bail!(
            "missing voice clone audio: {} (set --voice / VOICE_CLONE_WAV)",
            args.voice.display()
        );
    }
    let model_dir = resolve_model_dir(args.model_dir.clone())?;
    if !model_dir.join("config.json").is_file() {
        bail!(
            "invalid model directory: {} (set --model-dir / VOXCPM_MODEL_DIR)",
            model_dir.display()
        );
    }

    let raw_dir = root.join("voice/raw");
    let voice_dir = root.join("voice");
    std::fs::create_dir_all(&raw_dir)?;
    let manifest_path = voice_dir.join("manifest.json");
    let mut manifest = if manifest_path.exists() {
        read_json(&manifest_path)?
    } else {
        json!({ "sentences": {} })
    };
    if !manifest["sentences"].is_object() {
        manifest["sentences"] = json!({});
    }
    let only = parse_only(args.only.as_deref())?;

    let mut synth: Vec<usize> = Vec::new();
    let mut trim_needs: Vec<usize> = Vec::new();
    for (src, line) in source.iter().zip(&lines) {
        let index = src.index;
        let raw = raw_dir.join(format!("sent-{index:04}.wav"));
        let trimmed = voice_dir.join(format!("sent-{index:04}.wav"));
        let unchanged = manifest["sentences"][index.to_string()]["tts_text"].as_str()
            == Some(line.text.as_str());
        let selected = only.as_ref().is_none_or(|set| set.contains(&index));
        let need = (args.force || !unchanged || !raw.exists()) && selected;
        if need {
            synth.push(index);
        }
        if need || !trimmed.exists() {
            trim_needs.push(index);
        }
    }

    if !synth.is_empty() {
        synthesize(&model_dir, &args.voice, &args, &raw_dir, &lines, &synth)?;
    }
    for index in &trim_needs {
        let raw = raw_dir.join(format!("sent-{index:04}.wav"));
        if !raw.exists() {
            bail!("missing synthesis output: {}", raw.display());
        }
        trim(&raw, &voice_dir.join(format!("sent-{index:04}.wav")))?;
    }

    let full_path = voice_dir.join("full.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&full_path, spec)
        .with_context(|| format!("cannot write {}", full_path.display()))?;
    let mut entries: Vec<Value> = Vec::new();
    let mut cursor: u64 = 0;
    let ms = |frames: u64| (frames as f64 * 1000.0 / SAMPLE_RATE as f64).round() as u64;

    for (pos, (src, line)) in source.iter().zip(&lines).enumerate() {
        let index = src.index;
        let trimmed = voice_dir.join(format!("sent-{index:04}.wav"));
        let (samples, rate) = read_pcm16(&trimmed)?;
        if rate != SAMPLE_RATE {
            bail!("trim output sample rate mismatch: {} ({rate})", trimmed.display());
        }
        for sample in &samples {
            writer.write_sample(*sample)?;
        }
        let start_ms = ms(cursor);
        cursor += samples.len() as u64;
        let end_ms = ms(cursor);
        let last = pos + 1 == source.len();
        let gap_ms = if last {
            0
        } else if source[pos + 1].paragraph != src.paragraph {
            PARAGRAPH_GAP_MS
        } else {
            SENTENCE_GAP_MS
        };
        if gap_ms > 0 {
            let silence = SAMPLE_RATE as u64 * gap_ms / 1000;
            for _ in 0..silence {
                writer.write_sample(0i16)?;
            }
            cursor += silence;
        }
        entries.push(json!({
            "index": index,
            "paragraph": src.paragraph,
            "text": src.text,
            "tts_text": line.text,
            "start_ms": start_ms,
            "end_ms": end_ms,
            "duration_ms": end_ms - start_ms,
            "gap_ms": gap_ms,
        }));
        manifest["sentences"][index.to_string()] = json!({
            "source_text": src.text,
            "tts_text": line.text,
            "raw": format!("voice/raw/sent-{index:04}.wav"),
            "file": format!("voice/sent-{index:04}.wav"),
            "duration_ms": end_ms - start_ms,
        });
    }
    writer.finalize().context("failed to finalize full.wav")?;

    let total_ms = ms(cursor);
    write_json(
        &voice_dir.join("timeline.json"),
        &json!({ "sample_rate": SAMPLE_RATE, "sentences": entries, "total_ms": total_ms }),
    )?;
    manifest["voice"] = json!(args.voice.display().to_string());
    manifest["model_dir"] = json!(model_dir.display().to_string());
    write_json(&manifest_path, &manifest)?;

    let mut srt = String::new();
    for entry in &entries {
        srt.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            entry["index"],
            timestamp(entry["start_ms"].as_u64().unwrap_or(0)),
            timestamp(entry["end_ms"].as_u64().unwrap_or(0)),
            entry["text"].as_str().unwrap_or("")
        ));
    }
    let srt_path = root.join("subs/final.srt");
    if let Some(parent) = srt_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&srt_path, srt)?;

    text::print_json(&json!({
        "ok": true,
        "sentences": entries.len(),
        "synthesized": synth,
        "trimmed": trim_needs,
        "full": full_path.display().to_string(),
        "duration_ms": total_ms,
        "srt": srt_path.display().to_string(),
    }));
    Ok(())
}
