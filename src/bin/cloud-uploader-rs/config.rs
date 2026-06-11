use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use spider_cloud_rs::uploader::UploadContext;
use std::fs;
use tracing::warn;

/// Deserialized directly from the TOML file (keys unchanged); the
/// date- and placeholder-dependent fields are resolved in `finalize`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub debug: bool,
    #[serde(default = "default_server_location")]
    pub server_location: String,
    #[serde(default)]
    custom_date: Option<String>,
    /// Resolved at load time from `custom_date` or today's date.
    #[serde(skip)]
    pub date: String,
    #[serde(default = "default_upload_mode")]
    pub upload_mode: UploadMode,
    #[serde(default)]
    pub archive_source_folder: String,
    #[serde(default)]
    pub archive_output: String,
    pub archive_password: Option<String>,
    #[serde(default = "default_true")]
    pub require_archive: bool,
    #[serde(default)]
    pub files_source_folder: String,
    pub file_pattern: Option<String>,
    pub archive_upload_path: Option<String>,
    pub files_upload_path: Option<String>,
    #[serde(default)]
    pub chanify_enabled: bool,
    pub chanify_token: Option<String>,
    #[serde(default = "default_chanify_url")]
    pub chanify_url: String,
    #[serde(default)]
    pub email_enabled: bool,
    pub email_sender: Option<String>,
    pub email_password: Option<String>,
    pub email_recipient: Option<String>,
    #[serde(default)]
    pub pushgo_enabled: bool,
    pub pushgo_api_token: Option<String>,
    pub pushgo_hex_key: Option<String>,
    #[serde(default = "default_pushgo_url")]
    pub pushgo_url: String,
    pub pushgo_channel_id: Option<String>,
    pub pushgo_password: Option<String>,
    pub pushgo_icon: Option<String>,
    pub pushgo_image: Option<String>,
    #[serde(default)]
    pub baidu_enabled: bool,
    pub baidu_app_key: Option<String>,
    pub baidu_app_secret: Option<String>,
    pub baidu_config_path: Option<String>,
    pub baidu_archive_upload_path: Option<String>,
    pub baidu_files_upload_path: Option<String>,
    #[serde(default)]
    pub cloud189_enabled: bool,
    pub cloud189_config_path: Option<String>,
    pub cloud189_username: Option<String>,
    pub cloud189_password: Option<String>,
    #[serde(default)]
    pub cloud189_qr_login: bool,
    pub cloud189_archive_upload_path: Option<String>,
    pub cloud189_files_upload_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadMode {
    Archive,
    Files,
    Hybrid,
}

impl UploadMode {
    pub fn needs_archive(&self) -> bool {
        matches!(self, UploadMode::Archive | UploadMode::Hybrid)
    }

    pub fn needs_files(&self) -> bool {
        matches!(self, UploadMode::Files | UploadMode::Hybrid)
    }
}

