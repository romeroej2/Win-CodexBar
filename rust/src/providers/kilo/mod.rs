//! Kilo provider implementation
//!
//! Fetches credit and pass usage from Kilo's tRPC batch API.
//! Auth: KILO_API_KEY env var, config, or ~/.local/share/kilo/auth.json

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const KILO_API_BASE: &str = "https://app.kilo.ai/api/trpc";
const KILO_CREDENTIAL_TARGET: &str = "codexbar-kilo";

const PROCEDURES: [&str; 3] = [
    "user.getCreditBlocks",
    "kiloPass.getState",
    "user.getAutoTopUpPaymentMethod",
];

pub struct KiloProvider {
    metadata: ProviderMetadata,
}

impl KiloProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Kilo,
                display_name: "Kilo",
                session_label: "Credits",
                weekly_label: "Kilo Pass",
                supports_opus: false,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://app.kilo.ai/account/usage"),
                status_page_url: None,
            },
        }
    }

    fn get_api_token(api_key: Option<&str>) -> Result<String, ProviderError> {
        if let Some(key) = api_key {
            if !key.is_empty() {
                return Ok(key.to_string());
            }
        }

        if let Ok(entry) = keyring::Entry::new(KILO_CREDENTIAL_TARGET, "api_token") {
            if let Ok(token) = entry.get_password() {
                return Ok(token);
            }
        }

        if let Ok(key) = std::env::var("KILO_API_KEY") {
            let trimmed = key.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }

        // Try CLI auth file (~/.local/share/kilo/auth.json)
        if let Some(token) = Self::read_cli_auth_token() {
            return Ok(token);
        }

        Err(ProviderError::NotInstalled(
            "Kilo API key not found. Set KILO_API_KEY, configure in Preferences, or run `kilo login`.".to_string(),
        ))
    }

    fn read_cli_auth_token() -> Option<String> {
        let home = dirs::home_dir()?;
        let auth_path = home
            .join(".local")
            .join("share")
            .join("kilo")
            .join("auth.json");

        let data = std::fs::read_to_string(&auth_path).ok()?;
        let parsed: AuthFile = serde_json::from_str(&data).ok()?;
        let token = parsed.kilo?.access?;
        let trimmed = token.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    fn build_batch_url() -> Result<String, ProviderError> {
        let joined = PROCEDURES.join(",");
        let input_map: serde_json::Value = serde_json::json!({
            "0": {"json": null},
            "1": {"json": null},
            "2": {"json": null},
        });
        let input_str =
            serde_json::to_string(&input_map).map_err(|e| ProviderError::Parse(e.to_string()))?;

        Ok(format!(
            "{}/{}?batch=1&input={}",
            KILO_API_BASE,
            joined,
            urlencoding_encode(&input_str)
        ))
    }

    async fn fetch_usage_api(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let api_key = Self::get_api_token(ctx.api_key.as_deref())?;
        let url = Self::build_batch_url()?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Accept", "application/json")
            .send()
            .await?;

        match resp.status().as_u16() {
            401 | 403 => return Err(ProviderError::AuthRequired),
            404 => {
                return Err(ProviderError::Parse(
                    "Kilo API endpoint not found (404). tRPC batch path may have changed."
                        .to_string(),
                ))
            }
            500..=599 => {
                return Err(ProviderError::Other(format!(
                    "Kilo API unavailable (HTTP {})",
                    resp.status()
                )))
            }
            200 => {}
            code => {
                return Err(ProviderError::Other(format!(
                    "Kilo API returned HTTP {}",
                    code
                )))
            }
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Invalid JSON: {}", e)))?;

        Self::parse_batch_response(&body)
    }

    fn parse_batch_response(root: &serde_json::Value) -> Result<UsageSnapshot, ProviderError> {
        let entries = Self::extract_entries(root)?;

        let credit_payload = entries.first().and_then(|e| Self::result_payload(e));
        let pass_payload = entries.get(1).and_then(|e| Self::result_payload(e));

        let (credits_used, credits_total, credits_remaining) = Self::parse_credits(&credit_payload);
        let pass = Self::parse_pass(&pass_payload);
        let plan_name = Self::parse_plan_name(&pass_payload);

        let primary = Self::build_credits_window(credits_used, credits_total, credits_remaining);
        let secondary = Self::build_pass_window(&pass);

        let mut usage = UsageSnapshot::new(primary);
        if let Some(sec) = secondary {
            usage = usage.with_secondary(sec);
        }
        if let Some(name) = plan_name {
            usage = usage.with_login_method(name);
        }

        Ok(usage)
    }

    fn extract_entries(root: &serde_json::Value) -> Result<Vec<&serde_json::Value>, ProviderError> {
        if let Some(arr) = root.as_array() {
            return Ok(arr.iter().take(PROCEDURES.len()).collect());
        }
        if let Some(obj) = root.as_object() {
            if obj.contains_key("result") || obj.contains_key("error") {
                return Ok(vec![root]);
            }
            let mut indexed: Vec<(usize, &serde_json::Value)> = obj
                .iter()
                .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v)))
                .filter(|(i, _)| *i < PROCEDURES.len())
                .collect();
            indexed.sort_by_key(|(i, _)| *i);
            if !indexed.is_empty() {
                return Ok(indexed.into_iter().map(|(_, v)| v).collect());
            }
        }
        Err(ProviderError::Parse(
            "Unexpected tRPC batch shape".to_string(),
        ))
    }

    fn result_payload(entry: &serde_json::Value) -> Option<serde_json::Value> {
        let result = entry.get("result")?;
        if let Some(data) = result.get("data") {
            if let Some(json) = data.get("json") {
                if json.is_null() {
                    return None;
                }
                return Some(json.clone());
            }
            return Some(data.clone());
        }
        if let Some(json) = result.get("json") {
            if json.is_null() {
                return None;
            }
            return Some(json.clone());
        }
        None
    }

    fn parse_credits(
        payload: &Option<serde_json::Value>,
    ) -> (Option<f64>, Option<f64>, Option<f64>) {
        let payload = match payload {
            Some(p) => p,
            None => return (None, None, None),
        };

        if let Some(blocks) = payload.get("creditBlocks").and_then(|v| v.as_array()) {
            let mut total = 0.0_f64;
            let mut remaining = 0.0_f64;
            let mut saw_total = false;
            let mut saw_remaining = false;

            for block in blocks {
                if let Some(amount) = block.get("amount_mUsd").and_then(as_f64) {
                    total += amount / 1_000_000.0;
                    saw_total = true;
                }
                if let Some(balance) = block.get("balance_mUsd").and_then(as_f64) {
                    remaining += balance / 1_000_000.0;
                    saw_remaining = true;
                }
            }

            if saw_total || saw_remaining {
                let t = if saw_total {
                    Some(total.max(0.0))
                } else {
                    None
                };
                let r = if saw_remaining {
                    Some(remaining.max(0.0))
                } else {
                    None
                };
                let u = match (t, r) {
                    (Some(t_val), Some(r_val)) => Some((t_val - r_val).max(0.0)),
                    _ => None,
                };
                return (u, t, r);
            }
        }

        // Zero-balance edge case
        if let Some(balance) = payload.get("totalBalance_mUsd").and_then(as_f64) {
            if balance == 0.0 {
                return (Some(0.0), Some(0.0), Some(0.0));
            }
            let b = (balance / 1_000_000.0).max(0.0);
            return (Some(0.0), Some(b), Some(b));
        }

        (None, None, None)
    }

    fn parse_pass(payload: &Option<serde_json::Value>) -> PassFields {
        let payload = match payload {
            Some(p) => p,
            None => return PassFields::default(),
        };

        if let Some(sub) = Self::subscription_data(payload) {
            let used = sub
                .get("currentPeriodUsageUsd")
                .and_then(as_f64)
                .map(|v| v.max(0.0));
            let base = sub
                .get("currentPeriodBaseCreditsUsd")
                .and_then(as_f64)
                .map(|v| v.max(0.0));
            let bonus = sub
                .get("currentPeriodBonusCreditsUsd")
                .and_then(as_f64)
                .unwrap_or(0.0)
                .max(0.0);
            let total = base.map(|b| b + bonus);
            let remaining = match (total, used) {
                (Some(t), Some(u)) => Some((t - u).max(0.0)),
                _ => None,
            };
            let resets_at = sub
                .get("nextBillingAt")
                .and_then(parse_date)
                .or_else(|| sub.get("nextRenewalAt").and_then(parse_date))
                .or_else(|| sub.get("renewsAt").and_then(parse_date));

            return PassFields {
                used,
                total,
                remaining,
                bonus: if bonus > 0.0 { Some(bonus) } else { None },
                resets_at,
            };
        }

        PassFields::default()
    }

    fn subscription_data(payload: &serde_json::Value) -> Option<&serde_json::Value> {
        if let Some(sub) = payload.get("subscription") {
            if sub.is_null() {
                return None;
            }
            if sub.is_object() {
                return Some(sub);
            }
        }
        let has_shape = payload.get("currentPeriodUsageUsd").is_some()
            || payload.get("currentPeriodBaseCreditsUsd").is_some()
            || payload.get("tier").is_some();
        if has_shape {
            Some(payload)
        } else {
            None
        }
    }

    fn parse_plan_name(payload: &Option<serde_json::Value>) -> Option<String> {
        let payload = payload.as_ref()?;
        if let Some(sub) = Self::subscription_data(payload) {
            if let Some(tier) = sub.get("tier").and_then(|v| v.as_str()) {
                let trimmed = tier.trim();
                if !trimmed.is_empty() {
                    return Some(match trimmed {
                        "tier_19" => "Starter".to_string(),
                        "tier_49" => "Pro".to_string(),
                        "tier_199" => "Expert".to_string(),
                        other => other.to_string(),
                    });
                }
            }
            return Some("Kilo Pass".to_string());
        }
        None
    }

    fn build_credits_window(
        used: Option<f64>,
        total: Option<f64>,
        _remaining: Option<f64>,
    ) -> RateWindow {
        let (u, t) = match (used, total) {
            (Some(u), Some(t)) => (u, t),
            _ => return RateWindow::new(0.0),
        };

        let percent = if t > 0.0 {
            ((u / t) * 100.0).clamp(0.0, 100.0)
        } else {
            100.0
        };

        let desc = format!("{}/{} credits", compact_number(u), compact_number(t));
        let mut w = RateWindow::new(percent);
        w.reset_description = Some(desc);
        w
    }

    fn build_pass_window(pass: &PassFields) -> Option<RateWindow> {
        let total = pass.total?;
        let used = pass.used.unwrap_or(0.0);
        let bonus = pass.bonus.unwrap_or(0.0);
        let base = (total - bonus).max(0.0);

        let percent = if total > 0.0 {
            ((used / total) * 100.0).clamp(0.0, 100.0)
        } else {
            100.0
        };

        let mut desc = format!("${:.2} / ${:.2}", used, base);
        if bonus > 0.0 {
            desc.push_str(&format!(" (+ ${:.2} bonus)", bonus));
        }

        let mut w = RateWindow::new(percent);
        w.reset_description = Some(desc);
        w.resets_at = pass.resets_at;
        Some(w)
    }
}

