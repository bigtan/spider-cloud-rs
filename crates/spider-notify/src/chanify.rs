use crate::config::validate_notify_url;
use crate::{Notifier, Result};
use anyhow::Context;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde_json::json;
use std::fmt;
use std::time::Duration;
use tracing::{debug, error, info};
use zeroize::Zeroizing;

/// Chanify webhook notifier.
pub struct ChanifyNotifier {
    url: String,
    token: Zeroizing<String>,
    client: Client,
}

impl fmt::Debug for ChanifyNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChanifyNotifier")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl ChanifyNotifier {
    /// Construct a new [`ChanifyNotifier`], validating the endpoint URL and building the HTTP client.
    pub fn new(url: String, token: String) -> Result<Self> {
        validate_notify_url(&url, "chanify.url")?;
        if token.trim().is_empty() {
            anyhow::bail!("Chanify token cannot be empty");
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build Chanify HTTP client")?;
        Ok(Self {
            url: url.trim().to_string(),
            token: Zeroizing::new(token),
            client,
        })
    }
}

impl Notifier for ChanifyNotifier {
    fn name(&self) -> &str {
        "Chanify"
    }

    fn send(&self, subject: &str, message: &str) -> Result<()> {
        let payload = json!({
            "token": self.token.as_str(),
            "title": subject,
            "text": message,
            "autocopy": 1,
            "sound": 1,
            "priority": 10,
            "interruptionlevel": 0,
        });

        debug!("Sending Chanify notification to: {}", self.url);

        let response = self
            .client
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&payload)
            .send()
            .context("Chanify HTTP request failed")?;

        if response.status().is_success() {
            info!("Chanify notification sent: {}", subject);
            Ok(())
        } else {
            let status = response.status();
            error!("Failed to send Chanify notification: {}", status);
            anyhow::bail!("chanify request failed: {}", status)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_token_and_rejects_external_http() {
        assert!(
            ChanifyNotifier::new(
                "http://example.com/v1/sender".to_string(),
                "secret-tok".to_string()
            )
            .is_err()
        );

        let notifier = ChanifyNotifier::new(
            "https://chanify.example.com/v1/sender".to_string(),
            "secret-tok".to_string(),
        )
        .unwrap();
        let dbg = format!("{notifier:?}");
        assert!(!dbg.contains("secret-tok"));
        assert!(dbg.contains("<redacted>"));
    }
}
