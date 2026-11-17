use crate::Result;
use anyhow::Context;
use lettre::message::Mailbox;
use serde::Deserialize;
use std::env;
use std::fmt;
use url::Url;

/// Aggregated notification channel settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotifierConfig {
    pub chanify: ChanifyConfig,
    pub email: EmailConfig,
    pub pushgo: PushgoConfig,
}

/// Chanify webhook configuration.
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ChanifyConfig {
    pub enabled: bool,
    pub url: Option<String>,
    pub token: Option<String>,
}

impl fmt::Debug for ChanifyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChanifyConfig")
            .field("enabled", &self.enabled)
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// SMTP email notification configuration.
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EmailConfig {
    pub enabled: bool,
    pub sender: Option<String>,
    pub password: Option<String>,
    pub recipient: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
}

impl fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailConfig")
            .field("enabled", &self.enabled)
            .field("sender", &self.sender)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("recipient", &self.recipient)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .finish()
    }
}

/// Pushgo encrypted push notification configuration.
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PushgoConfig {
    pub enabled: bool,
    pub url: Option<String>,
    pub api_token: Option<String>,
    pub hex_key: Option<String>,
    pub channel_id: Option<String>,
    pub password: Option<String>,
    pub icon: Option<String>,
    pub image: Option<String>,
}

impl fmt::Debug for PushgoConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushgoConfig")
            .field("enabled", &self.enabled)
            .field("url", &self.url)
            .field("api_token", &self.api_token.as_ref().map(|_| "<redacted>"))
            .field("hex_key", &self.hex_key.as_ref().map(|_| "<redacted>"))
            .field("channel_id", &self.channel_id)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("icon", &self.icon)
            .field("image", &self.image)
            .finish()
    }
}

impl NotifierConfig {
    /// Normalize blank strings to `None`, fill missing values from environment variables,
    /// and fail fast if any enabled channel has invalid or incomplete settings.
    pub fn resolve_and_validate(&mut self) -> Result<()> {
        self.resolve_with_env(|key| env::var(key).ok())
    }

