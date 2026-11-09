use crate::check::read_script;
use crate::text::{self, print_json, write_json};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// article directory
    pub dir: PathBuf,
}

fn paragraph_counts(path: &Path) -> Result<Vec<usize>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut counts = Vec::new();
    let mut current = 0usize;
    for line in content.lines() {
        if line.trim().is_empty() {
            if current > 0 {
                counts.push(current);
                current = 0;
            }
        } else if line.trim().starts_with("## ") {
            // 标题行不是句子，不计数
        } else {
            current += 1;
        }
    }
    if current > 0 {
        counts.push(current);
    }
    Ok(counts)
}

pub fn build(root: &Path) -> Result<Value> {
    let script = root.join("script.md");
    let speech = root.join("speech.txt");
    for path in [&script, &speech] {
        if !path.is_file() {
            bail!("missing file: {}", path.display());
        }
    }
    let titles: Vec<String> = read_script(&script)
        .into_iter()
        .map(|(title, _)| title)
        .collect();
    let paragraphs = paragraph_counts(&speech)?;
    let slides = text::count_slides(root);

    if titles.is_empty() {
        bail!("no page heading starting with '##' in script.md");
    }
    if titles.len() != slides {
        bail!(
            "script.md page count does not match slide images ({} vs {})",
            titles.len(),
            slides
        );
    }
    if paragraphs.len() != titles.len() {
        bail!(
            "speech.txt paragraph count does not match script.md pages ({} vs {})",
            paragraphs.len(),
            titles.len()
        );
    }

    let mut pages = Vec::with_capacity(titles.len());
    let mut start = 1usize;
    for (index, (title, count)) in titles.iter().zip(&paragraphs).enumerate() {
        pages.push(json!({
            "slide": index + 1,
            "title": title,
            "from": start,
            "to": start + count - 1,
        }));
        start += count;
    }

    let path = root.join("slides/pages.json");
    let value = json!({ "pages": pages });
    write_json(&path, &value)?;
    Ok(json!({
        "ok": true,
        "path": path.display().to_string(),
        "pages": titles.len(),
        "sentences": start - 1,
    }))
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    let value = build(&root)?;
    print_json(&value);
    Ok(())
}
