use anyhow::Result;
use std::path::PathBuf;
use tracing::{info, warn};

use spider_cloud_rs::uploader::{
    BaiduPanUploader, Cloud189Uploader, UploadAttempt, UploadContext,
    UploadResult as EstanUploadResult, Uploader,
};

use super::config::{Config, UploadMode};

pub type UploadResult = EstanUploadResult;

struct UploaderBinding {
    uploader: Box<dyn Uploader>,
    archive_dest: Option<String>,
    files_dest: Option<String>,
}

/// Upload manager that coordinates multiple cloud uploaders
pub struct UploadManagerWrapper {
    uploaders: Vec<UploaderBinding>,
}

impl UploadManagerWrapper {
    /// Create a new upload manager from configuration
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut uploaders = Vec::new();

        let baidu_archive_dest = if config.upload_mode.needs_archive() {
            config
                .baidu_archive_upload_path
                .as_ref()
                .or(config.archive_upload_path.as_ref())
                .cloned()
        } else {
            None
        };
        let baidu_files_dest = if config.upload_mode.needs_files() {
            config
                .baidu_files_upload_path
                .as_ref()
                .or(config.files_upload_path.as_ref())
                .cloned()
        } else {
            None
        };
        if config.baidu_enabled {
            if config.upload_mode.needs_archive() && baidu_archive_dest.is_none() {
                warn!(
                    "Baidu Pan enabled for archive mode but missing ARCHIVE_UPLOAD_PATH or BAIDU_ARCHIVE_UPLOAD_PATH"
                );
            }
            if config.upload_mode.needs_files() && baidu_files_dest.is_none() {
                warn!(
                    "Baidu Pan enabled for files mode but missing FILES_UPLOAD_PATH or BAIDU_FILES_UPLOAD_PATH"
                );
            }
            if baidu_archive_dest.is_some() || baidu_files_dest.is_some() {
                if let (Some(app_key), Some(app_secret)) =
                    (&config.baidu_app_key, &config.baidu_app_secret)
                {
                    info!("Creating Baidu Pan uploader");
                    uploaders.push(UploaderBinding {
                        uploader: Box::new(BaiduPanUploader::new(
                            app_key.clone(),
                            app_secret.clone(),
                            None,
                        )?),
                        archive_dest: baidu_archive_dest,
                        files_dest: baidu_files_dest,
                    });
                } else {
                    warn!("Baidu Pan enabled but missing BAIDU_APP_KEY or BAIDU_APP_SECRET");
                }
            }
        }

        let cloud189_archive_dest = if config.upload_mode.needs_archive() {
            config
                .cloud189_archive_upload_path
                .as_ref()
                .or(config.archive_upload_path.as_ref())
                .cloned()
        } else {
            None
        };
        let cloud189_files_dest = if config.upload_mode.needs_files() {
            config
                .cloud189_files_upload_path
                .as_ref()
                .or(config.files_upload_path.as_ref())
                .cloned()
        } else {
            None
        };
        if config.cloud189_enabled {
            if config.upload_mode.needs_archive() && cloud189_archive_dest.is_none() {
                warn!("Cloud189 enabled for archive mode but missing ARCHIVE_UPLOAD_PATH");
            }
            if config.upload_mode.needs_files() && cloud189_files_dest.is_none() {
                warn!("Cloud189 enabled for files mode but missing FILES_UPLOAD_PATH");
            }
            if cloud189_archive_dest.is_some() || cloud189_files_dest.is_some() {
                info!("Creating Cloud189 uploader");
                uploaders.push(UploaderBinding {
                    uploader: Box::new(Cloud189Uploader::new(
                        config.cloud189_config_path.as_ref().map(PathBuf::from),
                        config.cloud189_username.clone(),
                        config.cloud189_password.clone(),
                        config.cloud189_qr_login,
                    )?),
                    archive_dest: cloud189_archive_dest,
                    files_dest: cloud189_files_dest,
                });
            } else {
                warn!("Cloud189 enabled but no upload path is configured");
            }
        }

        if uploaders.is_empty() {
            warn!("No cloud uploaders configured");
        }

        Ok(Self { uploaders })
    }

    /// Upload a file to all configured cloud storage services
    pub fn upload_file(
        &mut self,
        file_path: &str,
        date_str: &str,
        mode: UploadMode,
    ) -> Result<UploadResult> {
        let ctx = UploadContext::with_date(date_str);
        let mut attempts = Vec::new();

        for binding in &mut self.uploaders {
            let dest = match mode {
                UploadMode::Archive => binding.archive_dest.as_ref(),
                UploadMode::Files | UploadMode::Hybrid => binding.files_dest.as_ref(),
            };
            let Some(dest) = dest else {
                continue;
            };

            let expanded_path = ctx.expand(dest);
            let name = binding.uploader.name().to_string();
            match binding.uploader.upload(file_path, &expanded_path) {
                Ok(()) => attempts.push(UploadAttempt::success(name)),
                Err(err) => attempts.push(UploadAttempt::failure(name, err.to_string())),
            }
        }

        Ok(UploadResult::from_attempts(attempts))
    }

    /// Check if any uploaders are configured
    pub fn has_uploaders(&self) -> bool {
        !self.uploaders.is_empty()
    }
}
