use crate::text::{self, print_json};
use anyhow::{Result, anyhow};
use regex::Regex;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(clap::Args)]
pub struct Args {
    /// check type
    #[arg(value_parser = ["script", "speech", "tts-text"])]
    pub kind: String,
    /// article directory
    pub dir: PathBuf,
}

fn heading() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^##\s+(.+)$").unwrap())
}

/// script.md：`## 标题` + 正文。
pub fn read_script(path: &Path) -> Vec<(String, String)> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut pages: Vec<(String, String)> = Vec::new();
    let mut title: Option<String> = None;
    let mut lines: Vec<String> = Vec::new();
    for line in content.lines() {
        if let Some(found) = heading().captures(line.trim()) {
            if let Some(name) = title.take() {
                pages.push((name, lines.join("\n").trim().to_string()));
            }
            title = Some(found[1].trim().to_string());
            lines.clear();
        } else if title.is_some() {
            lines.push(line.to_string());
        }
    }
    if let Some(name) = title {
        pages.push((name, lines.join("\n").trim().to_string()));
    }
    pages
}

fn check_script(root: &Path) -> Value {
    let pages = read_script(&root.join("script.md"));
    let slides = text::count_slides(root);
    let empty: Vec<&str> = pages
        .iter()
        .filter(|(_, body)| body.is_empty())
        .map(|(title, _)| title.as_str())
        .collect();
    let longest = pages
        .iter()
        .max_by_key(|(_, body)| body.chars().count())
        .map(|(title, body)| json!({ "title": title, "chars": body.chars().count() }));
    let ok = !pages.is_empty() && empty.is_empty() && pages.len() == slides;
    json!({
        "ok": ok,
        "pages": pages.len(),
        "slides": slides,
        "empty": empty,
        "longest": longest,
    })
}

fn check_speech(root: &Path) -> Value {
    let article = root.join("article.md");
    let speech = root.join("speech.md");
    for path in [&article, &speech] {
        if !path.is_file() {
            return json!({ "ok": false, "error": format!("missing file: {}", path.display()) });
        }
    }
    let sentences = match text::read_sentences(&speech) {
        Ok(value) => value,
        Err(err) => return json!({ "ok": false, "error": err.to_string() }),
    };
    let article_text = std::fs::read_to_string(&article).unwrap_or_default();
    let body = text::squeeze(&article_text);
    let joined = text::squeeze(
        &sentences
            .iter()
            .map(|item| item.text.as_str())
            .collect::<String>(),
    );
    let translated = text::cjk_ratio(&article_text) < 0.2 && text::cjk_ratio(&joined) > 0.5;

    if translated {
        let missing: Vec<usize> = sentences
            .iter()
            .filter(|item| !text::has_cjk(&item.text))
            .map(|item| item.index)
            .collect();
        if !missing.is_empty() {
            return json!({
                "ok": false,
                "mode": "translated",
                "error": "speech.md contains sentences that are not Chinese",
                "lines": missing,
            });
        }
    } else if body != joined {
        let expected: Vec<char> = body.chars().collect();
        let actual: Vec<char> = joined.chars().collect();
        let at = expected
            .iter()
            .zip(&actual)
            .position(|(a, b)| a != b)
            .unwrap_or(expected.len().min(actual.len()));
        let from = at.saturating_sub(20);
        return json!({
            "ok": false,
            "error": "sentences do not reassemble into article.md body: text changed, sentence missing, or heading added",
            "article_len": expected.len(),
            "speech_len": actual.len(),
            "at": at,
            "article": expected[from..(at + 20).min(expected.len())].iter().collect::<String>(),
            "speech": actual[from..(at + 20).min(actual.len())].iter().collect::<String>(),
        });
    }

    let paragraphs = {
        let mut seen: Vec<usize> = sentences.iter().map(|item| item.paragraph).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    };
    let script_pages = read_script(&root.join("script.md")).len();
    if script_pages != 0 && script_pages != paragraphs {
        return json!({
            "ok": false,
            "mode": if translated { "translated" } else { "same-language" },
            "error": "speech.md paragraph count does not match script.md pages",
            "script_pages": script_pages,
            "speech_paragraphs": paragraphs,
        });
    }
    let longest = sentences
        .iter()
        .max_by(|a, b| text::visual_len(&a.text).total_cmp(&text::visual_len(&b.text)));
    json!({
        "ok": true,
        "mode": if translated { "translated" } else { "same-language" },
        "sentences": sentences.len(),
        "paragraphs": paragraphs,
        "script_pages": script_pages,
        "longest": longest.map(|item| json!({
            "index": item.index,
            "len": text::visual_len(&item.text),
            "text": item.text,
        })),
        "hint": if translated { Some("translation accuracy is confirmed manually via review-speech") } else { None },
        "next": "stop here: hand speech.md to the user for review; after confirmation run state done <dir> review-speech",
    })
}

fn check_tts_text(root: &Path) -> Value {
    let speech = root.join("speech.md");
    let tts = root.join("speech.tts.md");
    for path in [&speech, &tts] {
        if !path.is_file() {
            return json!({ "ok": false, "error": format!("missing file: {}", path.display()) });
        }
    }
    let (source, lines) = match (text::read_sentences(&speech), text::read_sentences(&tts)) {
        (Ok(source), Ok(lines)) => (source, lines),
        (Err(err), _) | (_, Err(err)) => {
            return json!({ "ok": false, "error": err.to_string() });
        }
    };
    if source.len() != lines.len() {
        return json!({
            "ok": false,
            "error": "line count mismatch",
            "speech": source.len(),
            "tts": lines.len(),
        });
    }
    let digits: Vec<Value> = lines
        .iter()
        .filter(|item| item.text.chars().any(|c| c.is_ascii_digit()))
        .map(|item| json!({ "index": item.index, "text": item.text }))
        .collect();
    let mismatched: Vec<Value> = source
        .iter()
        .zip(&lines)
        .filter(|(src, line)| src.paragraph != line.paragraph)
        .map(|(src, line)| json!({ "index": src.index, "source": src.text, "tts": line.text }))
        .collect();
    json!({
        "ok": digits.is_empty() && mismatched.is_empty(),
        "sentences": lines.len(),
        "digits_left": digits,
        "paragraph_mismatch": mismatched,
    })
}

pub fn evaluate(kind: &str, root: &Path) -> Value {
    match kind {
        "script" => check_script(root),
        "speech" => check_speech(root),
        "tts-text" => check_tts_text(root),
        _ => json!({ "ok": false, "error": format!("unknown check type: {kind}") }),
    }
}

pub fn run(args: Args) -> Result<()> {
    let root = args
        .dir
        .canonicalize()
        .map_err(|_| anyhow!("directory not found: {}", args.dir.display()))?;
    let value = evaluate(&args.kind, &root);
    let ok = value["ok"] == json!(true);
    print_json(&value);
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
