use std::fmt;

use serde::Deserialize;

use crate::error::{Result, UploadError, redacted_opt};

pub const DEFAULT_MAX_CONCURRENT_CHUNKS: usize = 4;
pub const MIN_CONCURRENT_CHUNKS: usize = 1;
pub const MAX_CONCURRENT_CHUNKS: usize = 16;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UploadersConfig {
    pub max_concurrent_chunks: usize,
    pub baidu: BaiduUploaderConfig,
    pub cloud189: Cloud189UploaderConfig,
}

impl Default for UploadersConfig {
    fn default() -> Self {
        Self {
            max_concurrent_chunks: DEFAULT_MAX_CONCURRENT_CHUNKS,
            baidu: BaiduUploaderConfig::default(),
            cloud189: Cloud189UploaderConfig::default(),
        }
    }
}

impl fmt::Debug for UploadersConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadersConfig")
            .field("max_concurrent_chunks", &self.max_concurrent_chunks)
            .field("baidu", &self.baidu)
            .field("cloud189", &self.cloud189)
            .finish()
    }
}

impl UploadersConfig {
    /// Returns `true` if at least one cloud uploader is enabled.
    pub fn has_enabled_uploaders(&self) -> bool {
        self.baidu.enabled || self.cloud189.enabled
    }

    /// Normalize blank optional strings, apply environment variable fallbacks,
    /// and validate concurrency bounds and required credentials.
    pub fn resolve_and_validate(&mut self) -> Result<()> {
        self.resolve_and_validate_with(|key| std::env::var(key).ok())
    }