impl Config {
    pub fn from_toml(path: &str) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;
        let mut config: Config =
            toml::from_str(&contents).context("Failed to parse config file")?;
        config.finalize()?;
        Ok(config)
    }

    /// Resolve the date, fill date-dependent defaults and expand
    /// `{date}` / `{server_location}` placeholders.
    fn finalize(&mut self) -> Result<()> {
        self.date = parse_date(self.custom_date.as_deref())?;

        if self.archive_source_folder.is_empty() {
            self.archive_source_folder =
                format!("/home/tanlei678/cfmdc/his_quota_dir/{}/", self.date);
        }
        if self.archive_output.is_empty() {
            self.archive_output = format!("/home/tanlei678/archive/{}.7z", self.date);
        }
        if self.files_source_folder.is_empty() {
            self.files_source_folder =
                format!("/home/tanlei678/cfmdc/his_quota_dir/{}/", self.date);
        }

        let mut ctx = UploadContext::with_date(self.date.as_str());
        ctx.insert("server_location", self.server_location.as_str());

        self.archive_source_folder = ctx.expand(&self.archive_source_folder);
        self.archive_output = ctx.expand(&self.archive_output);
        self.files_source_folder = ctx.expand(&self.files_source_folder);
        for value in [
            &mut self.file_pattern,
            &mut self.archive_upload_path,
            &mut self.files_upload_path,
            &mut self.baidu_archive_upload_path,
            &mut self.baidu_files_upload_path,
            &mut self.cloud189_archive_upload_path,
            &mut self.cloud189_files_upload_path,
        ]
        .into_iter()
        .flatten()
        {
            *value = ctx.expand(value);
        }

        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.chanify_enabled && self.chanify_token.is_none() {
            warn!("Chanify enabled but CHANIFY_TOKEN not set");
        }

        if self.email_enabled
            && (self.email_sender.is_none()
                || self.email_password.is_none()
                || self.email_recipient.is_none())
        {
            warn!("Email enabled but missing EMAIL_SENDER, EMAIL_PASSWORD, or EMAIL_RECIPIENT");
        }

        if self.pushgo_enabled
            && (self.pushgo_api_token.is_none()
                || self.pushgo_hex_key.is_none()
                || self.pushgo_channel_id.is_none()
                || self.pushgo_password.is_none())
        {
            warn!(
                "Pushgo enabled but missing PUSHGO_API_TOKEN, PUSHGO_HEX_KEY, PUSHGO_CHANNEL_ID, or PUSHGO_PASSWORD"
            );
        }

        if self.baidu_enabled && (self.baidu_app_key.is_none() || self.baidu_app_secret.is_none()) {
            warn!("Baidu Pan enabled but missing BAIDU_APP_KEY or BAIDU_APP_SECRET");
        }

        if self.baidu_enabled
            && self.upload_mode.needs_archive()
            && self.archive_upload_path.is_none()
            && self.baidu_archive_upload_path.is_none()
        {
            warn!("Baidu Pan enabled for archive mode but ARCHIVE_UPLOAD_PATH not set");
        }

        if self.baidu_enabled
            && self.upload_mode.needs_files()
            && self.files_upload_path.is_none()
            && self.baidu_files_upload_path.is_none()
        {
            warn!("Baidu Pan enabled for files mode but FILES_UPLOAD_PATH not set");
        }

        if self.cloud189_enabled
            && self.upload_mode.needs_archive()
            && self.archive_upload_path.is_none()
            && self.cloud189_archive_upload_path.is_none()
        {
            warn!("Cloud189 enabled for archive mode but ARCHIVE_UPLOAD_PATH not set");
        }

        if self.cloud189_enabled
            && self.upload_mode.needs_files()
            && self.files_upload_path.is_none()
            && self.cloud189_files_upload_path.is_none()
        {
            warn!("Cloud189 enabled for files mode but FILES_UPLOAD_PATH not set");
        }

        Ok(())
    }
}

fn parse_date(custom_date: Option<&str>) -> Result<String> {
    if let Some(custom_date) = custom_date {
        if custom_date.len() == 8
            && let Ok(date) = NaiveDate::parse_from_str(custom_date, "%Y%m%d")
        {
            return Ok(date.format("%Y%m%d").to_string());
        }

        if let Ok(date) = NaiveDate::parse_from_str(custom_date, "%Y-%m-%d") {
            return Ok(date.format("%Y%m%d").to_string());
        }

        warn!(
            "Invalid CUSTOM_DATE format: {}. Using today's date",
            custom_date
        );
    }

    Ok(Utc::now().format("%Y%m%d").to_string())
}

fn default_server_location() -> String {
    "Unknown Server".to_string()
}

fn default_upload_mode() -> UploadMode {
    UploadMode::Archive
}

fn default_true() -> bool {
    true
}

fn default_chanify_url() -> String {
    "https://chanify.estan.cn/v1/sender/".to_string()
}

fn default_pushgo_url() -> String {
    "https://pushgo.estan.cn/message".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_expands_placeholders_and_defaults() {
        let mut config: Config = toml::from_str(
            r#"
            server_location = "ServerA"
            custom_date = "2026-06-11"
            upload_mode = "hybrid"
            archive_output = "/backup/{date}.7z"
            archive_upload_path = "/cloud/{server_location}/{date}/"
            "#,
        )
        .unwrap();
        config.finalize().unwrap();

        assert_eq!(config.date, "20260611");
        assert_eq!(config.archive_output, "/backup/20260611.7z");
        assert_eq!(
            config.archive_upload_path.as_deref(),
            Some("/cloud/ServerA/20260611/")
        );
        // 未配置的字段落到带日期的默认值
        assert!(config.archive_source_folder.contains("20260611"));
        assert!(config.require_archive);
        assert_eq!(config.upload_mode, UploadMode::Hybrid);
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let mut config: Config = toml::from_str("").unwrap();
        config.finalize().unwrap();

        assert_eq!(config.server_location, "Unknown Server");
        assert_eq!(config.upload_mode, UploadMode::Archive);
        assert!(!config.debug);
        assert!(config.chanify_url.contains("chanify"));
    }
}
