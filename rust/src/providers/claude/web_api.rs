//! Claude Web API fetcher - uses browser cookies to fetch usage from claude.ai

use chrono::{DateTime, Utc};
use reqwest::{Client, header};
use serde::Deserialize;

use crate::browser::cookies::get_cookie_header;
use crate::core::{CostSnapshot, ProviderError, ProviderFetchResult, RateWindow, UsageSnapshot};

/// Claude Web API fetcher
pub struct ClaudeWebApiFetcher {
    client: Client,
}

/// Organization info from Claude API
#[derive(Debug, Deserialize)]
struct Organization {
    uuid: String,
    #[allow(dead_code)]
    name: Option<String>,
}

/// Usage response from Claude API
#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(rename = "five_hour")]
    five_hour: Option<UsageWindow>,

    #[serde(rename = "seven_day")]
    seven_day: Option<UsageWindow>,

    #[serde(rename = "seven_day_opus")]
    seven_day_opus: Option<UsageWindow>,

    #[serde(rename = "seven_day_sonnet")]
    seven_day_sonnet: Option<UsageWindow>,
}

/// A usage window from the API
#[derive(Debug, Deserialize)]
struct UsageWindow {
    utilization: Option<f64>,

    #[serde(rename = "resets_at")]
    resets_at: Option<String>,
}

/// Extra usage (credits) response
#[derive(Debug, Deserialize)]
struct ExtraUsageResponse {
    #[serde(rename = "monthly_credit_limit")]
    monthly_credit_limit: Option<f64>,

    #[serde(rename = "used_credits")]
    used_credits: Option<f64>,

    currency: Option<String>,

    #[serde(rename = "is_enabled")]
    is_enabled: Option<bool>,
}

/// Account info response
#[derive(Debug, Deserialize)]
struct AccountResponse {
    email_address: Option<String>,

    #[serde(rename = "rate_limit_tier")]
    rate_limit_tier: Option<String>,
}

impl ClaudeWebApiFetcher {
    const BASE_URL: &'static str = "https://claude.ai/api";

    /// Create a new fetcher
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Fetch usage using browser cookies or env-var session key
    pub async fn fetch_with_cookies(&self) -> Result<ProviderFetchResult, ProviderError> {
        // Check for explicit session key in environment first (most reliable)
        if let Some(session_key) = Self::resolve_session_key_from_env() {
            tracing::debug!("Using session key from environment variable");
            let cookie_header = format!("sessionKey={}", session_key);
            return self.fetch_with_cookie_header(&cookie_header).await;
        }

        // Try multiple domains - Claude uses different domains for different services
        let domains = [
            "claude.ai",
            "claude.com",
            "console.anthropic.com",
            "anthropic.com",
        ];

        for domain in domains {
            match get_cookie_header(domain) {
                Ok(cookie_header) if !cookie_header.is_empty() => {
                    tracing::debug!("Found cookies for {}", domain);
                    return self.fetch_with_cookie_header(&cookie_header).await;
                }
                Ok(_) => {
                    tracing::debug!("No cookies found for {}", domain);
                }
                Err(e) => {
                    tracing::debug!("Failed to get cookies for {}: {}", domain, e);
                }
            }
        }

        Err(ProviderError::NoCookies)
    }

    /// Fetch usage with a provided cookie header
    pub async fn fetch_with_cookie_header(
        &self,
        cookie_header: &str,
    ) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Claude usage via web API");

        // Build browser-mimicking headers shared across all requests
        let headers = Self::build_headers(cookie_header);

        // Step 1: Get organization ID
        let org_id = self.get_organization_id(&headers).await?;
        tracing::debug!("Got organization ID: {}", org_id);

        // Fetch usage data
        let usage = self.get_usage(&org_id, &headers).await?;

        // Fetch extra usage (credits) - optional
        let extra_usage = self.get_extra_usage(&org_id, &headers).await.ok();

        // Fetch account info - optional
        let account = self.get_account_info(&headers).await.ok();

        // Build the result
        let primary = usage
            .five_hour
            .as_ref()
            .map(|w| self.to_rate_window(w, Some(300))) // 5 hours = 300 minutes
            .unwrap_or_else(|| RateWindow::new(0.0));

        let secondary = usage
            .seven_day
            .as_ref()
            .map(|w| self.to_rate_window(w, Some(10080))); // 7 days = 10080 minutes

        let model_specific = usage
            .seven_day_opus
            .as_ref()
            .map(|w| self.to_rate_window(w, Some(10080)));

        let mut snapshot = UsageSnapshot::new(primary);

        if let Some(s) = secondary {
            snapshot = snapshot.with_secondary(s);
        }

        if let Some(m) = model_specific {
            snapshot = snapshot.with_model_specific(m);
        }

        if let Some(ref acc) = account {
            if let Some(ref email) = acc.email_address {
                snapshot = snapshot.with_email(email.clone());
            }
            if let Some(ref tier) = acc.rate_limit_tier {
                snapshot = snapshot.with_login_method(Self::tier_to_plan_name(tier));
            }
        }

        let mut result = ProviderFetchResult::new(snapshot, "web");