    /// Testable helper that resolves missing fields via `env_lookup` and validates enabled channels.
    pub(crate) fn resolve_with_env(
        &mut self,
        mut env_lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<()> {
        for opt in [
            &mut self.chanify.url,
            &mut self.chanify.token,
            &mut self.email.sender,
            &mut self.email.password,
            &mut self.email.recipient,
            &mut self.email.smtp_host,
            &mut self.pushgo.url,
            &mut self.pushgo.api_token,
            &mut self.pushgo.hex_key,
            &mut self.pushgo.channel_id,
            &mut self.pushgo.password,
            &mut self.pushgo.icon,
            &mut self.pushgo.image,
        ] {
            normalize_opt(opt);
        }

        fill_from_env(&mut self.chanify.url, "CHANIFY_URL", &mut env_lookup);
        fill_from_env(&mut self.chanify.token, "CHANIFY_TOKEN", &mut env_lookup);

        fill_from_env(&mut self.email.sender, "EMAIL_SENDER", &mut env_lookup);
        fill_from_env(&mut self.email.password, "EMAIL_PASSWORD", &mut env_lookup);
        fill_from_env(
            &mut self.email.recipient,
            "EMAIL_RECIPIENT",
            &mut env_lookup,
        );
        fill_from_env(
            &mut self.email.smtp_host,
            "EMAIL_SMTP_HOST",
            &mut env_lookup,
        );

        fill_from_env(&mut self.pushgo.url, "PUSHGO_URL", &mut env_lookup);
        fill_from_env(
            &mut self.pushgo.api_token,
            "PUSHGO_API_TOKEN",
            &mut env_lookup,
        );
        fill_from_env(&mut self.pushgo.hex_key, "PUSHGO_HEX_KEY", &mut env_lookup);
        fill_from_env(
            &mut self.pushgo.channel_id,
            "PUSHGO_CHANNEL_ID",
            &mut env_lookup,
        );
        fill_from_env(
            &mut self.pushgo.password,
            "PUSHGO_PASSWORD",
            &mut env_lookup,
        );

        self.validate()
    }

    /// Validate all enabled notification channels without consulting environment variables.
    pub fn validate(&self) -> Result<()> {
        if self.chanify.enabled {
            let url = require_non_empty(&self.chanify.url, "notifier.chanify.url / CHANIFY_URL")?;
            validate_notify_url(url, "notifier.chanify.url")?;
            require_non_empty(
                &self.chanify.token,
                "notifier.chanify.token / CHANIFY_TOKEN",
            )?;
        }

        if self.email.enabled {
            let sender =
                require_non_empty(&self.email.sender, "notifier.email.sender / EMAIL_SENDER")?;
            require_non_empty(
                &self.email.password,
                "notifier.email.password / EMAIL_PASSWORD",
            )?;
            let recipient = require_non_empty(
                &self.email.recipient,
                "notifier.email.recipient / EMAIL_RECIPIENT",
            )?;

            sender
                .parse::<Mailbox>()
                .with_context(|| format!("Invalid email sender mailbox '{sender}'"))?;
            recipient
                .parse::<Mailbox>()
                .with_context(|| format!("Invalid email recipient mailbox '{recipient}'"))?;

            if self.email.smtp_port == Some(0) {
                anyhow::bail!("notifier.email.smtp_port must be greater than 0");
            }
            if let Some(host) = &self.email.smtp_host
                && host.trim().is_empty()
            {
                anyhow::bail!("notifier.email.smtp_host cannot be blank");
            }
        }

        if self.pushgo.enabled {
            let url = require_non_empty(&self.pushgo.url, "notifier.pushgo.url / PUSHGO_URL")?;
            validate_notify_url(url, "notifier.pushgo.url")?;
            require_non_empty(
                &self.pushgo.api_token,
                "notifier.pushgo.api_token / PUSHGO_API_TOKEN",
            )?;
            let hex_key = require_non_empty(
                &self.pushgo.hex_key,
                "notifier.pushgo.hex_key / PUSHGO_HEX_KEY",
            )?;
            let decoded = hex::decode(hex_key.trim())
                .context("notifier.pushgo.hex_key must be valid hexadecimal")?;
            if decoded.len() != 16 && decoded.len() != 32 {
                anyhow::bail!(
                    "notifier.pushgo.hex_key must decode to 16 or 32 bytes (got {})",
                    decoded.len()
                );
            }
            require_non_empty(
                &self.pushgo.channel_id,
                "notifier.pushgo.channel_id / PUSHGO_CHANNEL_ID",
            )?;
            require_non_empty(
                &self.pushgo.password,
                "notifier.pushgo.password / PUSHGO_PASSWORD",
            )?;
        }

        Ok(())
    }
}

pub(crate) fn validate_notify_url(raw: &str, field_name: &str) -> Result<()> {
    let parsed = Url::parse(raw.trim()).with_context(|| format!("Invalid URL for {field_name}"))?;

    match parsed.scheme() {
        "https" => {
            if parsed.host_str().is_none() {
                anyhow::bail!("{field_name} must include a host");
            }
            Ok(())
        }
        "http" => {
            if matches!(
                parsed.host_str(),
                Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
            ) {
                Ok(())
            } else {
                anyhow::bail!(
                    "{field_name} must use https:// (insecure http:// is only allowed for localhost / 127.0.0.1)"
                );
            }
        }
        other => {
            anyhow::bail!("{field_name} has unsupported scheme '{other}': expected https://")
        }
    }
}

fn normalize_opt(opt: &mut Option<String>) {
    if let Some(value) = opt {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            *opt = None;
        } else if trimmed.len() != value.len() {
            *opt = Some(trimmed.to_string());
        }
    }
}

fn fill_from_env(
    target: &mut Option<String>,
    var_name: &str,
    env_lookup: &mut impl FnMut(&str) -> Option<String>,
) {
    if target.is_none()
        && let Some(raw) = env_lookup(var_name)
    {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            *target = Some(trimmed.to_string());
        }
    }
}

