//! 逐句音频体检：响度 / 音色 / 音高 / 语速。
//!
//! 合成结果的三类返工（音色漂移、语速偏差、音量偏差）在这里量化：
//! - 音色：有声帧的对数频谱均值（去均值后）与参考音频的余弦相似度
//! - 音高：有声帧基频中位数
//! - 语速：有声时长（跳过句内停顿）/ 语速单位
//! - 响度：有声帧的均方电平
//!
//! 锚取自克隆参考音频本身（切段自相似分布标定阈值），不靠手调常数。

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

use crate::text::{read_json, write_json};

/// 分析用采样率；参考音频与合成句都降到这个采样率再算特征。
pub const ANA_SR: u32 = 16_000;
const N_FFT: usize = 512;
const HOP: usize = 160;
const BANDS: usize = 24;
const BAND_LO: f32 = 100.0;
const BAND_HI: f32 = 7_600.0;
const F0_MIN: f32 = 70.0;
const F0_MAX: f32 = 400.0;
/// 有声帧门限：比最响的帧低多少 dB 算静音。
const GATE_DB: f32 = 30.0;
/// 绝对静音门限，防止整句极轻时把噪声当有声。
const FLOOR_DB: f32 = -55.0;

/// 响度目标（有声帧均方电平）与最大补增益。
pub const TARGET_DB: f32 = -20.0;
pub const MAX_GAIN_DB: f32 = 9.0;
/// 峰值上限，拼接后留 1dB 余量。
const PEAK_CEIL_DB: f32 = -1.0;

/// 参考音频切段做自相似标定时的段长。
const ANCHOR_CHUNK_MS: usize = 1_500;
const ANCHOR_MIN_CHUNKS: usize = 3;
/// 切段不够时的兜底音色下限。
const FALLBACK_TIMBRE_FLOOR: f64 = 0.90;
/// 自标定出来的下限再钳一道，避免短参考音频标出离谱的值。
const FLOOR_MIN: f64 = 0.80;
const FLOOR_MAX: f64 = 0.95;
/// 合成音频和参考音频不是同一次录音（编码、噪声底、混响都不同），音色相似度
/// 天然比"参考 vs 参考"低一截，标定值要往下让这么多。
const SYNTH_MARGIN: f64 = 0.04;

#[derive(Clone, Debug)]
pub struct Metrics {
    /// 有声帧总时长（毫秒）
    pub voiced_ms: f64,
    pub loudness_db: f64,
    pub peak_db: f64,
    /// 有声帧基频中位数；读不出返回 0
    pub f0_hz: f64,
    /// 24 维对数频谱均值（已去均值），音色指纹
    pub timbre: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
pub struct Tolerances {
    pub rate: f64,
    pub pitch: f64,
}

#[derive(Clone, Debug)]
pub struct Deviation {
    /// 1.0 = 正好到阈值
    pub score: f64,
    pub why: &'static str,
}

/// 体检锚：参考音频的音色/音高，加上本批句子的语速中位数。
#[derive(Clone, Debug)]
pub struct Baseline {
    pub ref_path: String,
    pub f0_hz: f64,
    pub timbre: Vec<f32>,
    pub timbre_floor: f64,
    pub timbre_floor_source: String,
    /// 语速中心（毫秒/单位）；0 表示还没测出来
    pub rate_ms: f64,
    pub sentences: usize,
    pub cfg: f32,
    pub steps: usize,
}

impl Baseline {
    /// 从参考音频建锚：整段算音色/音高，切段自相似标定音色下限。
    pub fn from_reference(audio: &[f32], path: &Path) -> Result<Self> {
        let whole = analyze(audio, ANA_SR).context("参考音频读不出有声段")?;
        // 拿"短段 vs 整段"的相似度当分布：和"一句 vs 整段"是同一个比较方式，
        // 分布才对得上；短段两两比会偏乐观。
        let chunks = chunk_metrics(audio, ANA_SR);
        let mut sims: Vec<f64> = chunks
            .iter()
            .map(|chunk| cosine(&chunk.timbre, &whole.timbre))
            .collect();
        let (floor, source) = if chunks.len() >= ANCHOR_MIN_CHUNKS {
            sims.sort_by(f64::total_cmp);
            let index = (chunks.len() / 5).min(sims.len() - 1);
            (
                (sims[index] - SYNTH_MARGIN).clamp(FLOOR_MIN, FLOOR_MAX),
                format!("ref p20 of {} chunks", chunks.len()),
            )
        } else {
            (
                FALLBACK_TIMBRE_FLOOR,
                format!("fallback, only {} chunks", chunks.len()),
            )
        };
        Ok(Self {
            ref_path: path.display().to_string(),
            f0_hz: whole.f0_hz,
            timbre: whole.timbre,
            timbre_floor: floor,
            timbre_floor_source: source,
            rate_ms: 0.0,
            sentences: 0,
            cfg: 0.0,
            steps: 0,
        })
    }

