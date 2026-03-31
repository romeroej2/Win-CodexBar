//! Internationalization (i18n) system for CodexBar
//!
//! Provides translation lookup for all UI strings via the `Locale` struct.
//! Translations are embedded as JSON at compile time.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

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
            Language::ZhCn => "简体中文",
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
    lang: Language,
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
        Self { lang, map }
    }

    /// Translate a key. Returns a static fallback string if not found.
    /// This allows showing missing translations during development.
    pub fn t(&self, key: &str) -> &str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    /// Translate a key and substitute a single `{placeholder}` with a value.
    /// Example: `t("app.used_pct", "{pct}", "75")` → `"已使用 75%"`
    pub fn tf(&self, key: &str, placeholder: &str, value: &str) -> String {
        self.t(key).replace(placeholder, value)
    }

    /// Get the current language
    pub fn language(&self) -> Language {
        self.lang
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_zh_cn() {
        let locale = Locale::load(Language::ZhCn);
        assert!(locale.t("tray.open").len() > 0);
        assert_ne!(locale.t("tray.open"), "tray.open");
    }

    #[test]
    fn test_load_en() {
        let locale = Locale::load(Language::En);
        assert!(locale.t("tray.open").len() > 0);
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
        let result = locale.tf("app.used_pct", "{pct}", "75");
        assert!(!result.contains("{pct}"));
        assert!(result.contains("75"));
    }
}
