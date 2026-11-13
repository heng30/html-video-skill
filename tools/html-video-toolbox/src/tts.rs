use crate::quality::{self, Baseline, Metrics, Tolerances};
use crate::text::{self, print_json, read_json, write_json};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use voxcpm_rs::{GenerateOptions, Prompt, PromptAudio, VoxCPM};

const SAMPLE_RATE: u32 = 48_000;
const SENTENCE_GAP_MS: u64 = 250;
const PARAGRAPH_GAP_MS: u64 = 400;
/// 页间静默：至少要放得下 compose 叠的翻页音效（0.55s），再给切页留点余量。
const PAGE_GAP_MS: u64 = 800;
const TRIM_FILTER: &str = "silenceremove=start_periods=1:start_silence=0.05:start_threshold=-40dB,\
areverse,silenceremove=start_periods=1:start_silence=0.12:start_threshold=-40dB,areverse";
/// 参考音频最多补/降多少 dB，避免把噪声底噪拉上来。
const REF_MAX_GAIN_DB: f32 = 12.0;

/// 每句最多再合成几次（共 TAKE_TRIES + 1 个 take）。
const TAKE_TRIES: usize = 2;
/// 参与语速统计所需的最少语速单位（汉字或英文单词）：太短的句子语速天然偏高，
/// 拿它们当中位数基准或判离群都会误报。
const RATE_MIN_UNITS: f64 = 10.0;

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
    /// redo sentences whose median pitch deviates more than this fraction from
    /// the reference audio (0 disables the check)
    #[arg(long, default_value_t = 0.15)]
    pub pitch_tolerance: f64,
    /// override the auto-calibrated timbre similarity floor (0 = use the
    /// distribution measured on the reference audio)
    #[arg(long, default_value_t = 0.0)]
    pub min_timbre: f64,
    /// silence at the head of the track, so the video opens with a beat before speech
    #[arg(long, default_value_t = 400)]
    pub lead_in_ms: u64,
    /// silence at the tail of the track, so the last word is not cut off at the frame edge
    #[arg(long, default_value_t = 600)]
    pub tail_ms: u64,
}

#[derive(clap::Args)]
pub struct AuditArgs {
    /// article directory
    pub dir: PathBuf,
    /// voice clone reference audio, rebuilds voice/baseline.json
    #[arg(long, env = "VOICE_CLONE_WAV")]
    pub voice: Option<PathBuf>,
    #[arg(long, default_value_t = 0.25)]
    pub rate_tolerance: f64,
    #[arg(long, default_value_t = 0.15)]
    pub pitch_tolerance: f64,
    #[arg(long, default_value_t = 0.0)]
    pub min_timbre: f64,
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
    fn load(model_dir: &Path, reference: &Path, args: &Args, count: usize) -> Result<Self> {
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
        let sample_rate = model.sample_rate();
        let options = GenerateOptions {
            cfg_value: args.cfg,
            inference_timesteps: args.steps,
            max_len: args.max_len,
            prompt: Prompt::Reference {
                audio: PromptAudio::File(reference.to_path_buf()),
            },
            ..Default::default()
        };
        Ok(Self {
            model,
            options,
            sample_rate,
        })
    }

    /// 同一个 `seed` + 同一句 → 同一段音频，重做可复现、可事后比对。
    fn one(&self, text: &str, index: usize, path: &Path, seed: u64) -> Result<()> {
        voxcpm_rs::set_seed(Some(seed));
        let samples = self
            .model
            .generate(text, self.options.clone())
            .map_err(|e| anyhow!("sentence {index} synthesis failed: {e}"))?;
        voxcpm_rs::audio::write_wav(path, &samples, self.sample_rate)
            .map_err(|e| anyhow!("write failed {}: {e}", path.display()))
    }
}

fn take_seed(index: usize, attempt: usize) -> u64 {
    0x5eed_0000 ^ ((index as u64) << 8) ^ attempt as u64
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
            take_seed(*index, 0),
        )?;
    }
    Ok(())
}