    pub fn load(root: &Path) -> Option<Self> {
        let data = read_json(&path_of(root)).ok()?;
        let timbre: Vec<f32> = data["timbre"]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        if timbre.len() != BANDS {
            return None;
        }
        Some(Self {
            ref_path: data["ref_path"].as_str().unwrap_or_default().to_string(),
            f0_hz: data["f0_hz"].as_f64().unwrap_or(0.0),
            timbre,
            timbre_floor: data["timbre_floor"].as_f64().unwrap_or(FALLBACK_TIMBRE_FLOOR),
            timbre_floor_source: data["timbre_floor_source"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            rate_ms: data["rate_ms"].as_f64().unwrap_or(0.0),
            sentences: data["sentences"].as_u64().unwrap_or(0) as usize,
            cfg: data["cfg"].as_f64().unwrap_or(0.0) as f32,
            steps: data["steps"].as_u64().unwrap_or(0) as usize,
        })
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_json(
            &path_of(root),
            &json!({
                "ref_path": self.ref_path,
                "f0_hz": round(self.f0_hz, 2),
                "timbre": self.timbre.iter().map(|v| round(*v as f64, 4)).collect::<Vec<_>>(),
                "timbre_floor": round(self.timbre_floor, 4),
                "timbre_floor_source": self.timbre_floor_source,
                "rate_ms": round(self.rate_ms, 2),
                "sentences": self.sentences,
                "cfg": self.cfg,
                "steps": self.steps,
            }),
        )
    }
}

pub fn path_of(root: &Path) -> std::path::PathBuf {
    root.join("voice/baseline.json")
}

fn round(value: f64, digits: u32) -> f64 {
    let scale = 10f64.powi(digits as i32);
    (value * scale).round() / scale
}

/// 语速：毫秒/单位；单位太少不参与统计（太短的句子语速天然偏高）。
pub fn rate_ms(metrics: &Metrics, units: f64) -> Option<f64> {
    if units <= 0.0 || metrics.voiced_ms <= 0.0 {
        return None;
    }
    Some(metrics.voiced_ms / units)
}

/// 相对锚的偏差，取四项里最差的那一项。
pub fn deviations(
    metrics: &Metrics,
    units: f64,
    baseline: &Baseline,
    tol: &Tolerances,
) -> Deviation {
    let mut worst = Deviation {
        score: 0.0,
        why: "ok",
    };
    let mut note = |score: f64, why: &'static str| {
        if score > worst.score {
            worst = Deviation { score, why };
        }
    };

