//! Minimal UI language helpers.

pub const UI_LANGUAGE_EN: &str = "en";

pub fn is_zh(language: &str) -> bool {
    matches!(
        language.trim().to_ascii_lowercase().as_str(),
        "zh" | "zh-cn" | "zh_cn" | "cn" | "chinese"
    )
}

pub fn tr<'a>(language: &str, en: &'a str, zh: &'a str) -> &'a str {
    if is_zh(language) { zh } else { en }
}
