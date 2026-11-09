use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

pub struct Sentence {
    pub index: usize,
    pub paragraph: usize,
    pub text: String,
}

/// 非空正文行是一句，空行结束当前段落；`##` 标题行不进语音与字幕。
pub fn read_sentences(path: &Path) -> Result<Vec<Sentence>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut sentences: Vec<Sentence> = Vec::new();
    let mut paragraph = 0usize;
    let mut seen_text = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            if seen_text {
                paragraph += 1;
                seen_text = false;
            }
            continue;
        }
        if line.starts_with("## ") {
            continue;
        }
        seen_text = true;
        sentences.push(Sentence {
            index: sentences.len() + 1,
            paragraph,
            text: line.to_string(),
        });
    }
    Ok(sentences)
}

/// 字幕行尾去掉句读标点，否则拖尾标点在屏幕上很突兀。
pub fn strip_tail_punct(text: &str) -> &str {
    let trimmed = text.trim_end_matches(|c: char| "。，；：、！？!?,.;:".contains(c));
    trimmed.trim_end_matches(['—', '-', '）', ')'])
}

pub fn squeeze(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

pub fn visual_len(text: &str) -> f64 {
    text.chars()
        .map(|c| if (c as u32) < 128 { 0.5 } else { 1.0 })
        .sum()
}

/// 语速单位：一个汉字 = 1，一个英文单词 = 1。
/// 逐字符数会把 `VoidZero` 当成 8 个字，带英文标识符的句子因此被判成语速异常，
/// `tts` 反复重合成、`verify` 报一串假警告。
pub fn speech_units(text: &str) -> f64 {
    let mut units = 0.0;
    let mut in_word = false;
    for c in text.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            units += 1.0;
            in_word = false;
        } else if c.is_alphabetic() {
            if !in_word {
                units += 1.0;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    units
}

pub fn cjk_ratio(text: &str) -> f64 {
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return 0.0;
    }
    let cjk = letters
        .iter()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(*c))
        .count();
    cjk as f64 / letters.len() as f64
}

pub fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

pub fn read_json(path: &Path) -> Result<Value> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("JSON parse failed: {}", path.display()))
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, text + "\n").with_context(|| format!("cannot write {}", path.display()))
}

/// slides/slide-XX.png 的数量。
pub fn count_slides(root: &Path) -> usize {
    std::fs::read_dir(root.join("slides"))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    name.starts_with("slide-") && name.ends_with(".png")
                })
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::speech_units;

    #[test]
    fn english_word_counts_as_one_unit() {
        assert_eq!(speech_units("它找得到差异"), 6.0);
        assert_eq!(speech_units("用 VoidZero 打包"), 1.0 + 1.0 + 2.0);
        assert_eq!(speech_units("VoidZero"), 1.0);
        assert_eq!(speech_units("napi-rs"), 2.0);
        assert_eq!(speech_units("它叫 O X C"), 2.0 + 3.0);
        assert_eq!(speech_units(""), 0.0);
        assert_eq!(speech_units("   "), 0.0);
        // 混排时数和逐字符算出来的差距，正是"英文让语速爆表"的根因
        assert!(speech_units("ts.Transformer") < "ts.Transformer".chars().count() as f64);
    }
}