impl Default for KiloProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for KiloProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Kilo
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Kilo usage");

        match ctx.source_mode {
            SourceMode::Auto | SourceMode::OAuth => {
                let usage = self.fetch_usage_api(ctx).await?;
                Ok(ProviderFetchResult::new(usage, "api"))
            }
            SourceMode::Web | SourceMode::Cli => {
                Err(ProviderError::UnsupportedSource(ctx.source_mode))
            }
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::OAuth]
    }

    fn supports_web(&self) -> bool {
        false
    }

    fn supports_cli(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct PassFields {
    used: Option<f64>,
    total: Option<f64>,
    remaining: Option<f64>,
    bonus: Option<f64>,
    resets_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct AuthFile {
    kilo: Option<KiloSection>,
}

#[derive(Deserialize)]
struct KiloSection {
    access: Option<String>,
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn parse_date(v: &serde_json::Value) -> Option<DateTime<Utc>> {
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = chrono::DateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%SZ") {
            return Some(dt.with_timezone(&Utc));
        }
    }
    if let Some(n) = as_f64(v) {
        let secs = if n.abs() > 10_000_000_000.0 {
            n / 1000.0
        } else {
            n
        };
        return DateTime::from_timestamp(secs as i64, 0);
    }
    None
}

fn compact_number(value: f64) -> String {
    if value == value.trunc() {
        format!("{}", value as i64)
    } else {
        format!("{:.2}", value)
    }
}

fn urlencoding_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                String::from(b as char)
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_credits_from_blocks() {
        let payload = serde_json::json!({
            "creditBlocks": [
                {"amount_mUsd": 10_000_000, "balance_mUsd": 7_000_000},
                {"amount_mUsd": 5_000_000, "balance_mUsd": 2_000_000},
            ]
        });
        let (used, total, remaining) = KiloProvider::parse_credits(&Some(payload));
        assert!((total.unwrap() - 15.0).abs() < 0.01);
        assert!((remaining.unwrap() - 9.0).abs() < 0.01);
        assert!((used.unwrap() - 6.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_credits_zero_balance() {
        let payload = serde_json::json!({
            "creditBlocks": [],
            "totalBalance_mUsd": 0
        });
        let (used, total, remaining) = KiloProvider::parse_credits(&Some(payload));
        assert_eq!(used, Some(0.0));
        assert_eq!(total, Some(0.0));
        assert_eq!(remaining, Some(0.0));
    }

    #[test]
    fn test_parse_plan_name_tier() {
        let payload = serde_json::json!({
            "subscription": {"tier": "tier_49"}
        });
        assert_eq!(
            KiloProvider::parse_plan_name(&Some(payload)),
            Some("Pro".to_string())
        );
    }

    #[test]
    fn test_parse_pass_subscription() {
        let payload = serde_json::json!({
            "subscription": {
                "currentPeriodUsageUsd": 5.0,
                "currentPeriodBaseCreditsUsd": 20.0,
                "currentPeriodBonusCreditsUsd": 5.0,
                "tier": "tier_49"
            }
        });
        let pass = KiloProvider::parse_pass(&Some(payload));
        assert!((pass.used.unwrap() - 5.0).abs() < 0.01);
        assert!((pass.total.unwrap() - 25.0).abs() < 0.01);
        assert!((pass.remaining.unwrap() - 20.0).abs() < 0.01);
        assert!((pass.bonus.unwrap() - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_compact_number() {
        assert_eq!(compact_number(100.0), "100");
        assert_eq!(compact_number(99.5), "99.50");
        assert_eq!(compact_number(0.0), "0");
    }

    #[test]
    fn test_build_batch_url() {
        let url = KiloProvider::build_batch_url().unwrap();
        assert!(url.starts_with(KILO_API_BASE));
        assert!(url.contains("batch=1"));
        assert!(url.contains("user.getCreditBlocks"));
    }
}