fn rewrite(path: &Path, samples: &[i16], rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("cannot write {}", path.display()))?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn apply_gain(samples: &[i16], gain_db: f32) -> Vec<i16> {
    let gain = 10f32.powf(gain_db / 20.0);
    samples
        .iter()
        .map(|sample| (*sample as f32 * gain).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// 读盘上这一句并体检；文件不存在或整段没声返回 `None`。
fn measure(path: &Path) -> Result<Option<Metrics>> {
    if !path.is_file() {
        return Ok(None);
    }
    let (samples, rate) = read_pcm16(path)?;
    let audio = quality::mono_at(&quality::to_f32(&samples), 1, rate);
    Ok(quality::analyze(&audio, quality::ANA_SR))
}

/// 把这一句的响度拉到目标、增益钳在 ±[`quality::MAX_GAIN_DB`]，返回补完增益后的体检结果。
fn level_fix(path: &Path) -> Result<Option<Metrics>> {
    let (samples, rate) = read_pcm16(path)?;
    let audio = quality::mono_at(&quality::to_f32(&samples), 1, rate);
    let Some(mut metrics) = quality::analyze(&audio, quality::ANA_SR) else {
        return Ok(None);
    };
    let Some(needed) = quality::needed_gain(&metrics) else {
        return Ok(Some(metrics));
    };
    let gain = needed.clamp(-quality::MAX_GAIN_DB, quality::MAX_GAIN_DB);
    if gain.abs() >= 0.1 {
        rewrite(path, &apply_gain(&samples, gain), rate)?;
        metrics.loudness_db += gain as f64;
        metrics.peak_db += gain as f64;
        eprintln!("    level: {gain:+.1}dB");
    }
    Ok(Some(metrics))
}

/// 参考音频标准化：裁首尾静音 + 响度对齐，写到 `voice/ref.wav`。
/// 直接吃原始克隆音频，它的静音结构和电平会进条件里，音色漂移跟着变大。
/// 每次开跑都重做：换音色（`--voice`）时不用记得先手工处理一遍。
fn prepare_reference(root: &Path, voice: &Path) -> Result<PathBuf> {
    let target = root.join("voice/ref.wav");
    let tmp = root.join("voice/.ref.trim.wav");
    trim(voice, &tmp)?;
    let (samples, rate) = read_pcm16(&tmp)?;
    let audio = quality::mono_at(&quality::to_f32(&samples), 1, rate);
    let gain = quality::analyze(&audio, quality::ANA_SR)
        .and_then(|metrics| quality::needed_gain(&metrics))
        .map(|gain| gain.clamp(-REF_MAX_GAIN_DB, REF_MAX_GAIN_DB))
        .unwrap_or(0.0);
    rewrite(&target, &apply_gain(&samples, gain), rate)?;
    let _ = std::fs::remove_file(&tmp);
    eprintln!(
        "reference {} → {} ({gain:+.1}dB)",
        voice.display(),
        target.display()
    );
    Ok(target)
}

fn tolerances(rate: f64, pitch: f64) -> Tolerances {
    Tolerances { rate, pitch }
}

/// 建锚：有 `--voice` 就从参考音频重建（便宜，且锚必须跟着参考走），否则读盘上的。
fn resolve_baseline(
    root: &Path,
    voice: Option<&Path>,
    cfg: f32,
    steps: usize,
    min_timbre: f64,
) -> Result<Baseline> {
    let mut baseline = match voice {
        Some(voice) => {
            let reference = prepare_reference(root, voice)?;
            let (samples, rate) = read_pcm16(&reference)?;
            let audio = quality::mono_at(&quality::to_f32(&samples), 1, rate);
            Baseline::from_reference(&audio, &reference)?
        }
        None => Baseline::load(root).ok_or_else(|| {
            anyhow!(
                "missing {} — run tts first or pass --voice",
                quality::path_of(root).display()
            )
        })?,
    };
    if min_timbre > 0.0 {
        baseline.timbre_floor = min_timbre;
        baseline.timbre_floor_source = "cli --min-timbre".to_string();
    }
    if cfg > 0.0 {
        baseline.cfg = cfg;
        baseline.steps = steps;
    }
    Ok(baseline)
}

/// 逐句体检 + 打分，写 `voice/quality.json`；返回离群句编号。不改音频。
fn measure_all(
    root: &Path,
    lines: &[text::Sentence],
    baseline: &mut Baseline,
    tol: &Tolerances,
) -> Result<(Vec<Value>, Vec<usize>)> {
    let voice_dir = root.join("voice");
    let mut measured: Vec<(usize, String, Metrics)> = Vec::new();
    for line in lines {
        let path = voice_dir.join(format!("sent-{:04}.wav", line.index));
        if let Some(metrics) = measure(&path)? {
            measured.push((line.index, line.text.clone(), metrics));
        }
    }

    let mut rates: Vec<f64> = measured
        .iter()
        .filter_map(|(_, text, metrics)| {
            let units = text::speech_units(text);
            (units >= RATE_MIN_UNITS)
                .then(|| quality::rate_ms(metrics, units))
                .flatten()
        })
        .collect();
    if rates.len() >= 3 {
        rates.sort_by(f64::total_cmp);
        baseline.rate_ms = rates[rates.len() / 2];
    }
    baseline.sentences = measured.len();

    let mut entries: Vec<Value> = Vec::with_capacity(measured.len());
    let mut outliers: Vec<usize> = Vec::new();
    for (index, text, metrics) in &measured {
        let dev = quality::deviations(metrics, text::speech_units(text), baseline, tol);
        if dev.score > 1.0 {
            outliers.push(*index);
        }
        entries.push(quality::report_entry(*index, text, metrics, &dev, baseline));
    }
    write_json(
        &root.join("voice/quality.json"),
        &json!({
            "ok": outliers.is_empty(),
            "baseline": {
                "ref_path": baseline.ref_path,
                "f0_hz": baseline.f0_hz.round(),
                "timbre_floor": baseline.timbre_floor,
                "timbre_floor_source": baseline.timbre_floor_source,
                "rate_ms": baseline.rate_ms,
                "cfg": baseline.cfg,
                "steps": baseline.steps,
            },
            "tolerances": { "rate": tol.rate, "pitch": tol.pitch },
            "sentences": entries,
            "outliers": outliers,
        }),
    )?;
    Ok((entries, outliers))
}

/// 离群的句子换种子重做，只留偏差最小的一次（原文件一直在盘上，更差的直接丢）。
/// 返回真正重做过的句子编号。
fn redo_outliers(
    synth: &Synth,
    root: &Path,
    lines: &[text::Sentence],
    candidates: &[usize],
    baseline: &Baseline,
    tol: &Tolerances,
) -> Result<Vec<usize>> {
    let voice_dir = root.join("voice");
    let raw_dir = voice_dir.join("raw");
    let mut retried: Vec<usize> = Vec::new();
    for index in candidates {
        let text = &lines[index - 1].text;
        let units = text::speech_units(text);
        let trimmed = voice_dir.join(format!("sent-{index:04}.wav"));
        let best = match measure(&trimmed)? {
            Some(metrics) => quality::deviations(&metrics, units, baseline, tol),
            None => quality::Deviation {
                score: f64::INFINITY,
                why: "unreadable",
            },
        };
        if best.score <= 1.0 {
            continue;
        }
        eprintln!(
            "[quality] sentence {index}: {} {:.2}x the tolerance, redoing",
            best.why, best.score
        );
        let mut best = best;
        let raw_best = raw_dir.join(format!("sent-{index:04}.wav"));
        let raw_tmp = raw_dir.join(format!(".sent-{index:04}.retry.wav"));
        let trim_tmp = voice_dir.join(format!(".sent-{index:04}.retry.wav"));
        for attempt in 1..=TAKE_TRIES {
            synth.one(text, *index, &raw_tmp, take_seed(*index, attempt))?;
            trim(&raw_tmp, &trim_tmp)?;
            let fresh = match level_fix(&trim_tmp)? {
                Some(metrics) => quality::deviations(&metrics, units, baseline, tol),
                None => quality::Deviation {
                    score: f64::INFINITY,
                    why: "unreadable",
                },
            };
            if fresh.score < best.score {
                eprintln!(
                    "    take {attempt}: {} {:.2} (was {:.2}), kept",
                    fresh.why, fresh.score, best.score
                );
                best = fresh;
                std::fs::rename(&raw_tmp, &raw_best)?;
                std::fs::rename(&trim_tmp, &trimmed)?;
            } else {
                eprintln!(
                    "    take {attempt}: {} {:.2} (was {:.2}), dropped",
                    fresh.why, fresh.score, best.score
                );
            }
            let _ = std::fs::remove_file(&raw_tmp);
            let _ = std::fs::remove_file(&trim_tmp);
            if best.score <= 1.0 {
                break;
            }
        }
        if best.score > 1.0 {
            eprintln!(
                "    sentence {index} still {:.2}x the tolerance on {}, kept the closest take",
                best.score, best.why
            );
        }
        retried.push(*index);
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

pub fn audit(args: AuditArgs) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    let lines = text::read_sentences(&root.join("speech.tts.txt"))?;
    let mut baseline = resolve_baseline(
        &root,
        args.voice.as_deref(),
        0.0,
        0,
        args.min_timbre,
    )?;
    let tol = tolerances(args.rate_tolerance, args.pitch_tolerance);
    let (entries, outliers) = measure_all(&root, &lines, &mut baseline, &tol)?;
    baseline.save(&root)?;
    print_json(&json!({
        "ok": outliers.is_empty(),
        "sentences": entries.len(),
        "outliers": outliers,
        "baseline": {
            "ref_path": baseline.ref_path,
            "f0_hz": baseline.f0_hz.round(),
            "timbre_floor": baseline.timbre_floor,
            "timbre_floor_source": baseline.timbre_floor_source,
            "rate_ms": baseline.rate_ms.round(),
        },
        "report": root.join("voice/quality.json").display().to_string(),
    }));
    if !outliers.is_empty() {
        bail!(
            "{} sentences are off the reference: {} — check voice/quality.json, then redo with `tts --only {} --force`",
            outliers.len(),
            outliers
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(","),
            outliers
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    Ok(())
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

    let mut baseline = resolve_baseline(
        &root,
        Some(&args.voice),
        args.cfg,
        args.steps,
        args.min_timbre,
    )?;
    let tol = tolerances(args.rate_tolerance, args.pitch_tolerance);

    let mut redo: Vec<usize> = Vec::new();
    if !synth.is_empty() {
        let model = Synth::load(&model_dir, Path::new(&baseline.ref_path), &args, synth.len())?;
        synthesize(&model, &raw_dir, &lines, &synth)?;
        for index in &trim_needs {
            let raw = raw_dir.join(format!("sent-{index:04}.wav"));
            if !raw.exists() {
                bail!("missing synthesis output: {}", raw.display());
            }
            let trimmed = voice_dir.join(format!("sent-{index:04}.wav"));
            trim(&raw, &trimmed)?;
            level_fix(&trimmed)?;
        }
        let (_, outliers) = measure_all(&root, &lines, &mut baseline, &tol)?;
        baseline.save(&root)?;
        // 过半句子都判离群，说明锚本身不对（音色下限、语速基准或参考音频有问题），
        // 这时候全量重做只会白烧几小时，直接停下来让人看报告。
        if outliers.len() * 2 > lines.len() {
            bail!(
                "{} of {} sentences are off the anchor — check voice/quality.json before redoing; \
                 tune --min-timbre / --pitch-tolerance / --rate-tolerance or replace the reference audio",
                outliers.len(),
                lines.len()
            );
        }
        let candidates: Vec<usize> = outliers
            .iter()
            .copied()
            .filter(|index| synth.contains(index))
            .collect();
        redo = redo_outliers(&model, &root, &lines, &candidates, &baseline, &tol)?;
    }
    let (entries, outliers) = measure_all(&root, &lines, &mut baseline, &tol)?;
    baseline.save(&root)?;

    let full_path = voice_dir.join("full.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&full_path, spec)
        .with_context(|| format!("cannot write {}", full_path.display()))?;
    let mut timeline: Vec<Value> = Vec::new();
    let lead_frames = SAMPLE_RATE as u64 * args.lead_in_ms / 1000;
    for _ in 0..lead_frames {
        writer.write_sample(0i16)?;
    }
    let mut cursor: u64 = lead_frames;
    let page_starts = page_start_indexes(&root);
    let ms = |frames: u64| (frames as f64 * 1000.0 / SAMPLE_RATE as f64).round() as u64;
    let quality_of = |index: usize| -> Option<&Value> {
        entries.iter().find(|entry| entry["index"] == json!(index))
    };

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
        timeline.push(json!({
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
            "loudness_db": quality_of(index).map(|entry| entry["loudness_db"].clone()),
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
        &json!({ "sample_rate": SAMPLE_RATE, "sentences": timeline, "total_ms": total_ms }),
    )?;
    manifest["voice"] = json!(args.voice.display().to_string());
    manifest["reference"] = json!(baseline.ref_path);
    manifest["model_dir"] = json!(model_dir.display().to_string());
    write_json(&manifest_path, &manifest)?;

    let mut srt = String::new();
    for entry in &timeline {
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
        "sentences": timeline.len(),
        "synthesized": synth,
        "trimmed": trim_needs,
        "quality_retried": redo,
        "quality_outliers": outliers,
        "reference": baseline.ref_path,
        "full": full_path.display().to_string(),
        "lead_in_ms": args.lead_in_ms,
        "tail_ms": args.tail_ms,
        "duration_ms": total_ms,
        "srt": srt_path.display().to_string(),
    }));
    Ok(())
}
