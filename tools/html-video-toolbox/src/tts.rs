use crate::text::{self, read_json, write_json};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use voxcpm_rs::{GenerateOptions, Prompt, PromptAudio, VoxCPM};

const SAMPLE_RATE: u32 = 48_000;
const SENTENCE_GAP_MS: u64 = 150;
const PARAGRAPH_GAP_MS: u64 = 400;
/// 页间静默：至少要放得下 compose 叠的翻页音效（0.55s），再给切页留点余量。
const PAGE_GAP_MS: u64 = 800;
const TRIM_FILTER: &str = "silenceremove=start_periods=1:start_silence=0.05:start_threshold=-40dB,\
areverse,silenceremove=start_periods=1:start_silence=0.12:start_threshold=-40dB,areverse";

/// 每句最多再合成几次（共 RATE_TRIES + 1 个 take）。
const RATE_TRIES: usize = 2;
/// 参与语速统计所需的最少语速单位（汉字或英文单词），太短的句子语速天然偏高。
const RATE_MIN_UNITS: f64 = 6.0;

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
    /// re-synthesize only the sentences listed in voice/asr-retry.json
    #[arg(long)]
    pub retry: bool,
    /// synthesize on the wgpu GPU backend instead of the CPU (flex) backend
    #[arg(long)]
    pub gpu: bool,
    /// redo sentences whose speaking rate deviates more than this fraction from
    /// the batch median (0 disables the check)
    #[arg(long, default_value_t = 0.25)]
    pub rate_tolerance: f64,
    /// silence at the head of the track, so the video opens with a beat before speech
    #[arg(long, default_value_t = 400)]
    pub lead_in_ms: u64,
    /// silence at the tail of the track, so the last word is not cut off at the frame edge
    #[arg(long, default_value_t = 600)]
    pub tail_ms: u64,
}

