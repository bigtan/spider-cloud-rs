use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};
use spider_common::archive::ArchiveFormat;
use spider_common::fs::parse_normalized_date;
use spider_common::template::TemplateContext;
use spider_notify::config::NotifierConfig;
use spider_uploader::config::UploadersConfig;
use std::fmt;
use std::fs;
use std::path::Path;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub debug: bool,
    #[serde(default = "default_server_location")]
    pub server_location: String,
    #[serde(default)]
    pub custom_date: Option<String>,
    #[serde(default = "default_upload_mode")]
    pub upload_mode: UploadMode,
    #[serde(default)]
    pub archive: ArchiveSection,
    #[serde(default)]
    pub files: FilesSection,
    #[serde(default)]
    pub uploader: UploadersConfig,
    #[serde(default)]
    pub notifier: NotifierConfig,
    /// Resolved at load time from `date_override` / `custom_date` or today's date (`%Y%m%d`).
    #[serde(skip)]
    pub date: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ArchiveSection {
    pub source_folder: String,
    pub output: String,
    #[serde(deserialize_with = "deserialize_opt_zeroizing")]
    pub password: Option<Zeroizing<String>>,
    pub require_archive: bool,
    pub level: Option<i32>,
    pub dict_size: Option<String>,
    pub threads: Option<u32>,
    pub upload_path: Option<String>,
}

impl Default for ArchiveSection {
    fn default() -> Self {
        Self {
            source_folder: default_source_folder(),
            output: default_archive_output(),
            password: None,
            require_archive: true,
            level: None,
            dict_size: None,
            threads: None,
            upload_path: None,
        }
    }
}

impl fmt::Debug for ArchiveSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchiveSection")
            .field("source_folder", &self.source_folder)
            .field("output", &self.output)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("require_archive", &self.require_archive)
            .field("level", &self.level)
            .field("dict_size", &self.dict_size)
            .field("threads", &self.threads)
            .field("upload_path", &self.upload_path)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesSection {
    pub source_folder: String,
    pub pattern: Option<String>,
    pub upload_path: Option<String>,
}

