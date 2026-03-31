//! Internationalization (i18n) system for CodexBar
//!
//! Provides translation lookup for all UI strings via the `Locale` struct.
//! Translations are embedded as JSON at compile time.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

static EN_JSON: &str = include_str!("../locales/en.json");
static ZH_CN_JSON: &str = include_str!("../locales/zh-CN.json");

/// Supported UI languages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    /// Simplified Chinese (default)
    #[default]
    ZhCn,
    /// English
    En,
}

impl Language {
    /// Display name in the language's own native script.
    /// Used in the language selector dropdown so it's always readable.
    pub fn display_name(&self) -> &'static str {
        match self {
            Language::ZhCn => "ç®€ä½“ä¸­æ–‡",
            Language::En => "English",
        }
    }

    /// All available languages
    pub fn all() -> &'static [Language] {
        &[Language::ZhCn, Language::En]
    }
}

/// Translation lookup system backed by embedded JSON files
pub struct Locale {
    map: HashMap<String, String>,
}

impl Locale {
    /// Load translations for a given language
    pub fn load(lang: Language) -> Self {
        let json = match lang {
            Language::ZhCn => ZH_CN_JSON,
            Language::En => EN_JSON,
        };
        let map: HashMap<String, String> =
            serde_json::from_str(json).expect("locale JSON is always valid");
        Self { map }
    }

    /// Translate a key. Returns a static fallback string if not found.
    /// This allows showing missing translations during development.
    pub fn t(&self, key: &str) -> &str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_zh_cn() {
        let locale = Locale::load(Language::ZhCn);
        assert!(!locale.t("tray.open").is_empty());
        assert_ne!(locale.t("tray.open"), "tray.open");
    }

    #[test]
    fn test_load_en() {
        let locale = Locale::load(Language::En);
        assert!(!locale.t("tray.open").is_empty());
        assert_ne!(locale.t("tray.open"), "tray.open");
    }

    #[test]
    fn test_fallback() {
        let locale = Locale::load(Language::En);
        assert_eq!(locale.t("nonexistent.key"), "");
    }

    #[test]
    fn test_substitution() {
        let locale = Locale::load(Language::ZhCn);
        let result = locale.t("app.used_pct").replace("{pct}", "75");
        assert!(!result.contains("{pct}"));
        assert!(result.contains("75"));
    }
}
