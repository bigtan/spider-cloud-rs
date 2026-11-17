use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};
use spider_common::fs::{parse_normalized_date, validate_safe_name};
use spider_common::template::TemplateContext;
use spider_notify::config::NotifierConfig;
use spider_uploader::config::UploadersConfig;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub debug: bool,
    #[serde(default = "default_server_location")]
    pub server_location: String,
    #[serde(default)]
    pub custom_date: Option<String>,
    #[serde(
        default = "default_work_dir",
        deserialize_with = "deserialize_work_dir"
    )]
    pub work_dir: PathBuf,
    #[serde(default = "default_compression_level")]
    pub compression_level: i32,
    #[serde(default)]
    pub threads: Option<u32>,
    #[serde(default)]
    pub uploader: UploadersConfig,
    #[serde(default)]
    pub notifier: NotifierConfig,
    pub backups: Vec<BackupItem>,
    /// Resolved at load time from `date_override` / `custom_date` or today's date (`%Y%m%d`).
    #[serde(skip)]
    pub date: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupItem {
    pub archive_name: String,
    #[serde(default)]
    pub source_dir: Option<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub command_workdir: Option<String>,
    #[serde(default)]
    pub keep_command_source: Option<bool>,
    pub remote_dir: String,
    #[serde(default)]
    pub keep_archive: Option<bool>,
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

    pub fn finalize(&mut self) -> Result<()> {
        normalize_opt_string(&mut self.custom_date);
        if self.server_location.trim().is_empty() {
            self.server_location = default_server_location();
        }

        let resolved_date = parse_normalized_date(self.custom_date.as_deref())?;
        self.date = resolved_date.format("%Y%m%d").to_string();

        if self.work_dir.as_os_str().is_empty() {
            self.work_dir = default_work_dir();
        } else if let Some(raw_work_dir) = self.work_dir.to_str() {
            let mut ctx = TemplateContext::with_date(self.date.as_str());
            ctx.insert("server_location", self.server_location.as_str());
            let expanded = ctx.expand(raw_work_dir);
            if expanded.trim().is_empty() {
                self.work_dir = default_work_dir();
            } else {
                self.work_dir = PathBuf::from(expanded);
            }
        }

        for item in &mut self.backups {
            normalize_opt_string(&mut item.source_dir);
            normalize_opt_string(&mut item.source_path);
            normalize_opt_string(&mut item.command);
            normalize_opt_string(&mut item.command_workdir);
        }

        Ok(())
    }

    /// Fail-fast validation before running commands, archiving, or uploading.
    pub fn validate(&mut self) -> Result<()> {
        let parsed_date = parse_normalized_date(self.custom_date.as_deref())?;
        if self.date.is_empty() {
            self.date = parsed_date.format("%Y%m%d").to_string();
        }

        if self.backups.is_empty() {
            anyhow::bail!("No backups configured");
        }

        for item in &self.backups {
            validate_safe_name(&item.archive_name, "archive_name")?;
            let has_source = item
                .source_path
                .as_deref()
                .or(item.source_dir.as_deref())
                .is_some_and(|s| !s.trim().is_empty());
            if !has_source {
                anyhow::bail!(
                    "Backup '{}' is missing source_path or source_dir",
                    item.archive_name
                );
            }
            if item.remote_dir.trim().is_empty() {
                anyhow::bail!("Backup '{}' has empty remote_dir", item.archive_name);
            }
        }

        self.uploader.resolve_and_validate()?;
        if !self.uploader.baidu.enabled && !self.uploader.cloud189.enabled {
            anyhow::bail!("No cloud uploader enabled");
        }

        self.notifier.resolve_and_validate()?;
        Ok(())
    }
}

fn normalize_opt_string(opt: &mut Option<String>) {
    if opt.as_ref().is_some_and(|s| s.trim().is_empty()) {
        *opt = None;
    }
}

fn deserialize_work_dir<'de, D>(deserializer: D) -> std::result::Result<PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    match opt {
        Some(s) if !s.trim().is_empty() => Ok(PathBuf::from(s.trim())),
        _ => Ok(default_work_dir()),
    }
}

fn default_server_location() -> String {
    "Backup Server".to_string()
}

fn default_work_dir() -> PathBuf {
    env::temp_dir()
}

fn default_compression_level() -> i32 {
    6
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    fn base_toml(extra: &str) -> String {
        format!(
            r#"
            {extra}
            [uploader.baidu]
            enabled = true
            app_key = "k"
            app_secret = "s"

            [[backups]]
            archive_name = "app"
            source_dir = "/var/www/app"
            remote_dir = "/apps/backups/{{date}}"
            "#
        )
    }

    #[test]
    fn defaults_use_temp_dir_and_level_6() {
        let mut config = Config::from_toml_str(&base_toml(""), None).unwrap();
        config.validate().unwrap();

        assert_eq!(config.server_location, "Backup Server");
        assert_eq!(config.work_dir, env::temp_dir());
        assert_eq!(config.compression_level, 6);
        assert_eq!(config.date, Local::now().format("%Y%m%d").to_string());
    }

    #[test]
    fn validate_rejects_unsafe_archive_names() {
        for bad in [
            "../escape",
            "/abs",
            "a/b",
            "a\\b",
            "-flag",
            "",
            "   ",
            "..",
            ".",
        ] {
            let toml_str = format!(
                r#"
                [uploader.baidu]
                enabled = true
                app_key = "k"
                app_secret = "s"

                [[backups]]
                archive_name = "{bad}"
                source_dir = "/tmp/src"
                remote_dir = "/backups"
                "#
            );
            let mut config = Config::from_toml_str(&toml_str, None).unwrap();
            assert!(
                config.validate().is_err(),
                "expected archive_name '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn validate_rejects_malformed_date_and_empty_backups() {
        let err =
            Config::from_toml_str(&base_toml(r#"custom_date = "2026/01/01""#), None).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("date"));

        let mut empty_backups = Config::from_toml_str(
            r#"
            backups = []
            [uploader.baidu]
            enabled = true
            app_key = "k"
            app_secret = "s"
            "#,
            None,
        )
        .unwrap();
        assert!(empty_backups.validate().is_err());
    }

    #[test]
    fn deny_unknown_fields_rejects_legacy_app_section() {
        let err = toml::from_str::<Config>(
            r#"
            [app]
            baidu_enabled = true
            [[backups]]
            archive_name = "app"
            source_dir = "/tmp"
            remote_dir = "/b"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn example_config_parses_and_validates() {
        let example = include_str!("../../../backup.toml.example");
        assert!(!example.contains(concat!("estan", ".cn")));
        assert!(example.contains("https://chanify.example.com/"));
        assert!(example.contains("https://pushgo.example.com/"));
        let mut config = Config::from_toml_str(example, None).unwrap();
        config.validate().unwrap();
    }
}