impl Default for FilesSection {
    fn default() -> Self {
        Self {
            source_folder: default_source_folder(),
            pattern: None,
            upload_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadMode {
    Archive,
    Files,
    Hybrid,
}

impl UploadMode {
    pub fn needs_archive(self) -> bool {
        matches!(self, UploadMode::Archive | UploadMode::Hybrid)
    }

    pub fn needs_files(self) -> bool {
        matches!(self, UploadMode::Files | UploadMode::Hybrid)
    }
}

impl Config {
    pub fn from_toml(path: &str, date_override: Option<&str>) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {path}"))?;
        Self::from_toml_str(&contents, date_override)
    }

    pub fn from_toml_str(contents: &str, date_override: Option<&str>) -> Result<Self> {
        let mut config: Config = toml::from_str(contents).context("Failed to parse config file")?;
        if let Some(override_date) = date_override {
            config.custom_date = Some(override_date.to_string());
        }
        config.finalize()?;
        Ok(config)
    }

    /// Resolve the date, normalize blank strings, fill defaults, and expand
    /// `{date}` / `{server_location}` placeholders.
    pub fn finalize(&mut self) -> Result<()> {
        normalize_opt_string(&mut self.custom_date);
        normalize_opt_string(&mut self.archive.dict_size);
        normalize_opt_string(&mut self.archive.upload_path);
        normalize_opt_string(&mut self.files.pattern);
        normalize_opt_string(&mut self.files.upload_path);
        normalize_opt_string(&mut self.uploader.baidu.archive_upload_path);
        normalize_opt_string(&mut self.uploader.baidu.files_upload_path);
        normalize_opt_string(&mut self.uploader.cloud189.archive_upload_path);
        normalize_opt_string(&mut self.uploader.cloud189.files_upload_path);

        if self
            .archive
            .password
            .as_ref()
            .is_some_and(|p| p.trim().is_empty())
        {
            self.archive.password = None;
        }

        if self.server_location.trim().is_empty() {
            self.server_location = default_server_location();
        }

        let resolved_date = parse_normalized_date(self.custom_date.as_deref())?;
        self.date = resolved_date.format("%Y%m%d").to_string();

        if self.archive.source_folder.trim().is_empty() {
            self.archive.source_folder = default_source_folder();
        }
        if self.archive.output.trim().is_empty() {
            self.archive.output = default_archive_output();
        }
        if self.files.source_folder.trim().is_empty() {
            self.files.source_folder = default_source_folder();
        }

        let mut ctx = TemplateContext::with_date(self.date.as_str());
        ctx.insert("server_location", self.server_location.as_str());

        self.archive.source_folder = ctx.expand(&self.archive.source_folder);
        self.archive.output = ctx.expand(&self.archive.output);
        self.files.source_folder = ctx.expand(&self.files.source_folder);

        for value in [
            &mut self.files.pattern,
            &mut self.archive.upload_path,
            &mut self.files.upload_path,
            &mut self.uploader.baidu.archive_upload_path,
            &mut self.uploader.baidu.files_upload_path,
            &mut self.uploader.cloud189.archive_upload_path,
            &mut self.uploader.cloud189.files_upload_path,
        ]
        .into_iter()
        .flatten()
        {
            *value = ctx.expand(value);
        }

        Ok(())
    }

    /// Fail-fast validation before any archiving or uploading starts.
    pub fn validate(&mut self) -> Result<()> {
        let parsed_date = parse_normalized_date(self.custom_date.as_deref())?;
        if self.date.is_empty() {
            self.date = parsed_date.format("%Y%m%d").to_string();
        }

        if self.upload_mode.needs_archive() && !self.archive.output.trim().is_empty() {
            ArchiveFormat::from_path(Path::new(&self.archive.output))?;
        }

        self.uploader.resolve_and_validate()?;
        if !self.uploader.baidu.enabled && !self.uploader.cloud189.enabled {
            anyhow::bail!("No cloud uploader enabled");
        }

        if self.uploader.baidu.enabled {
            if self.upload_mode.needs_archive() && self.baidu_archive_dest().is_none() {
                anyhow::bail!(
                    "Baidu Pan enabled for archive mode but neither archive.upload_path nor uploader.baidu.archive_upload_path is set"
                );
            }
            if self.upload_mode.needs_files() && self.baidu_files_dest().is_none() {
                anyhow::bail!(
                    "Baidu Pan enabled for files mode but neither files.upload_path nor uploader.baidu.files_upload_path is set"
                );
            }
        }

        if self.uploader.cloud189.enabled {
            if self.upload_mode.needs_archive() && self.cloud189_archive_dest().is_none() {
                anyhow::bail!(
                    "Cloud189 enabled for archive mode but neither archive.upload_path nor uploader.cloud189.archive_upload_path is set"
                );
            }
            if self.upload_mode.needs_files() && self.cloud189_files_dest().is_none() {
                anyhow::bail!(
                    "Cloud189 enabled for files mode but neither files.upload_path nor uploader.cloud189.files_upload_path is set"
                );
            }
        }

        self.notifier.resolve_and_validate()?;
        Ok(())
    }

    pub fn baidu_archive_dest(&self) -> Option<String> {
        non_empty_opt(self.uploader.baidu.archive_upload_path.as_deref())
            .or_else(|| non_empty_opt(self.archive.upload_path.as_deref()))
    }

    pub fn baidu_files_dest(&self) -> Option<String> {
        non_empty_opt(self.uploader.baidu.files_upload_path.as_deref())
            .or_else(|| non_empty_opt(self.files.upload_path.as_deref()))
    }

    pub fn cloud189_archive_dest(&self) -> Option<String> {
        non_empty_opt(self.uploader.cloud189.archive_upload_path.as_deref())
            .or_else(|| non_empty_opt(self.archive.upload_path.as_deref()))
    }

    pub fn cloud189_files_dest(&self) -> Option<String> {
        non_empty_opt(self.uploader.cloud189.files_upload_path.as_deref())
            .or_else(|| non_empty_opt(self.files.upload_path.as_deref()))
    }
}

fn non_empty_opt(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn normalize_opt_string(opt: &mut Option<String>) {
    if opt.as_ref().is_some_and(|s| s.trim().is_empty()) {
        *opt = None;
    }
}

fn deserialize_opt_zeroizing<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Zeroizing<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    // Preserve leading/trailing whitespace on non-empty passwords (e.g. 7z `-p`).
    Ok(opt.filter(|s| !s.trim().is_empty()).map(Zeroizing::new))
}

fn default_server_location() -> String {
    "Unknown Server".to_string()
}

fn default_upload_mode() -> UploadMode {
    UploadMode::Archive
}

fn default_source_folder() -> String {
    "data/{date}/".to_string()
}

fn default_archive_output() -> String {
    "archive/{date}.tar.zst".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    fn minimal_baidu_uploader_toml() -> &'static str {
        r#"
        [archive]
        upload_path = "/apps/archive/{server_location}/{date}/"

        [uploader.baidu]
        enabled = true
        app_key = "test_key"
        app_secret = "test_secret"
        "#
    }

    #[test]
    fn finalize_expands_placeholders_and_defaults() {
        let mut config = Config::from_toml_str(
            r#"
            server_location = "ServerA"
            custom_date = "2026-06-11"
            upload_mode = "hybrid"

            [archive]
            output = "/backup/{date}.tar.zst"
            upload_path = "/cloud/{server_location}/{date}/"

            [files]
            upload_path = "/files/{server_location}/{date}/"

            [uploader.baidu]
            enabled = true
            app_key = "k"
            app_secret = "s"
            "#,
            None,
        )
        .unwrap();
        config.validate().unwrap();

        assert_eq!(config.date, "20260611");
        assert_eq!(config.archive.output, "/backup/20260611.tar.zst");
        assert_eq!(
            config.archive.upload_path.as_deref(),
            Some("/cloud/ServerA/20260611/")
        );
        assert!(config.archive.source_folder.contains("20260611"));
        assert!(config.archive.require_archive);
        assert_eq!(config.upload_mode, UploadMode::Hybrid);
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let config = Config::from_toml_str("", None).unwrap();

        assert_eq!(config.server_location, "Unknown Server");
        assert_eq!(config.upload_mode, UploadMode::Archive);
        assert!(!config.debug);
        assert_eq!(config.date, Local::now().format("%Y%m%d").to_string());
        assert_eq!(
            config.archive.output,
            format!("archive/{}.tar.zst", config.date)
        );
        assert_eq!(
            config.archive.source_folder,
            format!("data/{}/", config.date)
        );
        assert_eq!(config.files.source_folder, format!("data/{}/", config.date));
    }

    #[test]
    fn validate_fails_fast_on_malformed_date_and_missing_uploaders_and_paths() {
        let err = Config::from_toml_str(
            &format!(
                "custom_date = \"not-a-date\"\n{}",
                minimal_baidu_uploader_toml()
            ),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("date"));

        let mut no_uploaders = Config::from_toml_str("", None).unwrap();
        assert!(no_uploaders.validate().is_err());

        let mut missing_dest = Config::from_toml_str(
            r#"
            upload_mode = "archive"
            [uploader.baidu]
            enabled = true
            app_key = "k"
            app_secret = "s"
            "#,
            None,
        )
        .unwrap();
        assert!(missing_dest.validate().is_err());

        let mut missing_files_dest = Config::from_toml_str(
            r#"
            upload_mode = "hybrid"
            [archive]
            upload_path = "/archive/"
            [uploader.baidu]
            enabled = true
            app_key = "k"
            app_secret = "s"
            "#,
            None,
        )
        .unwrap();
        assert!(missing_files_dest.validate().is_err());
    }

    #[test]
    fn date_override_replaces_custom_date() {
        let mut config = Config::from_toml_str(
            &format!(
                "custom_date = \"20260101\"\n{}",
                minimal_baidu_uploader_toml()
            ),
            Some("2026-07-15"),
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.date, "20260715");
        assert_eq!(
            config.archive.upload_path.as_deref(),
            Some("/apps/archive/Unknown Server/20260715/")
        );
    }

    #[test]
    fn archive_password_preserves_whitespace_and_redacts_debug() {
        let config = Config::from_toml_str(
            r#"
            [archive]
            password = "  secret with spaces  "
            "#,
            None,
        )
        .unwrap();
        assert_eq!(
            config.archive.password.as_deref().map(String::as_str),
            Some("  secret with spaces  ")
        );
        let debug_str = format!("{:?}", config.archive);
        assert!(!debug_str.contains("secret with spaces"));
        assert!(debug_str.contains("[REDACTED]"));

        let blank_pwd = Config::from_toml_str(
            r#"
            [archive]
            password = "   "
            "#,
            None,
        )
        .unwrap();
        assert!(blank_pwd.archive.password.is_none());
    }

    #[test]
    fn deny_unknown_fields_rejects_legacy_flat_keys() {
        let err = toml::from_str::<Config>(
            r#"
            baidu_enabled = true
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn example_config_parses_against_schema() {
        let example = include_str!("../../../cloud-uploader.toml.example");
        assert!(!example.contains("estan.cn"));
        let mut config = Config::from_toml_str(example, None).unwrap();
        config.uploader.resolve_and_validate().unwrap();
        config.notifier.resolve_and_validate().unwrap();
    }
}