    if !baseline.timbre.is_empty() {
        let room = (1.0 - baseline.timbre_floor).max(0.02);
        note((1.0 - cosine(&metrics.timbre, &baseline.timbre)) / room, "timbre");
    }
    if baseline.f0_hz > 0.0 && metrics.f0_hz > 0.0 && tol.pitch > 0.0 {
        note(
            (metrics.f0_hz / baseline.f0_hz - 1.0).abs() / tol.pitch,
            "pitch",
        );
    }
    if baseline.rate_ms > 0.0
        && tol.rate > 0.0
        && let Some(rate) = rate_ms(metrics, units)
    {
        note((rate / baseline.rate_ms - 1.0).abs() / tol.rate, "rate");
    }
    // 响度已经归一过，剩下还低于目标一整个钳位范围，说明这个 take 本身是坏的
    // （闷、破、几乎没声），补增益救不回来。
    let floor = (TARGET_DB - MAX_GAIN_DB) as f64;
    if metrics.loudness_db < floor {
        note((floor - metrics.loudness_db) / 3.0 + 1.0, "level");
    }
    worst
}

/// 把这一句拉到目标响度需要的增益（dB，可负）。
pub fn needed_gain(metrics: &Metrics) -> Option<f32> {
    if metrics.loudness_db <= FLOOR_DB as f64 {
        return None;
    }
    let mut gain = TARGET_DB - metrics.loudness_db as f32;
    let headroom = PEAK_CEIL_DB - metrics.peak_db as f32;
    if gain > headroom {
        gain = headroom;
    }
    Some(gain)
}

pub fn report_entry(
    index: usize,
    text: &str,
    metrics: &Metrics,
    dev: &Deviation,
    baseline: &Baseline,
) -> Value {
    json!({
        "index": index,
        "text": text,
        "voiced_ms": metrics.voiced_ms.round(),
        "loudness_db": round(metrics.loudness_db, 1),
        "peak_db": round(metrics.peak_db, 1),
        "f0_hz": round(metrics.f0_hz, 1),
        "timbre_sim": round(cosine(&metrics.timbre, &baseline.timbre), 3),
        "score": round(dev.score, 3),
        "worst": dev.why,
    })
}

pub fn to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|s| *s as f32 / 32768.0).collect()
}

/// 混单声道并降到 [`ANA_SR`]。
pub fn mono_at(samples: &[f32], channels: u16, rate: u32) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    let mono: Vec<f32> = if channels == 1 {
        samples.to_vec()
    } else {
        samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if rate == ANA_SR {
        return mono;
    }
    audio_utils::audio::resample_audio(&mono, rate, ANA_SR, 1).unwrap_or(mono)
}

/// 有声段特征：响度/峰值/基频/对数频谱均值。
pub fn analyze(audio: &[f32], rate: u32) -> Option<Metrics> {
    if audio.len() < N_FFT {
        return None;
    }
    let frames = frames(audio);
    let peak_db = peak_db(audio);
    let mut voiced: Vec<&[f32]> = Vec::new();
    let mut energies: Vec<f32> = Vec::with_capacity(frames.len());
    for frame in &frames {
        energies.push(frame_rms_db(frame));
    }
    let loudest = energies.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    if !loudest.is_finite() || loudest <= FLOOR_DB {
        return None;
    }
    let gate = (loudest - GATE_DB).max(FLOOR_DB);
    for (frame, energy) in frames.iter().zip(&energies) {
        if *energy > gate {
            voiced.push(frame);
        }
    }
    if voiced.is_empty() {
        return None;
    }

    let mut square_sum = 0.0f64;
    let mut sample_count = 0usize;
    for frame in &voiced {
        for sample in frame.iter() {
            square_sum += (*sample as f64) * (*sample as f64);
        }
        sample_count += frame.len();
    }
    let mean_square = square_sum / sample_count.max(1) as f64;
    let loudness_db = 10.0 * mean_square.max(1e-10).log10();

    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let bands = band_edges();
    let mut band_power = [0.0f64; BANDS];
    let mut pitches: Vec<f32> = Vec::new();
    for frame in &voiced {
        input.iter_mut().for_each(|v| *v = 0.0);
        for (i, sample) in frame.iter().enumerate() {
            let window = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N_FFT as f32).cos();
            input[i] = sample * window;
        }
        if fft.process(&mut input, &mut output).is_err() {
            return None;
        }
        let power: Vec<f64> = output.iter().map(|c| (c.re * c.re + c.im * c.im) as f64).collect();
        for (band, range) in bands.iter().enumerate() {
            band_power[band] += power[range.clone()].iter().sum::<f64>();
        }
        if let Some(f0) = pitch(frame, rate) {
            pitches.push(f0);
        }
    }

    let mut timbre: Vec<f32> = band_power
        .iter()
        .map(|power| (10.0 * (power / voiced.len() as f64).max(1e-12).log10()) as f32)
        .collect();
    let mean = timbre.iter().sum::<f32>() / BANDS as f32;
    timbre.iter_mut().for_each(|v| *v -= mean);

    pitches.sort_by(f32::total_cmp);
    Some(Metrics {
        voiced_ms: voiced.len() as f64 * HOP as f64 * 1000.0 / rate as f64,
        loudness_db,
        peak_db,
        f0_hz: pitches
            .get(pitches.len() / 2)
            .map(|v| *v as f64)
            .unwrap_or(0.0),
        timbre,
    })
}