        // Add cost info if available
        if let Some(extra) = extra_usage
            && extra.is_enabled.unwrap_or(false)
        {
            let used_cents = extra.used_credits.unwrap_or(0.0);
            let limit_cents = extra.monthly_credit_limit;
            let currency = extra.currency.unwrap_or_else(|| "USD".to_string());

            let mut cost = CostSnapshot::new(
                used_cents / 100.0, // Convert cents to dollars
                currency,
                "Monthly",
            );

            if let Some(limit) = limit_cents {
                cost = cost.with_limit(limit / 100.0);
            }

            result = result.with_cost(cost);
        }

        Ok(result)
    }

    /// Build browser-mimicking headers for all claude.ai API requests.
    /// claude.ai validates that requests look like they come from the web app.
    fn build_headers(cookie_header: &str) -> reqwest::header::HeaderMap {
        use reqwest::header::HeaderValue;
        let mut map = reqwest::header::HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(cookie_header) {
            map.insert(header::COOKIE, v);
        }
        map.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        map.insert(header::ORIGIN, HeaderValue::from_static("https://claude.ai"));
        map.insert(header::REFERER, HeaderValue::from_static("https://claude.ai/settings/usage"));
        map.insert(header::USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36"));
        map.insert(reqwest::header::HeaderName::from_static("anthropic-client-platform"), HeaderValue::from_static("web_claude_ai"));
        map
    }

    /// Resolve a session key from `CLAUDE_AI_SESSION_KEY` or `CLAUDE_WEB_SESSION_KEY` env vars.
    fn resolve_session_key_from_env() -> Option<String> {
        let key = std::env::var("CLAUDE_AI_SESSION_KEY")
            .or_else(|_| std::env::var("CLAUDE_WEB_SESSION_KEY"))
            .ok()?;
        let key = key.trim().to_string();
        key.starts_with("sk-ant-").then_some(key)
    }

    /// Get the organization ID from the API
    async fn get_organization_id(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<String, ProviderError> {
        let url = format!("{}/organizations", Self::BASE_URL);

        let response = self
            .client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get organizations: {}",
                response.status()
            )));
        }

        let orgs: Vec<Organization> = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Failed to parse organizations: {}", e)))?;

        orgs.into_iter()
            .next()
            .map(|o| o.uuid)
            .ok_or_else(|| ProviderError::Parse("No organizations found".to_string()))
    }

    /// Get usage data
    async fn get_usage(
        &self,
        org_id: &str,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<UsageResponse, ProviderError> {
        let url = format!("{}/organizations/{}/usage", Self::BASE_URL, org_id);

        let response = self
            .client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get usage: {}",
                response.status()
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Failed to parse usage: {}", e)))
    }

    /// Get extra usage (credits)
    async fn get_extra_usage(
        &self,
        org_id: &str,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<ExtraUsageResponse, ProviderError> {
        let url = format!(
            "{}/organizations/{}/overage_spend_limit",
            Self::BASE_URL,
            org_id
        );

        let response = self
            .client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get extra usage: {}",
                response.status()
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Failed to parse extra usage: {}", e)))
    }

    /// Get account info
    async fn get_account_info(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AccountResponse, ProviderError> {
        let url = format!("{}/account", Self::BASE_URL);

        let response = self
            .client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get account: {}",
                response.status()
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Failed to parse account: {}", e)))
    }

    /// Convert a usage window to a RateWindow
    fn to_rate_window(&self, window: &UsageWindow, window_minutes: Option<u32>) -> RateWindow {
        let used_percent = normalize_utilization(window.utilization.unwrap_or(0.0));

        let resets_at = window
            .resets_at
            .as_ref()
            .and_then(|s| Self::parse_iso8601(s));

        let reset_description = resets_at.map(Self::format_reset_time);

        RateWindow::with_details(used_percent, window_minutes, resets_at, reset_description)
    }

    /// Parse ISO8601 date string
    fn parse_iso8601(s: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                    .ok()
                    .map(|ndt| ndt.and_utc())
            })
    }

    /// Format reset time for display
    fn format_reset_time(dt: DateTime<Utc>) -> String {
        dt.format("%b %-d at %-I:%M%p").to_string()
    }

    /// Convert rate limit tier to plan name
    fn tier_to_plan_name(tier: &str) -> String {
        match tier.to_lowercase().as_str() {
            "free" => "Claude Free".to_string(),
            "pro" | "claude_pro" => "Claude Pro".to_string(),
            "max" | "claude_max_5" | "claude_max_20" => "Claude Max".to_string(),
            "team" => "Claude Team".to_string(),
            "enterprise" => "Claude Enterprise".to_string(),
            _ => format!("Claude ({})", tier),
        }
    }
}

impl Default for ClaudeWebApiFetcher {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_utilization(utilization: f64) -> f64 {
    if utilization > 0.0 && utilization <= 1.0 {
        utilization * 100.0
    } else {
        utilization
    }
}

#[cfg(test)]
mod tests {
    use super::{ClaudeWebApiFetcher, UsageWindow};

    #[test]
    fn converts_fractional_utilization_to_percent() {
        let window = UsageWindow {
            utilization: Some(0.23),
            resets_at: None,
        };

        let rate = ClaudeWebApiFetcher::new().to_rate_window(&window, Some(300));

        assert!((rate.used_percent - 23.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preserves_existing_percentage_utilization() {
        let window = UsageWindow {
            utilization: Some(23.0),
            resets_at: None,
        };

        let rate = ClaudeWebApiFetcher::new().to_rate_window(&window, Some(300));

        assert!((rate.used_percent - 23.0).abs() < f64::EPSILON);
    }
}
