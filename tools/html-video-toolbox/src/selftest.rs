use crate::text::print_json;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn write(root: &Path, name: &str, text: &str) -> Result<()> {
    let path = root.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

fn expect(label: &str, value: &Value, want_ok: bool) -> Result<()> {
    if value["ok"] == json!(want_ok) {
        return Ok(());
    }
    bail!("selftest failed [{label}] expected ok={want_ok}, got {value}")
}

pub fn run() -> Result<()> {
    let root: PathBuf =
        std::env::temp_dir().join(format!("html-video-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let mut checks = 0;

    write(&root, "url", "https://example.com\n")?;
    write(&root, "article.md", "First sentence.\n\nSecond sentence.\n")?;
    write(&root, "title.md", "Title\n")?;
    write(&root, "slides/slide-01.png", "")?;
    write(&root, "slides/slide-02.png", "")?;
    write(&root, "covers/cover-16x9.png", "")?;
    write(&root, "covers/cover-4x3.png", "")?;
    write(&root, "covers/cover-3x4.png", "")?;
    write(
        &root,
        "script.md",
        "## 01 Test page one\nFirst sentence.\n\n## 02 Test page two\nSecond sentence.\n",
    )?;
    write(&root, "speech.txt", "First sentence.\n\nSecond sentence.\n")?;
    write(&root, "speech.tts.txt", "First sentence.\n\nSecond sentence.\n")?;

    expect("speech ok", &crate::check::evaluate("speech", &root), true)?;
    checks += 1;
    expect(
        "tts-text ok",
        &crate::check::evaluate("tts-text", &root),
        true,
    )?;
    checks += 1;

    let pages = crate::pages::build(&root)?;
    expect("pages", &pages, true)?;
    if pages["pages"] != json!(2) || pages["sentences"] != json!(2) {
        bail!("selftest failed [pages] wrong page or sentence count: {pages}");
    }
    checks += 1;
    let written = crate::text::read_json(&root.join("slides/pages.json"))?;
    if written["pages"][1]["from"] != json!(2) || written["pages"][1]["to"] != json!(2) {
        bail!("selftest failed [pages.json] wrong sentence ranges: {written}");
    }
    checks += 1;

    // 改字后必须拼不回原文
    write(&root, "speech.txt", "First sentence.\n\nSecond sentence edited.\n")?;
    expect(
        "speech edited",
        &crate::check::evaluate("speech", &root),
        false,
    )?;
    checks += 1;
    write(&root, "speech.txt", "First sentence.\n\nSecond sentence.\n")?;

    // 配音稿残留阿拉伯数字
    write(&root, "speech.tts.txt", "First sentence.\n\nSecond sentence in 2024.\n")?;
    let digits = crate::check::evaluate("tts-text", &root);
    expect("tts-text digits", &digits, false)?;
    if digits["digits_left"].as_array().map(Vec::len) != Some(1) {
        bail!("selftest failed [digits_left] expected 1 item: {digits}");
    }
    checks += 1;
    write(&root, "speech.tts.txt", "First sentence.\n\nSecond sentence.\n")?;

    // 段落数与页数不一致
    write(&root, "speech.txt", "First sentence.\nSecond sentence.\n")?;
    expect(
        "speech paragraphs",
        &crate::check::evaluate("speech", &root),
        false,
    )?;
    checks += 1;
    write(&root, "speech.txt", "First sentence.\n\nSecond sentence.\n")?;

    // 分句校验：script 页数与 slides 一致
    expect("script ok", &crate::check::evaluate("script", &root), true)?;
    checks += 1;

    // 断点状态：按顺序 done，越级拒绝
    let status = crate::state::status(&root)?;
    if status["next"] != json!("fetch") {
        bail!("selftest failed [state] next should be fetch: {status}");
    }
    crate::state::mark_done(&root, "fetch")?;
    if crate::state::mark_done(&root, "script").is_ok() {
        bail!("selftest failed [state] skipping done should be rejected");
    }
    let status = crate::state::mark_done(&root, "slides")?;
    if status["next"] != json!("script") {
        bail!("selftest failed [state] next should be script: {status}");
    }
    checks += 1;
    let reset = crate::state::reset(&root, Some("slides"))?;
    if reset["next"] != json!("slides") {
        bail!("selftest failed [state reset]: {reset}");
    }
    checks += 1;

    // ASR 比对：数字的阿拉伯写法与中文读法算一致
    if (crate::asr::similarity("二零二四年发布", "2024年发布") - 1.0).abs() > 1e-9 {
        bail!("selftest failed [asr] digits should normalize to the same text");
    }
    if crate::asr::similarity("完全不一样的一句话", "牛头不对马嘴的感受") > 0.6 {
        bail!("selftest failed [asr] unrelated texts must not match");
    }
    checks += 1;

    let _ = std::fs::remove_dir_all(&root);
    print_json(&json!({ "ok": true, "checks": checks }));
    Ok(())
}
