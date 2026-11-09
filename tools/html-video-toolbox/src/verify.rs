use crate::state::STEPS;
use crate::text::{self, print_json, read_json};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// 每秒多少语速单位算正常：单位 = 一个汉字或一个英文单词。
const MIN_SEC_PER_UNIT: f64 = 0.08;
const MAX_SEC_PER_UNIT: f64 = 0.60;

#[derive(clap::Args)]
pub struct Args {
    /// article directory
    pub dir: PathBuf,
}

fn probe_duration(path: &Path) -> f64 {
    let Ok(output) = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
    else {
        return -1.0;
    };
    if !output.status.success() {
        return -1.0;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(-1.0)
}

fn parse_srt(text: &str) -> Vec<(String, String)> {
    text.trim()
        .split("\n\n")
        .filter_map(|block| {
            let lines: Vec<&str> = block
                .lines()
                .filter(|line| !line.trim().is_empty())
                .collect();
            if lines.len() < 3 {
                return None;
            }
            Some((lines[1].to_string(), lines[2..].concat()))
        })
        .collect()
}

pub fn evaluate(root: &Path) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for name in [
        "article.md",
        "speech.txt",
        "speech.tts.txt",
        "voice/full.wav",
        "voice/timeline.json",
        "subs/final.srt",
        "final.mp4",
        "final.srt",
        "slides/pages.json",
    ] {
        if !root.join(name).exists() {
            problems.push(format!("missing file: {name}"));
        }
    }
    if !problems.is_empty() {
        return json!({ "ok": false, "problems": problems });
    }

    let progress = root.join("progress.json");
    if progress.exists() {
        let data = read_json(&progress).unwrap_or(json!({}));
        for step in STEPS.iter().filter(|step| **step != "verify") {
            if data["steps"][step]["status"].as_str() != Some("done") {
                problems.push(format!("step not marked done: {step}"));
            }
        }
    }

    let timeline = read_json(&root.join("voice/timeline.json")).unwrap_or(json!({}));
    let sentences = timeline["sentences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total = sentences.len();

    let pages = read_json(&root.join("slides/pages.json")).unwrap_or(json!({}))["pages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut expected = 1i64;
    for page in &pages {
        let from = page["from"].as_i64().unwrap_or(0);
        if from != expected {
            problems.push(format!(
                "pages.json not contiguous: expected from={expected}, got {from}"
            ));
            break;
        }
        expected = page["to"].as_i64().unwrap_or(0) + 1;
    }
    if expected != total as i64 + 1 {
        problems.push(format!(
            "pages.json covers {} sentences but there are {}",
            expected - 1,
            total
        ));
    }

    let srt = std::fs::read_to_string(root.join("subs/final.srt")).unwrap_or_default();
    let cues = parse_srt(&srt);
    if cues.len() != total {
        problems.push(format!("{} subtitle cues vs {} sentences", cues.len(), total));
    } else {
        for (index, (cue, sentence)) in cues.iter().zip(&sentences).enumerate() {
            let expected = text::strip_tail_punct(sentence["text"].as_str().unwrap_or(""));
            if expected != cue.1.as_str() {
                problems.push(format!("subtitle {} does not match speech.txt", index + 1));
            }
        }
    }

    for sentence in &sentences {
        let text = sentence["text"].as_str().unwrap_or("");
        let units = crate::text::speech_units(text);
        let seconds = sentence["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
        let per_unit = if units > 0.0 { seconds / units } else { 0.0 };
        if units > 0.0 && !(MIN_SEC_PER_UNIT..=MAX_SEC_PER_UNIT).contains(&per_unit) {
            warnings.push(format!(
                "sentence {} speaking rate out of range: {seconds:.2}s / {units} units",
                sentence["index"]
            ));
        }
    }

    let audio = probe_duration(&root.join("voice/full.wav"));
    let video = probe_duration(&root.join("final.mp4"));
    if audio < 0.0 || video < 0.0 {
        problems.push("ffprobe failed to read durations".to_string());
    } else {
        let timeline_seconds = timeline["total_ms"].as_f64().unwrap_or(0.0) / 1000.0;
        if (audio - timeline_seconds).abs() > 0.1 {
            problems.push(format!(
                "audio {audio:.2}s does not match timeline {timeline_seconds:.2}s"
            ));
        }
        if (video - audio).abs() > 0.5 {
            problems.push(format!("video {video:.2}s differs from audio {audio:.2}s by more than 0.5s"));
        }
    }

    json!({
        "ok": problems.is_empty(),
        "problems": problems,
        "warnings": warnings,
        "sentences": total,
        "pages": pages.len(),
        "audio_seconds": (audio * 1000.0).round() / 1000.0,
        "video_seconds": (video * 1000.0).round() / 1000.0,
    })
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    let value = evaluate(&root);
    let ok = value["ok"] == json!(true);
    print_json(&value);
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