/// 参考音频切段，用来量"同一个人"的相似度分布。
fn chunk_metrics(audio: &[f32], rate: u32) -> Vec<Metrics> {
    // 参考音频往往只有 3-4 秒：段长跟着总长走，保证有足够的样本
    let wanted = ANCHOR_CHUNK_MS * rate as usize / 1000;
    let chunk = wanted.min(audio.len() / ANCHOR_MIN_CHUNKS).max(rate as usize / 2);
    let step = chunk / 2;
    let mut out = Vec::new();
    let mut start = 0usize;
    while start + chunk <= audio.len() {
        if let Some(metrics) = analyze(&audio[start..start + chunk], rate) {
            out.push(metrics);
        }
        start += step;
    }
    out
}

fn frames(audio: &[f32]) -> Vec<&[f32]> {
    let mut out = Vec::with_capacity(audio.len() / HOP);
    let mut start = 0usize;
    while start + N_FFT <= audio.len() {
        out.push(&audio[start..start + N_FFT]);
        start += HOP;
    }
    out
}

fn frame_rms_db(frame: &[f32]) -> f32 {
    let mean_square =
        frame.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / frame.len() as f64;
    10.0 * mean_square.max(1e-12).log10() as f32
}

fn peak_db(audio: &[f32]) -> f64 {
    let peak = audio.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    20.0 * (peak.max(1e-6) as f64).log10()
}

/// 基频：归一化自相关，取第一个接近峰值的滞后（避开倍频误判）。
fn pitch(frame: &[f32], rate: u32) -> Option<f32> {
    let min_lag = (rate as f32 / F0_MAX) as usize;
    let max_lag = (rate as f32 / F0_MIN) as usize;
    if frame.len() <= max_lag + 1 || min_lag < 2 {
        return None;
    }
    let energy: f32 = frame.iter().map(|s| s * s).sum();
    if energy <= 1e-6 {
        return None;
    }
    let mut correlations = Vec::with_capacity(max_lag - min_lag + 1);
    for lag in min_lag..=max_lag {
        let mut sum = 0.0f32;
        for i in 0..frame.len() - lag {
            sum += frame[i] * frame[i + lag];
        }
        correlations.push(sum / energy);
    }
    let (peak, value) = correlations
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |acc, (i, v)| {
            if *v > acc.1 { (i, *v) } else { acc }
        });
    if value < 0.3 {
        return None;
    }
    // 半周期也接近峰值说明挑中的是倍周期，取一半
    let mut offset = peak;
    if offset % 2 == 0 && correlations[offset / 2] > value * 0.85 {
        offset /= 2;
    }
    // 抛物线插值，抵消整数滞后带来的偏差
    let lag = min_lag + offset;
    let refined = if offset > 0 && offset + 1 < correlations.len() {
        let (left, mid, right) = (
            correlations[offset - 1],
            correlations[offset],
            correlations[offset + 1],
        );
        let denominator = left - 2.0 * mid + right;
        if denominator.abs() > 1e-9 {
            lag as f32 + 0.5 * (left - right) / denominator
        } else {
            lag as f32
        }
    } else {
        lag as f32
    };
    Some(rate as f32 / refined)
}

