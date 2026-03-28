use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use std::fs;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct Config {
    pub debug: bool,
    pub server_location: String,
    pub date: String,
    pub upload_mode: UploadMode,
    pub archive_source_folder: String,
    pub archive_output: String,
    pub archive_password: Option<String>,
    pub require_archive: bool,
    pub files_source_folder: String,
    pub file_pattern: Option<String>,
    pub archive_upload_path: Option<String>,
    pub files_upload_path: Option<String>,
    pub chanify_enabled: bool,
    pub chanify_token: Option<String>,
    pub chanify_url: String,
    pub email_enabled: bool,
    pub email_sender: Option<String>,
    pub email_password: Option<String>,
    pub email_recipient: Option<String>,
    pub pushgo_enabled: bool,
    pub pushgo_api_token: Option<String>,
    pub pushgo_hex_key: Option<String>,
    pub pushgo_url: String,
    pub pushgo_channel_id: Option<String>,
    pub pushgo_password: Option<String>,
    pub pushgo_icon: Option<String>,
    pub pushgo_image: Option<String>,
    pub baidu_enabled: bool,
    pub baidu_app_key: Option<String>,
    pub baidu_app_secret: Option<String>,
    pub baidu_archive_upload_path: Option<String>,
    pub baidu_files_upload_path: Option<String>,
    pub cloud189_enabled: bool,
    pub cloud189_config_path: Option<String>,
    pub cloud189_username: Option<String>,
    pub cloud189_password: Option<String>,
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

#[derive(Debug, Deserialize)]
struct RawConfig {
    debug: Option<bool>,
    server_location: Option<String>,
    custom_date: Option<String>,
    upload_mode: Option<UploadMode>,
    archive_source_folder: Option<String>,
    archive_output: Option<String>,
    archive_password: Option<String>,
    require_archive: Option<bool>,
    files_source_folder: Option<String>,
    file_pattern: Option<String>,
    archive_upload_path: Option<String>,
    files_upload_path: Option<String>,
    chanify_enabled: Option<bool>,
    chanify_token: Option<String>,
    chanify_url: Option<String>,
    email_enabled: Option<bool>,
    email_sender: Option<String>,
    email_password: Option<String>,
    email_recipient: Option<String>,
    pushgo_enabled: Option<bool>,
    pushgo_api_token: Option<String>,
    pushgo_hex_key: Option<String>,
    pushgo_url: Option<String>,
    pushgo_channel_id: Option<String>,
    pushgo_password: Option<String>,
    pushgo_icon: Option<String>,
    pushgo_image: Option<String>,
    baidu_enabled: Option<bool>,
    baidu_app_key: Option<String>,
    baidu_app_secret: Option<String>,
    baidu_archive_upload_path: Option<String>,
    baidu_files_upload_path: Option<String>,
    cloud189_enabled: Option<bool>,
    cloud189_config_path: Option<String>,
    cloud189_username: Option<String>,
    cloud189_password: Option<String>,
    cloud189_qr_login: Option<bool>,
    cloud189_archive_upload_path: Option<String>,
    cloud189_files_upload_path: Option<String>,
}

impl Config {
    pub fn from_toml(path: &str) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;
        let raw: RawConfig = toml::from_str(&contents).context("Failed to parse config file")?;

        let debug = raw.debug.unwrap_or(false);
        let server_location = raw
            .server_location
            .unwrap_or_else(|| "Unknown Server".to_string());
        let date = parse_date(raw.custom_date.as_deref())?;
        let upload_mode = raw.upload_mode.unwrap_or(UploadMode::Archive);

        let archive_source_folder = raw
            .archive_source_folder
            .unwrap_or_else(|| format!("/home/tanlei678/cfmdc/his_quota_dir/{}/", date));
        let archive_source_folder = replace_date_placeholder(&archive_source_folder, &date);

        let archive_output = raw
            .archive_output
            .unwrap_or_else(|| format!("/home/tanlei678/archive/{}.7z", date));
        let archive_output = replace_date_placeholder(&archive_output, &date);

        let archive_password = raw.archive_password;
        let require_archive = raw.require_archive.unwrap_or(true);

        let files_source_folder = raw
            .files_source_folder
            .unwrap_or_else(|| format!("/home/tanlei678/cfmdc/his_quota_dir/{}/", date));
        let files_source_folder = replace_date_placeholder(&files_source_folder, &date);

        let file_pattern = raw
            .file_pattern
            .map(|p| replace_date_placeholder(&p, &date));

        let archive_upload_path = raw.archive_upload_path.map(|p| {
            let p = replace_date_placeholder(&p, &date);
            replace_server_location_placeholder(&p, &server_location)
        });
        let files_upload_path = raw.files_upload_path.map(|p| {
            let p = replace_date_placeholder(&p, &date);
            replace_server_location_placeholder(&p, &server_location)
        });

        let chanify_enabled = raw.chanify_enabled.unwrap_or(false);
        let chanify_token = raw.chanify_token;
        let chanify_url = raw
            .chanify_url
            .unwrap_or_else(|| "https://chanify.estan.cn/v1/sender/".to_string());

        let email_enabled = raw.email_enabled.unwrap_or(false);
        let email_sender = raw.email_sender;
        let email_password = raw.email_password;
        let email_recipient = raw.email_recipient;

        let pushgo_enabled = raw.pushgo_enabled.unwrap_or(false);
        let pushgo_api_token = raw.pushgo_api_token;
        let pushgo_hex_key = raw.pushgo_hex_key;
        let pushgo_url = raw
            .pushgo_url
            .unwrap_or_else(|| "https://pushgo.estan.cn/message".to_string());
        let pushgo_channel_id = raw.pushgo_channel_id;
        let pushgo_password = raw.pushgo_password;
        let pushgo_icon = raw.pushgo_icon;
        let pushgo_image = raw.pushgo_image;

        let baidu_enabled = raw.baidu_enabled.unwrap_or(false);
        let baidu_app_key = raw.baidu_app_key;
        let baidu_app_secret = raw.baidu_app_secret;
        let baidu_archive_upload_path = raw.baidu_archive_upload_path.map(|p| {
            let p = replace_date_placeholder(&p, &date);
            replace_server_location_placeholder(&p, &server_location)
        });
        let baidu_files_upload_path = raw.baidu_files_upload_path.map(|p| {
            let p = replace_date_placeholder(&p, &date);
            replace_server_location_placeholder(&p, &server_location)
        });

        let cloud189_enabled = raw.cloud189_enabled.unwrap_or(false);
        let cloud189_config_path = raw.cloud189_config_path;
        let cloud189_username = raw.cloud189_username;
        let cloud189_password = raw.cloud189_password;
        let cloud189_qr_login = raw.cloud189_qr_login.unwrap_or(false);
        let cloud189_archive_upload_path = raw.cloud189_archive_upload_path.map(|p| {
            let p = replace_date_placeholder(&p, &date);
            replace_server_location_placeholder(&p, &server_location)
        });
        let cloud189_files_upload_path = raw.cloud189_files_upload_path.map(|p| {
            let p = replace_date_placeholder(&p, &date);
            replace_server_location_placeholder(&p, &server_location)
        });

        Ok(Config {
            debug,
            server_location,
            date,
            upload_mode,
            archive_source_folder,
            archive_output,
            archive_password,
            require_archive,
            files_source_folder,
            file_pattern,
            archive_upload_path,
            files_upload_path,
            chanify_enabled,
            chanify_token,
            chanify_url,
            email_enabled,
            email_sender,
            email_password,
            email_recipient,
            pushgo_enabled,
            pushgo_api_token,
            pushgo_hex_key,
            pushgo_url,
            pushgo_channel_id,
            pushgo_password,
            pushgo_icon,
            pushgo_image,
            baidu_enabled,
            baidu_app_key,
            baidu_app_secret,
            baidu_archive_upload_path,
            baidu_files_upload_path,
            cloud189_enabled,
            cloud189_config_path,
            cloud189_username,
            cloud189_password,
            cloud189_qr_login,
            cloud189_archive_upload_path,
            cloud189_files_upload_path,
        })
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

fn replace_date_placeholder(s: &str, date: &str) -> String {
    s.replace("{date}", date)
}

fn replace_server_location_placeholder(s: &str, server_location: &str) -> String {
    s.replace("{server_location}", server_location)
}