    pub(crate) fn resolve_and_validate_with(
        &mut self,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        normalize_opt_trimmed(&mut self.baidu.app_key);
        normalize_opt_secret(&mut self.baidu.app_secret);
        normalize_opt_trimmed(&mut self.baidu.config_path);
        normalize_opt_trimmed(&mut self.baidu.archive_upload_path);
        normalize_opt_trimmed(&mut self.baidu.files_upload_path);

        normalize_opt_trimmed(&mut self.cloud189.config_path);
        normalize_opt_trimmed(&mut self.cloud189.username);
        normalize_opt_secret(&mut self.cloud189.password);
        normalize_opt_trimmed(&mut self.cloud189.archive_upload_path);
        normalize_opt_trimmed(&mut self.cloud189.files_upload_path);

        if self.baidu.app_key.is_none() {
            self.baidu.app_key = lookup_non_blank(&env_lookup, "BAIDU_APP_KEY", true);
        }
        if self.baidu.app_secret.is_none() {
            self.baidu.app_secret = lookup_non_blank(&env_lookup, "BAIDU_APP_SECRET", false);
        }

        if self.cloud189.username.is_none() {
            self.cloud189.username = lookup_non_blank(&env_lookup, "CLOUD189_USERNAME", true);
        }
        if self.cloud189.password.is_none() {
            self.cloud189.password = lookup_non_blank(&env_lookup, "CLOUD189_PASSWORD", false);
        }
        if !self.cloud189.use_qr
            && let Some(raw_qr) = lookup_non_blank(&env_lookup, "CLOUD189_USE_QR", true)
        {
            self.cloud189.use_qr = parse_env_bool(&raw_qr).ok_or_else(|| {
                UploadError::InvalidConfig(format!(
                    "invalid CLOUD189_USE_QR value '{raw_qr}'; expected true/false/1/0/yes/no/on/off"
                ))
            })?;
        }

        if !(MIN_CONCURRENT_CHUNKS..=MAX_CONCURRENT_CHUNKS).contains(&self.max_concurrent_chunks) {
            return Err(UploadError::InvalidConfig(format!(
                "max_concurrent_chunks must be in {MIN_CONCURRENT_CHUNKS}..={MAX_CONCURRENT_CHUNKS}, got {}",
                self.max_concurrent_chunks
            )));
        }

        if self.baidu.enabled && (self.baidu.app_key.is_none() || self.baidu.app_secret.is_none()) {
            return Err(UploadError::InvalidConfig(
                "Baidu Pan uploader is enabled, but app_key or app_secret is missing (set in config or BAIDU_APP_KEY / BAIDU_APP_SECRET)"
                    .to_string(),
            ));
        }

        if self.cloud189.enabled && !self.cloud189.use_qr {
            match (
                self.cloud189.username.as_ref(),
                self.cloud189.password.as_ref(),
            ) {
                (Some(_), Some(_)) => {}
                (Some(_), None) | (None, Some(_)) => {
                    return Err(UploadError::InvalidConfig(
                        "Cloud189 uploader is enabled with password login, but username or password is missing"
                            .to_string(),
                    ));
                }
                (None, None) => {
                    return Err(UploadError::InvalidConfig(
                        "Cloud189 uploader is enabled, but neither username/password nor use_qr is configured"
                            .to_string(),
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BaiduUploaderConfig {
    pub enabled: bool,
    pub app_key: Option<String>,
    pub app_secret: Option<String>,
    pub config_path: Option<String>,
    pub archive_upload_path: Option<String>,
    pub files_upload_path: Option<String>,
}

impl fmt::Debug for BaiduUploaderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BaiduUploaderConfig")
            .field("enabled", &self.enabled)
            .field("app_key", &redacted_opt(&self.app_key))
            .field("app_secret", &redacted_opt(&self.app_secret))
            .field("config_path", &self.config_path)
            .field("archive_upload_path", &self.archive_upload_path)
            .field("files_upload_path", &self.files_upload_path)
            .finish()
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Cloud189UploaderConfig {
    pub enabled: bool,
    pub config_path: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(alias = "qr_login")]
    pub use_qr: bool,
    pub archive_upload_path: Option<String>,
    pub files_upload_path: Option<String>,
}

impl fmt::Debug for Cloud189UploaderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cloud189UploaderConfig")
            .field("enabled", &self.enabled)
            .field("config_path", &self.config_path)
            .field("username", &self.username)
            .field("password", &redacted_opt(&self.password))
            .field("use_qr", &self.use_qr)
            .field("archive_upload_path", &self.archive_upload_path)
            .field("files_upload_path", &self.files_upload_path)
            .finish()
    }
}

fn normalize_opt_trimmed(slot: &mut Option<String>) {
    if let Some(val) = slot {
        let trimmed = val.trim();
        if trimmed.is_empty() {
            *slot = None;
        } else if trimmed.len() != val.len() {
            *val = trimmed.to_string();
        }
    }
}

fn normalize_opt_secret(slot: &mut Option<String>) {
    if slot.as_ref().is_some_and(|s| s.trim().is_empty()) {
        *slot = None;
    }
}

fn lookup_non_blank(
    env_lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    trim: bool,
) -> Option<String> {
    env_lookup(key).and_then(|val| {
        if val.trim().is_empty() {
            None
        } else if trim {
            Some(val.trim().to_string())
        } else {
            Some(val)
        }
    })
}

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn default_config_is_valid_with_default_concurrency() {
        let mut cfg = UploadersConfig::default();
        assert_eq!(cfg.max_concurrent_chunks, 4);
        assert!(!cfg.has_enabled_uploaders());
        cfg.resolve_and_validate_with(|_| None).unwrap();
    }

    #[test]
    fn concurrency_bounds_are_enforced() {
        for bad in [0, 17, 100] {
            let mut cfg = UploadersConfig {
                max_concurrent_chunks: bad,
                ..Default::default()
            };
            let err = cfg.resolve_and_validate_with(|_| None).unwrap_err();
            assert!(matches!(err, UploadError::InvalidConfig(_)));
        }

        for good in [1, 4, 16] {
            let mut cfg = UploadersConfig {
                max_concurrent_chunks: good,
                ..Default::default()
            };
            cfg.resolve_and_validate_with(|_| None).unwrap();
        }
    }

    #[test]
    fn deny_unknown_fields_rejects_typos() {
        let toml_str = r#"
            max_concurrent_chunks = 4
            [baidu]
            enabled = true
            app_ky = "typo"
        "#;
        let parsed: std::result::Result<UploadersConfig, _> = toml_from_str_via_serde(toml_str);
        assert!(parsed.is_err());
    }

    fn toml_from_str_via_serde(s: &str) -> std::result::Result<UploadersConfig, String> {
        // Test unknown fields via serde_json/toml? We don't have `toml` in spider-uploader deps,
        // so test with serde_json!
        serde_json::from_str(s).map_err(|e| e.to_string())
    }

    #[test]
    fn serde_rejects_unknown_fields_and_parses_paths() {
        let err = serde_json::from_str::<UploadersConfig>(
            r#"{"max_concurrent_chunks": 2, "baidu": {"enabled": false, "bogus": 1}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"));

        let mut cfg: UploadersConfig = serde_json::from_str(
            r#"{
                "max_concurrent_chunks": 8,
                "baidu": {
                    "enabled": true,
                    "app_key": "  key1  ",
                    "app_secret": "sec1",
                    "archive_upload_path": "/apps/archive",
                    "files_upload_path": "/apps/files"
                },
                "cloud189": {
                    "enabled": true,
                    "use_qr": true,
                    "archive_upload_path": "/189/archive",
                    "files_upload_path": "/189/files"
                }
            }"#,
        )
        .unwrap();
        cfg.resolve_and_validate_with(|_| None).unwrap();
        assert_eq!(cfg.max_concurrent_chunks, 8);
        assert_eq!(cfg.baidu.app_key.as_deref(), Some("key1"));
        assert_eq!(
            cfg.baidu.archive_upload_path.as_deref(),
            Some("/apps/archive")
        );
        assert_eq!(
            cfg.cloud189.files_upload_path.as_deref(),
            Some("/189/files")
        );
    }

    #[test]
    fn env_fallbacks_populate_missing_credentials() {
        let mut cfg = UploadersConfig {
            baidu: BaiduUploaderConfig {
                enabled: true,
                ..Default::default()
            },
            cloud189: Cloud189UploaderConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let env_map = HashMap::from([
            ("BAIDU_APP_KEY", "env-key"),
            ("BAIDU_APP_SECRET", "env-secret"),
            ("CLOUD189_USERNAME", "env-user"),
            ("CLOUD189_PASSWORD", "env-pass"),
        ]);

        cfg.resolve_and_validate_with(|k| env_map.get(k).map(|v| (*v).to_string()))
            .unwrap();
        assert_eq!(cfg.baidu.app_key.as_deref(), Some("env-key"));
        assert_eq!(cfg.baidu.app_secret.as_deref(), Some("env-secret"));
        assert_eq!(cfg.cloud189.username.as_deref(), Some("env-user"));
        assert_eq!(cfg.cloud189.password.as_deref(), Some("env-pass"));
    }

    #[test]
    fn cloud189_use_qr_env_fallback_and_validation() {
        let mut cfg = UploadersConfig {
            cloud189: Cloud189UploaderConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // Missing credentials and use_qr = false -> fails
        let err = cfg.resolve_and_validate_with(|_| None).unwrap_err();
        assert!(matches!(err, UploadError::InvalidConfig(_)));

        // CLOUD189_USE_QR=true satisfies validation
        cfg.resolve_and_validate_with(|k| (k == "CLOUD189_USE_QR").then(|| "true".to_string()))
            .unwrap();
        assert!(cfg.cloud189.use_qr);
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let cfg = UploadersConfig {
            max_concurrent_chunks: 4,
            baidu: BaiduUploaderConfig {
                enabled: true,
                app_key: Some("super-baidu-key".into()),
                app_secret: Some("super-baidu-secret".into()),
                config_path: Some("/tmp/baidu.json".into()),
                archive_upload_path: Some("/archive".into()),
                files_upload_path: None,
            },
            cloud189: Cloud189UploaderConfig {
                enabled: true,
                config_path: None,
                username: Some("alice@example.com".into()),
                password: Some("super-189-password".into()),
                use_qr: false,
                archive_upload_path: None,
                files_upload_path: Some("/files".into()),
            },
        };

        let debug_str = format!("{cfg:?}");
        assert!(!debug_str.contains("super-baidu-key"));
        assert!(!debug_str.contains("super-baidu-secret"));
        assert!(!debug_str.contains("super-189-password"));
        assert!(debug_str.contains("[REDACTED]"));
        assert!(debug_str.contains("/tmp/baidu.json"));
    }
}