fn cosine(left: &[f32], right: &[f32]) -> f64 {
    let mut dot = 0.0f64;
    let mut left_norm = 0.0f64;
    let mut right_norm = 0.0f64;
    for (a, b) in left.iter().zip(right) {
        dot += (*a as f64) * (*b as f64);
        left_norm += (*a as f64) * (*a as f64);
        right_norm += (*b as f64) * (*b as f64);
    }
    if left_norm <= 0.0 || right_norm <= 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

/// 对数均匀分布的频带边界（FFT bin 下标）。
fn band_edges() -> Vec<std::ops::Range<usize>> {
    let bin_hz = ANA_SR as f32 / N_FFT as f32;
    let ratio = (BAND_HI / BAND_LO).powf(1.0 / BANDS as f32);
    (0..BANDS)
        .map(|band| {
            let low = BAND_LO * ratio.powi(band as i32);
            let high = BAND_LO * ratio.powi(band as i32 + 1);
            let from = ((low / bin_hz) as usize).max(1);
            let to = ((high / bin_hz) as usize + 1).min(N_FFT / 2);
            from..to.max(from + 1)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, rate: u32, ms: usize) -> Vec<f32> {
        let count = rate as usize * ms / 1000;
        (0..count)
            .map(|i| {
                let t = i as f32 / rate as f32;
                0.3 * (std::f32::consts::TAU * freq * t).sin()
            })
            .collect()
    }

    fn noisy_tone(freq: f32, rate: u32, ms: usize) -> Vec<f32> {
        tone(freq, rate, ms)
            .into_iter()
            .enumerate()
            .map(|(i, v)| v + 0.05 * ((i * 7919) % 13) as f32 / 13.0)
            .collect()
    }

    #[test]
    fn pitch_follows_the_fundamental() {
        let low = analyze(&tone(110.0, ANA_SR, 600), ANA_SR).unwrap();
        let high = analyze(&tone(220.0, ANA_SR, 600), ANA_SR).unwrap();
        assert!((low.f0_hz - 110.0).abs() < 8.0, "got {}", low.f0_hz);
        assert!((high.f0_hz - 220.0).abs() < 12.0, "got {}", high.f0_hz);
    }

    #[test]
    fn timbre_separates_different_spectra_and_matches_itself() {
        let a = analyze(&tone(220.0, ANA_SR, 600), ANA_SR).unwrap();
        let b = analyze(&noisy_tone(220.0, ANA_SR, 600), ANA_SR).unwrap();
        let again = analyze(&tone(220.0, ANA_SR, 600), ANA_SR).unwrap();
        assert!(cosine(&a.timbre, &again.timbre) > 0.99);
        assert!(cosine(&a.timbre, &b.timbre) < 0.95);
    }

    #[test]
    fn voiced_time_skips_internal_pause() {
        let mut audio = tone(180.0, ANA_SR, 300);
        audio.extend(std::iter::repeat_n(0.0f32, ANA_SR as usize));
        audio.extend(tone(180.0, ANA_SR, 300));
        let metrics = analyze(&audio, ANA_SR).unwrap();
        assert!(
            metrics.voiced_ms < 700.0,
            "pause should not count: {}",
            metrics.voiced_ms
        );
        assert!(metrics.voiced_ms > 500.0, "got {}", metrics.voiced_ms);
    }

    #[test]
    fn gain_pulls_loudness_to_target_and_respects_ceiling() {
        let quiet: Vec<f32> = tone(200.0, ANA_SR, 400).iter().map(|v| v * 0.05).collect();
        let metrics = analyze(&quiet, ANA_SR).unwrap();
        let gain = needed_gain(&metrics).unwrap();
        assert!(gain > 9.0, "quiet take should need a big boost: {gain}");
        let mut loud: Vec<f32> = tone(200.0, ANA_SR, 400);
        loud.iter_mut().for_each(|v| *v *= 3.0);
        let metrics = analyze(&loud, ANA_SR).unwrap();
        assert!(needed_gain(&metrics).unwrap() < 0.0);
    }

    #[test]
    fn deviation_names_the_worst_metric() {
        let reference = analyze(&tone(200.0, ANA_SR, 3000), ANA_SR).unwrap();
        let baseline = Baseline {
            ref_path: "ref.wav".into(),
            f0_hz: reference.f0_hz,
            timbre: reference.timbre.clone(),
            timbre_floor: 0.95,
            timbre_floor_source: "test".into(),
            rate_ms: 480.0,
            sentences: 1,
            cfg: 2.0,
            steps: 10,
        };
        let tol = Tolerances { rate: 0.25, pitch: 0.2 };
        let same = deviations(&reference, 6.0, &baseline, &tol);
        assert!(same.score < 1.0, "identical take should pass: {same:?}");
        let fast = deviations(&reference, 20.0, &baseline, &tol);
        assert_eq!(fast.why, "rate");
        assert!(fast.score > 1.0);
        let high = analyze(&tone(300.0, ANA_SR, 3000), ANA_SR).unwrap();
        let drifted = deviations(&high, 6.0, &baseline, &tol);
        assert!(drifted.score > 1.0);
    }
}