pub(crate) fn parse_only(only: Option<&str>) -> Result<Option<Vec<usize>>> {
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

/// 已加载的克隆模型：一次加载，反复单句合成。
struct Synth {
    model: VoxCPM,
    options: GenerateOptions,
    sample_rate: u32,
}

impl Synth {
    fn load(model_dir: &Path, voice: &Path, args: &Args, count: usize) -> Result<Self> {
        eprintln!(
            "loading model {} on {} ({count} sentences to synthesize)",
            model_dir.display(),
            if args.gpu { "wgpu/gpu" } else { "flex/cpu" },
        );
        let device = if args.gpu {
            burn::tensor::Device::wgpu(burn::tensor::DeviceKind::DefaultDevice)
        } else {
            burn::tensor::Device::flex()
        };
        let model = VoxCPM::from_local(model_dir, &device)
            .map_err(|e| anyhow!("model load failed: {e}"))?;
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
        Ok(Self {
            model,
            options,
            sample_rate,
        })
    }

    fn one(&self, text: &str, index: usize, path: &Path) -> Result<()> {
        let samples = self
            .model
            .generate(text, self.options.clone())
            .map_err(|e| anyhow!("sentence {index} synthesis failed: {e}"))?;
        voxcpm_rs::audio::write_wav(path, &samples, self.sample_rate)
            .map_err(|e| anyhow!("write failed {}: {e}", path.display()))
    }
}

fn synthesize(
    synth: &Synth,
    raw_dir: &Path,
    lines: &[text::Sentence],
    wanted: &[usize],
) -> Result<()> {
    for (done, index) in wanted.iter().enumerate() {
        let sentence = &lines[index - 1].text;
        eprintln!(
            "[{}/{}] sentence {index}: {sentence}",
            done + 1,
            wanted.len()
        );
        synth.one(
            sentence,
            *index,
            &raw_dir.join(format!("sent-{index:04}.wav")),
        )?;
    }
    Ok(())
}

/// 每单位时长（毫秒）；单位太少或读不出来返回 `None`，不参与语速统计。
fn rate_ms(path: &Path, text: &str) -> Option<f64> {
    let units = crate::text::speech_units(text);
    if units < RATE_MIN_UNITS {
        return None;
    }
    let (samples, rate) = read_pcm16(path).ok()?;
    Some(samples.len() as f64 * 1000.0 / rate as f64 / units)
}

/// 采样噪声会给每句带来 ±20% 的语速浮动，慢的那一端听着就是拖腔加音色漂移。
/// 用本次合成句子的语速中位数当基准，把偏离超过 `tolerance` 的句子换噪声
/// 重做，只保留离中位数最近的一次（原文件一直在盘上，更差的一次直接丢掉）。
/// 返回真正重做过的句子编号。
fn redo_rate_outliers(
    synth: &Synth,
    root: &Path,
    lines: &[text::Sentence],
    done: &[usize],
    tolerance: f64,
) -> Result<Vec<usize>> {
    if tolerance <= 0.0 {
        return Ok(Vec::new());
    }
    let voice_dir = root.join("voice");
    let raw_dir = voice_dir.join("raw");
    let mut rates: Vec<(usize, f64)> = Vec::new();
    for line in lines {
        let path = voice_dir.join(format!("sent-{:04}.wav", line.index));
        if let Some(ms) = rate_ms(&path, &line.text) {
            rates.push((line.index, ms));
        }
    }
    if rates.len() < 3 {
        return Ok(Vec::new());
    }
    let mut sorted: Vec<f64> = rates.iter().map(|r| r.1).collect();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];

    let mut retried: Vec<usize> = Vec::new();
    for (index, ms) in rates {
        if !done.contains(&index) {
            continue;
        }
        let mut best = (ms / median - 1.0).abs();
        if best <= tolerance {
            continue;
        }
        let text = &lines[index - 1].text;
        eprintln!("[rate] sentence {index}: {ms:.0}ms/char vs median {median:.0}ms/char, redoing");
        let raw_best = raw_dir.join(format!("sent-{index:04}.wav"));
        let trim_best = voice_dir.join(format!("sent-{index:04}.wav"));
        let raw_tmp = raw_dir.join(format!(".sent-{index:04}.retry.wav"));
        let trim_tmp = voice_dir.join(format!(".sent-{index:04}.retry.wav"));
        for attempt in 1..=RATE_TRIES {
            synth.one(text, index, &raw_tmp)?;
            trim(&raw_tmp, &trim_tmp)?;
            let fresh = rate_ms(&trim_tmp, text).unwrap_or(ms);
            let score = (fresh / median - 1.0).abs();
            if score < best {
                best = score;
                std::fs::rename(&raw_tmp, &raw_best)?;
                std::fs::rename(&trim_tmp, &trim_best)?;
                eprintln!("    take {attempt}: {fresh:.0}ms/char, closer to median, kept");
            } else {
                eprintln!("    take {attempt}: {fresh:.0}ms/char, still off, dropped");
            }
            let _ = std::fs::remove_file(&raw_tmp);
            let _ = std::fs::remove_file(&trim_tmp);
            if best <= tolerance {
                break;
            }
        }
        if best > tolerance {
            eprintln!(
                "    sentence {index} still {:.0}% off the median, kept the closest take",
                best * 100.0
            );
        }
        retried.push(index);
    }
    Ok(retried)
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

pub(crate) fn read_pcm16(path: &Path) -> Result<(Vec<i16>, u32)> {
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

/// 每页第一句的编号（除首页），页间静默按它加长。
fn page_start_indexes(root: &Path) -> Vec<usize> {
    let path = root.join("slides/pages.json");
    if !path.is_file() {
        return Vec::new();
    }
    let Ok(data) = read_json(&path) else {
        return Vec::new();
    };
    data["pages"]
        .as_array()
        .map(|pages| {
            pages
                .iter()
                .skip(1)
                .filter_map(|page| page["from"].as_u64().map(|from| from as usize))
                .collect()
        })
        .unwrap_or_default()
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    if !review_passed(&root)? {
        bail!(
            "review-speech not done: check speech.txt manually first, then run state done <dir> review-speech"
        );
    }
    let speech_path = root.join("speech.txt");
    let tts_path = root.join("speech.tts.txt");
    for path in [&speech_path, &tts_path] {
        if !path.is_file() {
            bail!("missing file: {}", path.display());
        }
    }
    let source = text::read_sentences(&speech_path)?;
    let lines = text::read_sentences(&tts_path)?;
    if source.len() != lines.len() {
        bail!(
            "speech.txt and speech.tts.txt line counts differ ({} vs {})",
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
    let retry = if args.retry {
        let indexes = crate::asr::retry_indexes(&root)?;
        if indexes.is_empty() {
            bail!("no sentence queued in voice/asr-retry.json, run `asr <dir>` first");
        }
        Some(indexes)
    } else {
        None
    };

    let mut synth: Vec<usize> = Vec::new();
    let mut trim_needs: Vec<usize> = Vec::new();
    for (src, line) in source.iter().zip(&lines) {
        let index = src.index;
        let raw = raw_dir.join(format!("sent-{index:04}.wav"));
        let trimmed = voice_dir.join(format!("sent-{index:04}.wav"));
        let unchanged = manifest["sentences"][index.to_string()]["tts_text"].as_str()
            == Some(line.text.as_str());
        let selected = only.as_ref().is_none_or(|set| set.contains(&index))
            && retry.as_ref().is_none_or(|set| set.contains(&index));
        let need = (args.force || retry.is_some() || !unchanged || !raw.exists()) && selected;
        if need {
            synth.push(index);
        }
        if need || !trimmed.exists() {
            trim_needs.push(index);
        }
    }

    let mut redo: Vec<usize> = Vec::new();
    if !synth.is_empty() {
        let model = Synth::load(&model_dir, &args.voice, &args, synth.len())?;
        synthesize(&model, &raw_dir, &lines, &synth)?;
        for index in &trim_needs {
            let raw = raw_dir.join(format!("sent-{index:04}.wav"));
            if !raw.exists() {
                bail!("missing synthesis output: {}", raw.display());
            }
            trim(&raw, &voice_dir.join(format!("sent-{index:04}.wav")))?;
        }
        redo = redo_rate_outliers(&model, &root, &lines, &synth, args.rate_tolerance)?;
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
    let lead_frames = SAMPLE_RATE as u64 * args.lead_in_ms / 1000;
    for _ in 0..lead_frames {
        writer.write_sample(0i16)?;
    }
    let mut cursor: u64 = lead_frames;
    let page_starts = page_start_indexes(&root);
    let ms = |frames: u64| (frames as f64 * 1000.0 / SAMPLE_RATE as f64).round() as u64;

    for (pos, (src, line)) in source.iter().zip(&lines).enumerate() {
        let index = src.index;
        let trimmed = voice_dir.join(format!("sent-{index:04}.wav"));
        let (samples, rate) = read_pcm16(&trimmed)?;
        if rate != SAMPLE_RATE {
            bail!(
                "trim output sample rate mismatch: {} ({rate})",
                trimmed.display()
            );
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
        } else if page_starts.contains(&source[pos + 1].index) {
            PAGE_GAP_MS
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
    let tail_frames = SAMPLE_RATE as u64 * args.tail_ms / 1000;
    for _ in 0..tail_frames {
        writer.write_sample(0i16)?;
    }
    cursor += tail_frames;
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
            text::strip_tail_punct(entry["text"].as_str().unwrap_or(""))
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
        "rate_retried": redo,
        "full": full_path.display().to_string(),
        "lead_in_ms": args.lead_in_ms,
        "tail_ms": args.tail_ms,
        "duration_ms": total_ms,
        "srt": srt_path.display().to_string(),
    }));
    Ok(())
}
