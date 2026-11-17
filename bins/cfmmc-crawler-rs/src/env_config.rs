use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde::Deserialize;
use spider_common::fs::{parse_normalized_date, validate_safe_name};
use spider_notify::config::NotifierConfig;
use std::fmt;
use std::fs;
use std::path::Path;
use tracing::debug;
use url::Url;
use zeroize::Zeroize;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub accounts: Vec<String>,
    pub passwords: Vec<String>,
}

impl fmt::Debug for AccountConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountConfig")
            .field("accounts", &self.accounts)
            .field("passwords", &format!("[{} redacted]", self.passwords.len()))
            .finish()
    }
}

impl Drop for AccountConfig {
    fn drop(&mut self) {
        self.passwords.zeroize();
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaiduOcrConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default = "default_baidu_ocr_url")]
    pub url: String,
}

impl fmt::Debug for BaiduOcrConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BaiduOcrConfig")
            .field("api_key", &redact_secret(&self.api_key))
            .field("secret_key", &redact_secret(&self.secret_key))
            .field("url", &self.url)
            .finish()
    }
}

impl Drop for BaiduOcrConfig {
    fn drop(&mut self) {
        self.api_key.zeroize();
        self.secret_key.zeroize();
    }
}

impl Default for BaiduOcrConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            url: default_baidu_ocr_url(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptchaProvider {
    #[default]
    Baidu,
    Onnx,
    OnnxThenBaidu,
    BaiduThenOnnx,
}

impl CaptchaProvider {
    pub fn uses_baidu(self) -> bool {
        matches!(
            self,
            Self::Baidu | Self::OnnxThenBaidu | Self::BaiduThenOnnx
        )
    }

    pub fn uses_onnx(self) -> bool {
        matches!(self, Self::Onnx | Self::OnnxThenBaidu | Self::BaiduThenOnnx)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptchaConfig {
    #[serde(default)]
    pub provider: CaptchaProvider,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnnxCaptchaConfig {
    #[serde(default = "default_onnx_captcha_model_path")]
    pub model_path: String,
    #[serde(default = "default_onnx_captcha_vocab_path")]
    pub vocab_path: String,
    #[serde(default = "default_onnx_captcha_length")]
    pub captcha_length: usize,
}

impl Default for OnnxCaptchaConfig {
    fn default() -> Self {
        Self {
            model_path: default_onnx_captcha_model_path(),
            vocab_path: default_onnx_captcha_vocab_path(),
            captcha_length: default_onnx_captcha_length(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub custom_date: Option<String>,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    #[serde(default = "default_log_retain_days")]
    pub log_retain_days: u64,
    #[serde(default)]
    pub captcha: CaptchaConfig,
    #[serde(default)]
    pub baidu_ocr: BaiduOcrConfig,
    #[serde(default)]
    pub onnx_captcha: OnnxCaptchaConfig,
    pub account: AccountConfig,
    #[serde(default)]
    pub notifier: NotifierConfig,
}

impl Config {
    /// Apply environment variable fallbacks, validate all fields, and resolve the target date.
    pub fn resolve_and_validate(&mut self, date_override: Option<&str>) -> Result<NaiveDate> {
        self.resolve_with_env(date_override, |key| std::env::var(key).ok())
    }

    pub(crate) fn resolve_with_env(
        &mut self,
        date_override: Option<&str>,
        mut env_lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<NaiveDate> {
        self.apply_env_fallbacks(&mut env_lookup);
        self.validate_schema()?;
        self.notifier.resolve_and_validate()?;
        self.resolve_target_date(date_override)
    }

    fn apply_env_fallbacks(&mut self, env_lookup: &mut impl FnMut(&str) -> Option<String>) {
        self.baidu_ocr.api_key = self.baidu_ocr.api_key.trim().to_string();
        self.baidu_ocr.secret_key = self.baidu_ocr.secret_key.trim().to_string();
        self.baidu_ocr.url = self.baidu_ocr.url.trim().to_string();

        if self.baidu_ocr.api_key.is_empty()
            && let Some(val) = env_lookup("BAIDU_OCR_API_KEY")
            && !val.trim().is_empty()
        {
            self.baidu_ocr.api_key = val.trim().to_string();
        }
        if self.baidu_ocr.secret_key.is_empty()
            && let Some(val) = env_lookup("BAIDU_OCR_SECRET_KEY")
            && !val.trim().is_empty()
        {
            self.baidu_ocr.secret_key = val.trim().to_string();
        }
    }

    fn validate_schema(&self) -> Result<()> {
        if self.data_dir.trim().is_empty() {
            bail!("data_dir must not be empty");
        }
        if self.log_dir.trim().is_empty() {
            bail!("log_dir must not be empty");
        }

        if self.account.accounts.is_empty()
            || self.account.accounts.len() != self.account.passwords.len()
        {
            bail!(
                "Account and password counts don't match or are empty: accounts={}, passwords={}",
                self.account.accounts.len(),
                self.account.passwords.len()
            );
        }

        for account in &self.account.accounts {
            validate_safe_name(account, "account")?;
        }

        if self.onnx_captcha.captcha_length == 0 {
            bail!("onnx_captcha.captcha_length must be greater than zero");
        }

        if self.captcha.provider.uses_baidu() {
            if self.baidu_ocr.api_key.trim().is_empty() {
                bail!("baidu_ocr.api_key (or BAIDU_OCR_API_KEY) is required for Baidu OCR");
            }
            if self.baidu_ocr.secret_key.trim().is_empty() {
                bail!("baidu_ocr.secret_key (or BAIDU_OCR_SECRET_KEY) is required for Baidu OCR");
            }
            validate_https_or_local_url(&self.baidu_ocr.url, "baidu_ocr.url")?;
        }

        if self.captcha.provider.uses_onnx() {
            if self.onnx_captcha.model_path.trim().is_empty() {
                bail!("onnx_captcha.model_path must not be empty");
            }
            if self.onnx_captcha.vocab_path.trim().is_empty() {
                bail!("onnx_captcha.vocab_path must not be empty");
            }
        }

        Ok(())
    }

    fn resolve_target_date(&self, date_override: Option<&str>) -> Result<NaiveDate> {
        if let Some(raw_custom) = self.custom_date.as_deref() {
            parse_normalized_date(Some(raw_custom))
                .context("Invalid custom_date in configuration")?;
        }
        let effective = date_override
            .filter(|s| !s.trim().is_empty())
            .or(self.custom_date.as_deref());
        parse_normalized_date(effective).context("Invalid target date")
    }

    /// Validate runtime filesystem prerequisites (such as ONNX model and vocab files).
    pub fn validate_runtime_paths(&self) -> Result<()> {
        if self.captcha.provider.uses_onnx() {
            let model_path = Path::new(&self.onnx_captcha.model_path);
            if !model_path.is_file() {
                bail!(
                    "ONNX CAPTCHA model file not found: {}",
                    model_path.display()
                );
            }
            let vocab_path = Path::new(&self.onnx_captcha.vocab_path);
            if !vocab_path.is_file() {
                bail!(
                    "ONNX CAPTCHA vocab file not found: {}",
                    vocab_path.display()
                );
            }
        }
        Ok(())
    }
}

pub fn load_config(path: &str, date_override: Option<&str>) -> Result<(Config, NaiveDate)> {
    debug!("Reading CFMMC configuration from TOML: {}", path);

    let contents =
        fs::read_to_string(path).with_context(|| format!("Failed to read config file: {path}"))?;

    let mut config: Config = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file: {path}"))?;

    let target_date = config.resolve_and_validate(date_override)?;
    Ok((config, target_date))
}

pub(crate) fn redact_secret(secret: &str) -> &'static str {
    if secret.is_empty() {
        "<empty>"
    } else {
        "[REDACTED]"
    }
}

fn validate_https_or_local_url(raw_url: &str, field_name: &str) -> Result<()> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        bail!("{field_name} must not be empty");
    }
    let parsed = Url::parse(trimmed).with_context(|| format!("Invalid {field_name}"))?;
    let is_https = parsed.scheme() == "https";
    let is_local_http = parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if !is_https && !is_local_http {
        bail!("{field_name} must use https:// (or http://localhost / 127.0.0.1)");
    }
    Ok(())
}

fn default_baidu_ocr_url() -> String {
    "https://aip.baidubce.com/rest/2.0/ocr/v1/accurate_basic".to_string()
}

fn default_onnx_captcha_model_path() -> String {
    "models/model.onnx".to_string()
}

fn default_onnx_captcha_vocab_path() -> String {
    "models/vocab.txt".to_string()
}

fn default_onnx_captcha_length() -> usize {
    6
}

fn default_data_dir() -> String {
    "data".to_string()
}

fn default_log_dir() -> String {
    "logs".to_string()
}

fn default_log_retain_days() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn parse_cfg(toml_str: &str) -> Config {
        toml::from_str(toml_str).unwrap()
    }

    #[test]
    fn reject_unknown_fields_on_all_config_structs() {
        let base_bad_top = r#"
            unknown_top = 1
            [account]
            accounts = ["u1"]
            passwords = ["p1"]
        "#;
        assert!(toml::from_str::<Config>(base_bad_top).is_err());

        let bad_account = r#"
            [account]
            accounts = ["u1"]
            passwords = ["p1"]
            extra = true
        "#;
        assert!(toml::from_str::<Config>(bad_account).is_err());

        let bad_captcha = r#"
            [captcha]
            provider = "onnx"
            extra = 1
            [account]
            accounts = ["u1"]
            passwords = ["p1"]
        "#;
        assert!(toml::from_str::<Config>(bad_captcha).is_err());

        let bad_baidu = r#"
            [baidu_ocr]
            extra = "x"
            [account]
            accounts = ["u1"]
            passwords = ["p1"]
        "#;
        assert!(toml::from_str::<Config>(bad_baidu).is_err());

        let bad_onnx = r#"
            [onnx_captcha]
            extra = "x"
            [account]
            accounts = ["u1"]
            passwords = ["p1"]
        "#;
        assert!(toml::from_str::<Config>(bad_onnx).is_err());
    }

    #[test]
    fn validate_rejects_mismatched_accounts_and_passwords() {
        let mut cfg = parse_cfg(
            r#"
            [captcha]
            provider = "onnx"
            [account]
            accounts = ["u1", "u2"]
            passwords = ["p1"]
            "#,
        );
        assert!(cfg.resolve_and_validate(None).is_err());
    }

    #[test]
    fn validate_rejects_path_traversal_account_names() {
        for bad_account in [
            "../evil", "a/b", r"a\b", "..", ".", "-flag", " ", "bad:name",
        ] {
            let mut cfg = parse_cfg(&format!(
                r#"
                [captcha]
                provider = "onnx"
                [account]
                accounts = [{bad_account:?}]
                passwords = ["secret"]
                "#
            ));
            let err = cfg.resolve_and_validate(None).unwrap_err();
            assert!(
                err.to_string().contains("account"),
                "expected account validation error for {bad_account:?}, got {err}"
            );
        }
    }

    #[test]
    fn date_override_takes_precedence_over_custom_date() {
        let mut cfg = parse_cfg(
            r#"
            custom_date = "2025-12-31"
            [captcha]
            provider = "onnx"
            [account]
            accounts = ["acc_01"]
            passwords = ["pass_01"]
            "#,
        );
        let from_toml = cfg.resolve_and_validate(None).unwrap();
        assert_eq!(from_toml, NaiveDate::from_ymd_opt(2025, 12, 31).unwrap());

        let from_cli = cfg.resolve_and_validate(Some("20260115")).unwrap();
        assert_eq!(from_cli, NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
    }

    #[test]
    fn invalid_custom_date_is_rejected() {
        let mut cfg = parse_cfg(
            r#"
            custom_date = "2026-99-99"
            [captcha]
            provider = "onnx"
            [account]
            accounts = ["acc_01"]
            passwords = ["pass_01"]
            "#,
        );
        assert!(cfg.resolve_and_validate(None).is_err());
        // Invalid config custom_date is caught even if CLI override is passed
        assert!(cfg.resolve_and_validate(Some("2026-01-15")).is_err());
    }

    #[test]
    fn debug_formatting_redacts_secrets() {
        let cfg = parse_cfg(
            r#"
            [captcha]
            provider = "baidu"
            [baidu_ocr]
            api_key = "super-secret-api-key"
            secret_key = "super-secret-key"
            [account]
            accounts = ["acc_01"]
            passwords = ["top-secret-password"]
            "#,
        );
        let rendered = format!("{cfg:?}");
        assert!(rendered.contains("acc_01"));
        assert!(!rendered.contains("top-secret-password"));
        assert!(!rendered.contains("super-secret-api-key"));
        assert!(!rendered.contains("super-secret-key"));
    }

    #[test]
    fn defaults_are_applied_for_dirs_and_retention() {
        let mut cfg = parse_cfg(
            r#"
            [captcha]
            provider = "onnx"
            [account]
            accounts = ["acc_01"]
            passwords = ["pass_01"]
            "#,
        );
        cfg.resolve_and_validate(None).unwrap();
        assert_eq!(cfg.data_dir, "data");
        assert_eq!(cfg.log_dir, "logs");
        assert_eq!(cfg.log_retain_days, 30);
    }

    #[test]
    fn baidu_ocr_env_fallback_populates_empty_credentials() {
        let mut cfg = parse_cfg(
            r#"
            [captcha]
            provider = "baidu"
            [account]
            accounts = ["acc_01"]
            passwords = ["pass_01"]
            "#,
        );
        cfg.resolve_with_env(None, |key| match key {
            "BAIDU_OCR_API_KEY" => Some("env-api-key".to_string()),
            "BAIDU_OCR_SECRET_KEY" => Some("env-secret-key".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.baidu_ocr.api_key, "env-api-key");
        assert_eq!(cfg.baidu_ocr.secret_key, "env-secret-key");
    }

    #[test]
    fn validate_runtime_paths_checks_onnx_files() {
        let cfg = parse_cfg(
            r#"
            [captcha]
            provider = "onnx"
            [onnx_captcha]
            model_path = "/nonexistent/model.onnx"
            vocab_path = "/nonexistent/vocab.txt"
            [account]
            accounts = ["acc_01"]
            passwords = ["pass_01"]
            "#,
        );
        assert!(cfg.validate_runtime_paths().is_err());
    }
}
