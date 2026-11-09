use crate::text::{print_json, read_json, write_json};
use anyhow::{Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const STEPS: [&str; 10] = [
    "fetch",
    "slides",
    "script",
    "review-script",
    "speech",
    "review-speech",
    "tts-text",
    "tts",
    "compose",
    "verify",
];

#[derive(Subcommand)]
pub enum Command {
    /// current next step
    Status { dir: PathBuf },
    /// mark a step done (in order only)
    Done { dir: PathBuf, step: String },
    /// reset steps to pending from one onward (keeps artifacts)
    Reset { dir: PathBuf, step: Option<String> },
}

fn required(step: &str) -> &'static [&'static str] {
    match step {
        "fetch" => &["url", "article.md", "title.md"],
        "slides" => &["slides", "covers"],
        "script" => &["script.md"],
        "speech" => &["speech.txt", "slides/pages.json"],
        "tts-text" => &["speech.tts.txt"],
        "tts" => &["voice/full.wav", "subs/final.srt"],
        "compose" => &["final.mp4", "final.srt"],
        _ => &[],
    }
}

fn hint(step: &str) -> Option<&'static str> {
    Some(match step {
        "fetch" => "fetch article text and images, write url / article.md / title.md",
        "slides" => "build index.html + cover.html, run screenshot for slides and --covers",
        "script" => "write script.md page by page against slides, then run check script",
        "review-script" => "after manually reviewing script.md, run state done <dir> review-script",
        "speech" => "split script.md into sentences as speech.txt, then run pages and check speech",
        "review-speech" => "after manually reviewing speech.txt, run state done <dir> review-speech",
        "tts-text" => "generate speech.tts.txt (digits to Chinese), then run check tts-text",
        "tts" => "run tts to synthesize audio and subtitles",
        "compose" => "run compose to build the video",
        "verify" => "run verify for acceptance",
        _ => return None,
    })
}

fn load(root: &Path) -> Result<Value> {
    let path = root.join("progress.json");
    let mut data = if path.exists() {
        read_json(&path)?
    } else {
        json!({ "steps": {} })
    };
    if !data["steps"].is_object() {
        data["steps"] = json!({});
    }
    let has_url = data["url"].as_str().is_some_and(|url| !url.is_empty());
    if !has_url && let Ok(url) = std::fs::read_to_string(root.join("url")) {
        data["url"] = json!(url.trim());
    }
    Ok(data)
}

fn step_done(root: &Path, data: &Value, step: &str) -> bool {
    data["steps"][step]["status"].as_str() == Some("done")
        && required(step).iter().all(|item| root.join(item).exists())
}

pub fn status(root: &Path) -> Result<Value> {
    let data = load(root)?;
    let mut next = None;
    let steps: Vec<Value> = STEPS
        .iter()
        .map(|step| {
            let done = step_done(root, &data, step);
            if !done && next.is_none() {
                next = Some(*step);
            }
            json!({ "name": step, "status": if done { "done" } else { "pending" } })
        })
        .collect();
    let paused = match next {
        Some("review-script") => {
            Some("waiting for manual review of script.md (one paragraph per page); after confirmation run state done <dir> review-script")
        }
        Some("review-speech") => {
            Some("waiting for manual review of speech.txt (sentences); after confirmation run state done <dir> review-speech")
        }
        _ => None,
    };
    Ok(json!({
        "ok": true,
        "dir": root.display().to_string(),
        "url": data["url"],
        "steps": steps,
        "next": next,
        "hint": next.and_then(hint),
        "paused": paused,
    }))
}

pub fn mark_done(root: &Path, step: &str) -> Result<Value> {
    let Some(index) = STEPS.iter().position(|name| *name == step) else {
        bail!("unknown step: {step} (options: {})", STEPS.join(", "));
    };
    let mut data = load(root)?;
    let missing: Vec<&str> = STEPS[..index]
        .iter()
        .copied()
        .filter(|name| !step_done(root, &data, name))
        .collect();
    if !missing.is_empty() {
        bail!("prerequisite steps not done: {}", missing.join(", "));
    }
    data["steps"][step] = json!({ "status": "done" });
    write_json(&root.join("progress.json"), &data)?;
    status(root)
}

pub fn reset(root: &Path, step: Option<&str>) -> Result<Value> {
    let start = match step {
        None => 0,
        Some(name) => STEPS
            .iter()
            .position(|item| *item == name)
            .ok_or_else(|| anyhow::anyhow!("unknown step: {name}"))?,
    };
    let mut data = load(root)?;
    for name in &STEPS[start..] {
        if let Some(steps) = data["steps"].as_object_mut() {
            steps.remove(*name);
        }
    }
    write_json(&root.join("progress.json"), &data)?;
    status(root)
}

fn resolve(dir: &Path) -> Result<PathBuf> {
    let root = dir
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("directory not found: {}", dir.display()))?;
    Ok(root)
}

pub fn run(cmd: Command) -> Result<()> {
    let value = match cmd {
        Command::Status { dir } => status(&resolve(&dir)?)?,
        Command::Done { dir, step } => mark_done(&resolve(&dir)?, &step)?,
        Command::Reset { dir, step } => reset(&resolve(&dir)?, step.as_deref())?,
    };
    print_json(&value);
    Ok(())
}
