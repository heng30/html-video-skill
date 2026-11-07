use crate::text::{print_json, read_json};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};

const PAGE_LEAD_MS: i64 = 200;

#[derive(clap::Args)]
pub struct Args {
    /// article directory
    pub dir: PathBuf,
    #[arg(long, default_value_t = 25)]
    pub fps: u32,
    #[arg(long, default_value_t = 20)]
    pub crf: u32,
    #[arg(long, default_value = "medium")]
    pub preset: String,
}

fn require(path: &Path) -> Result<Value> {
    if !path.exists() {
        bail!("missing file: {}", path.display());
    }
    read_json(path)
}

fn wav_seconds(path: &Path) -> Result<f64> {
    let reader =
        hound::WavReader::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let spec = reader.spec();
    Ok(reader.duration() as f64 / spec.sample_rate as f64)
}

fn quote(path: &Path) -> String {
    path.display().to_string().replace('\'', "'\\''")
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;

    let pages = require(&root.join("slides/pages.json"))?["pages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if pages.is_empty() {
        bail!("no pages in pages.json");
    }
    let timeline = require(&root.join("voice/timeline.json"))?;
    let sentences = timeline["sentences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total_ms = timeline["total_ms"].as_i64().unwrap_or(0);

    let mut expected_from = 1i64;
    for page in &pages {
        let from = page["from"].as_i64().unwrap_or(0);
        let to = page["to"].as_i64().unwrap_or(0);
        if from != expected_from {
            bail!("pages.json sentence ranges not contiguous: expected from={expected_from}, got {from}");
        }
        if to < from {
            bail!("pages.json to is less than from: {page}");
        }
        expected_from = to + 1;
    }
    if expected_from != sentences.len() as i64 + 1 {
        bail!(
            "pages.json does not cover all sentences (covered {}, of {})",
            expected_from - 1,
            sentences.len()
        );
    }

    let mut starts: Vec<i64> = Vec::with_capacity(pages.len());
    for (pos, page) in pages.iter().enumerate() {
        if pos == 0 {
            starts.push(0);
            continue;
        }
        let from = page["from"].as_i64().unwrap_or(1) as usize;
        let start_ms = sentences[from - 1]["start_ms"].as_i64().unwrap_or(0);
        let start = (start_ms - PAGE_LEAD_MS).max(starts[pos - 1] + 100);
        starts.push(start);
    }
    let mut ends: Vec<i64> = starts[1..].to_vec();
    ends.push(total_ms);
    if starts.iter().zip(&ends).any(|(start, end)| end <= start) {
        bail!("non-positive page duration in timeline");
    }

    let mut slides = Vec::with_capacity(pages.len());
    for page in &pages {
        let image = match page["image"].as_str() {
            Some(image) => image.to_string(),
            None => format!(
                "slides/slide-{:02}.png",
                page["slide"].as_u64().unwrap_or(0)
            ),
        };
        let path = root.join(&image);
        if !path.exists() {
            bail!("missing slide image: {image}");
        }
        slides.push(path);
    }

    let concat_path = root.join(".compose.txt");
    let mut lines = String::new();
    for (pos, path) in slides.iter().enumerate() {
        lines.push_str(&format!(
            "file '{}'\nduration {:.3}\n",
            quote(path),
            (ends[pos] - starts[pos]) as f64 / 1000.0
        ));
    }
    lines.push_str(&format!("file '{}'\n", quote(&slides[slides.len() - 1])));
    std::fs::write(&concat_path, lines)?;

    let duration = total_ms as f64 / 1000.0;
    let mut command = std::process::Command::new("ffmpeg");
    command
        .current_dir(&root)
        .args(["-y", "-v", "error", "-nostdin"]);
    if slides.len() == 1 {
        command
            .args(["-loop", "1", "-framerate"])
            .arg(args.fps.to_string())
            .arg("-t")
            .arg(format!("{duration:.3}"))
            .arg("-i")
            .arg(&slides[0]);
    } else {
        command
            .args(["-f", "concat", "-safe", "0", "-i"])
            .arg(&concat_path);
    }
    let result = command
        .args(["-i", "voice/full.wav"])
        .args([
            "-vf",
            "scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2",
            "-c:v",
            "libx264",
            "-preset",
        ])
        .arg(&args.preset)
        .args(["-crf"])
        .arg(args.crf.to_string())
        .args(["-pix_fmt", "yuv420p", "-r"])
        .arg(args.fps.to_string())
        .args(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart", "-frames:v"])
        .arg((duration * args.fps as f64).ceil().to_string())
        .arg("final.mp4")
        .output()
        .context("failed to run ffmpeg")?;
    let _ = std::fs::remove_file(&concat_path);
    if !result.status.success() {
        bail!(
            "ffmpeg compose failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }

    std::fs::copy(root.join("subs/final.srt"), root.join("final.srt"))
        .context("failed to copy final.srt")?;
    let audio_seconds = wav_seconds(&root.join("voice/full.wav"))?;
    print_json(&serde_json::json!({
        "ok": true,
        "mp4": root.join("final.mp4").display().to_string(),
        "srt": root.join("final.srt").display().to_string(),
        "pages": pages.len(),
        "audio_seconds": (audio_seconds * 1000.0).round() / 1000.0,
        "subtitles_embedded": false,
    }));
    Ok(())
}