fn require_non_empty<'a>(value: &'a Option<String>, field_name: &str) -> Result<&'a str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing required setting: {field_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_have_no_hardcoded_urls_and_reject_unknown_fields() {
        let cfg: NotifierConfig = toml::from_str("").unwrap();
        assert!(!cfg.chanify.enabled);
        assert!(cfg.chanify.url.is_none());
        assert!(!cfg.pushgo.enabled);
        assert!(cfg.pushgo.url.is_none());

        let unknown: std::result::Result<NotifierConfig, _> = toml::from_str(
            r#"
            [chanify]
            enabled = false
            unexpected = 1
            "#,
        );
        assert!(unknown.is_err());
    }

    #[test]
    fn debug_redacts_all_secret_fields() {
        let cfg = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some("https://chanify.example.com/v1/sender/".to_string()),
                token: Some("chanify-secret-tok".to_string()),
            },
            email: EmailConfig {
                enabled: true,
                sender: Some("sender@example.com".to_string()),
                password: Some("email-secret-pw".to_string()),
                recipient: Some("to@example.com".to_string()),
                smtp_host: Some("smtp.example.com".to_string()),
                smtp_port: Some(465),
            },
            pushgo: PushgoConfig {
                enabled: true,
                url: Some("https://pushgo.example.com/message".to_string()),
                api_token: Some("pushgo-secret-api".to_string()),
                hex_key: Some("00112233445566778899aabbccddeeff".to_string()),
                channel_id: Some("chan1".to_string()),
                password: Some("pushgo-secret-pw".to_string()),
                icon: None,
                image: None,
            },
        };

        let dbg = format!("{cfg:?}");
        for secret in [
            "chanify-secret-tok",
            "email-secret-pw",
            "pushgo-secret-api",
            "00112233445566778899aabbccddeeff",
            "pushgo-secret-pw",
        ] {
            assert!(!dbg.contains(secret), "secret leaked in Debug: {secret}");
        }
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("chanify.example.com"));
    }

    #[test]
    fn validate_rejects_external_http_urls_and_allows_localhost() {
        let mut ext_http = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some("http://example.com/v1/sender".to_string()),
                token: Some("tok".to_string()),
            },
            ..Default::default()
        };
        assert!(ext_http.resolve_with_env(|_| None).is_err());

        let mut localhost_http = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some("http://127.0.0.1:8080/v1/sender".to_string()),
                token: Some("tok".to_string()),
            },
            ..Default::default()
        };
        assert!(localhost_http.resolve_with_env(|_| None).is_ok());

        let mut localhost_name = NotifierConfig {
            pushgo: PushgoConfig {
                enabled: true,
                url: Some("http://localhost:9000/message".to_string()),
                api_token: Some("api".to_string()),
                hex_key: Some("00112233445566778899aabbccddeeff".to_string()),
                channel_id: Some("c1".to_string()),
                password: Some("pw".to_string()),
                icon: None,
                image: None,
            },
            ..Default::default()
        };
        assert!(localhost_name.resolve_with_env(|_| None).is_ok());
    }

    #[test]
    fn validate_rejects_missing_fields_bad_hex_key_and_bad_mailbox() {
        let mut missing_chanify = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some("   ".to_string()),
                token: Some("tok".to_string()),
            },
            ..Default::default()
        };
        assert!(missing_chanify.resolve_with_env(|_| None).is_err());

        let mut bad_email = NotifierConfig {
            email: EmailConfig {
                enabled: true,
                sender: Some("not-a-mailbox".to_string()),
                password: Some("pw".to_string()),
                recipient: Some("to@example.com".to_string()),
                smtp_host: None,
                smtp_port: None,
            },
            ..Default::default()
        };
        assert!(bad_email.resolve_with_env(|_| None).is_err());

        let mut bad_hex = NotifierConfig {
            pushgo: PushgoConfig {
                enabled: true,
                url: Some("https://pushgo.example.com/message".to_string()),
                api_token: Some("api".to_string()),
                hex_key: Some("abcd".to_string()),
                channel_id: Some("c1".to_string()),
                password: Some("pw".to_string()),
                icon: None,
                image: None,
            },
            ..Default::default()
        };
        assert!(bad_hex.resolve_with_env(|_| None).is_err());
    }

    #[test]
    fn env_fallbacks_populate_missing_values_and_normalize_blanks() {
        let env_map: HashMap<&str, &str> = [
            ("CHANIFY_URL", "https://chanify.example.com/v1/sender/"),
            ("CHANIFY_TOKEN", "env-tok"),
            ("EMAIL_SENDER", "from@example.com"),
            ("EMAIL_PASSWORD", "env-epw"),
            ("EMAIL_RECIPIENT", "to@example.com"),
            ("EMAIL_SMTP_HOST", "smtp.example.com"),
            ("PUSHGO_URL", "https://pushgo.example.com/message"),
            ("PUSHGO_API_TOKEN", "env-api"),
            (
                "PUSHGO_HEX_KEY",
                "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
            ),
            ("PUSHGO_CHANNEL_ID", "env-chan"),
            ("PUSHGO_PASSWORD", "env-ppw"),
        ]
        .into_iter()
        .collect();

        let mut cfg = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some(" ".to_string()),
                token: None,
            },
            email: EmailConfig {
                enabled: true,
                sender: None,
                password: None,
                recipient: None,
                smtp_host: None,
                smtp_port: Some(465),
            },
            pushgo: PushgoConfig {
                enabled: true,
                url: None,
                api_token: None,
                hex_key: None,
                channel_id: None,
                password: None,
                icon: Some("  ".to_string()),
                image: None,
            },
        };

        cfg.resolve_with_env(|k| env_map.get(k).map(|v| (*v).to_string()))
            .unwrap();
        assert_eq!(
            cfg.chanify.url.as_deref(),
            Some("https://chanify.example.com/v1/sender/")
        );
        assert_eq!(cfg.chanify.token.as_deref(), Some("env-tok"));
        assert_eq!(cfg.email.smtp_host.as_deref(), Some("smtp.example.com"));
        assert_eq!(cfg.pushgo.channel_id.as_deref(), Some("env-chan"));
        assert!(cfg.pushgo.icon.is_none());
    }
}
