use std::path::Path;

use tracing::info;

use crate::baidu::BaiduPanUploader;
use crate::cloud189::Cloud189Uploader;
use crate::config::{MAX_CONCURRENT_CHUNKS, MIN_CONCURRENT_CHUNKS, UploadersConfig};
use crate::digest::{FileDigests, compute_file_digests};
use crate::error::{Result, UploadError};

pub trait Uploader: Send {
    fn name(&self) -> &str;

    fn needs_baidu_digests(&self) -> bool {
        false
    }

    fn needs_cloud189_digests(&self) -> bool {
        false
    }

    fn upload_with_digests(
        &mut self,
        file_path: &Path,
        dest_path: &str,
        digests: &FileDigests,
    ) -> Result<()>;

    fn upload(&mut self, file_path: &Path, dest_path: &str) -> Result<()> {
        let digests = compute_file_digests(
            file_path,
            self.needs_baidu_digests(),
            self.needs_cloud189_digests(),
        )?;
        self.upload_with_digests(file_path, dest_path, &digests)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadAttempt {
    pub name: String,
    pub success: bool,
    pub error: Option<String>,
}

impl UploadAttempt {
    pub fn success(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            success: true,
            error: None,
        }
    }

    pub fn failure(name: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            success: false,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadSkippedReason {
    NoUploadersConfigured,
    NoDestinationConfigured,
    OptionalArchiveFailed,
    NoFilesFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub overall_success: bool,
    pub attempts: Vec<UploadAttempt>,
    pub skipped_reason: Option<UploadSkippedReason>,
}

impl UploadResult {
    pub fn empty() -> Self {
        Self {
            overall_success: false,
            attempts: Vec::new(),
            skipped_reason: None,
        }
    }

    pub fn skipped(reason: UploadSkippedReason) -> Self {
        Self {
            overall_success: false,
            attempts: Vec::new(),
            skipped_reason: Some(reason),
        }
    }

    pub fn from_attempts(attempts: Vec<UploadAttempt>) -> Self {
        let overall_success =
            !attempts.is_empty() && attempts.iter().all(|attempt| attempt.success);
        Self {
            overall_success,
            attempts,
            skipped_reason: None,
        }
    }
}

pub struct UploadCoordinator {
    uploaders: Vec<Box<dyn Uploader>>,
}

impl UploadCoordinator {
    /// Construct a coordinator from an explicit list of uploaders.
    pub fn new(uploaders: Vec<Box<dyn Uploader>>) -> Self {
        Self { uploaders }
    }

    /// Construct a coordinator from [`UploadersConfig`].
    ///
    /// When `dry_run` is `true`, uploaders are initialized without network
    /// authentication or interactive login prompts.
    pub fn from_config(cfg: &UploadersConfig, dry_run: bool) -> Result<Self> {
        if !(MIN_CONCURRENT_CHUNKS..=MAX_CONCURRENT_CHUNKS).contains(&cfg.max_concurrent_chunks) {
            return Err(UploadError::InvalidConfig(format!(
                "max_concurrent_chunks must be in {MIN_CONCURRENT_CHUNKS}..={MAX_CONCURRENT_CHUNKS}, got {}",
                cfg.max_concurrent_chunks
            )));
        }

        let mut uploaders: Vec<Box<dyn Uploader>> = Vec::new();

        if cfg.baidu.enabled {
            let app_key = cfg
                .baidu
                .app_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    UploadError::InvalidConfig(
                        "Baidu Pan uploader enabled without app_key".to_string(),
                    )
                })?
                .to_string();
            let app_secret = cfg
                .baidu
                .app_secret
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    UploadError::InvalidConfig(
                        "Baidu Pan uploader enabled without app_secret".to_string(),
                    )
                })?
                .to_string();
            let config_path = cfg
                .baidu
                .config_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from);

            info!("Initializing Baidu Pan uploader (dry_run={dry_run})");
            let uploader = if dry_run {
                BaiduPanUploader::new_dry_run(
                    app_key,
                    app_secret,
                    config_path,
                    cfg.max_concurrent_chunks,
                )?
            } else {
                BaiduPanUploader::with_concurrency(
                    app_key,
                    app_secret,
                    config_path,
                    cfg.max_concurrent_chunks,
                )?
            };
            uploaders.push(Box::new(uploader));
        }

        if cfg.cloud189.enabled {
            let config_path = cfg
                .cloud189
                .config_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from);

            info!("Initializing Cloud189 uploader (dry_run={dry_run})");
            let uploader = if dry_run {
                if !cfg.cloud189.use_qr
                    && (cfg.cloud189.username.is_some() ^ cfg.cloud189.password.is_some())
                {
                    return Err(UploadError::InvalidConfig(
                        "Cloud189 uploader enabled with incomplete username/password".to_string(),
                    ));
                }
                Cloud189Uploader::new_dry_run(cfg.max_concurrent_chunks)?
            } else {
                Cloud189Uploader::with_concurrency(
                    config_path,
                    cfg.cloud189.username.clone(),
                    cfg.cloud189.password.clone(),
                    cfg.cloud189.use_qr,
                    cfg.max_concurrent_chunks,
                )?
            };
            uploaders.push(Box::new(uploader));
        }

        Ok(Self { uploaders })
    }

    pub fn has_uploaders(&self) -> bool {
        !self.uploaders.is_empty()
    }

    pub fn uploader_names(&self) -> Vec<&str> {
        self.uploaders.iter().map(|u| u.name()).collect()
    }

    /// Compute required file digests once, then upload `local_path` across all
    /// matching providers concurrently using `std::thread::scope`.
    pub fn upload_to_dest(
        &mut self,
        local_path: &Path,
        dest_for_provider: impl Fn(&str) -> Option<String> + Sync,
    ) -> Result<UploadResult> {
        if self.uploaders.is_empty() {
            return Ok(UploadResult::skipped(
                UploadSkippedReason::NoUploadersConfigured,
            ));
        }

        let mut targets: Vec<(&mut Box<dyn Uploader>, String)> = Vec::new();
        for uploader in &mut self.uploaders {
            if let Some(dest) = dest_for_provider(uploader.name()) {
                targets.push((uploader, dest));
            }
        }

        if targets.is_empty() {
            return Ok(UploadResult::skipped(
                UploadSkippedReason::NoDestinationConfigured,
            ));
        }

        let need_baidu = targets.iter().any(|(u, _)| u.needs_baidu_digests());
        let need_cloud189 = targets.iter().any(|(u, _)| u.needs_cloud189_digests());
        let digests = compute_file_digests(local_path, need_baidu, need_cloud189)?;
        let digests_ref = &digests;

        let attempts = std::thread::scope(|scope| {
            let handles: Vec<_> = targets
                .into_iter()
                .map(|(uploader, dest)| {
                    scope.spawn(move || {
                        let name = uploader.name().to_string();
                        match uploader.upload_with_digests(local_path, &dest, digests_ref) {
                            Ok(()) => UploadAttempt::success(name),
                            Err(err) => UploadAttempt::failure(name, err.to_string()),
                        }
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        UploadAttempt::failure("uploader", "upload worker thread panicked")
                    })
                })
                .collect::<Vec<_>>()
        });

        Ok(UploadResult::from_attempts(attempts))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::config::{BaiduUploaderConfig, Cloud189UploaderConfig};

    struct MockUploader {
        name: &'static str,
        need_baidu: bool,
        need_cloud189: bool,
        delay: Duration,
        should_fail: bool,
        seen_digests: Arc<std::sync::Mutex<Vec<FileDigests>>>,
        active_count: Arc<AtomicUsize>,
        saw_parallel: Arc<AtomicBool>,
    }

    impl Uploader for MockUploader {
        fn name(&self) -> &str {
            self.name
        }

        fn needs_baidu_digests(&self) -> bool {
            self.need_baidu
        }

        fn needs_cloud189_digests(&self) -> bool {
            self.need_cloud189
        }

        fn upload_with_digests(
            &mut self,
            _file_path: &Path,
            _dest_path: &str,
            digests: &FileDigests,
        ) -> Result<()> {
            let cur = self.active_count.fetch_add(1, Ordering::SeqCst) + 1;
            if cur >= 2 {
                self.saw_parallel.store(true, Ordering::SeqCst);
            }
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            self.active_count.fetch_sub(1, Ordering::SeqCst);
            self.seen_digests.lock().unwrap().push(digests.clone());
            if self.should_fail {
                Err(UploadError::Protocol(format!("{} failed", self.name)))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn from_config_dry_run_constructs_uploaders_without_network() {
        let cfg = UploadersConfig {
            max_concurrent_chunks: 4,
            baidu: BaiduUploaderConfig {
                enabled: true,
                app_key: Some("k".into()),
                app_secret: Some("s".into()),
                ..Default::default()
            },
            cloud189: Cloud189UploaderConfig {
                enabled: true,
                use_qr: true,
                ..Default::default()
            },
        };

        let mut coord = UploadCoordinator::from_config(&cfg, true).unwrap();
        assert!(coord.has_uploaders());
        assert_eq!(coord.uploader_names(), vec!["BaiduPan", "Cloud189"]);

        let temp_file =
            std::env::temp_dir().join(format!("coord_dry_{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&temp_file, b"dry run payload").unwrap();

        let res = coord
            .upload_to_dest(&temp_file, |provider| match provider {
                "BaiduPan" => Some("/apps/test".to_string()),
                "Cloud189" => Some("/backups/test".to_string()),
                _ => None,
            })
            .unwrap();
        let _ = std::fs::remove_file(&temp_file);

        assert!(res.overall_success);
        assert_eq!(res.attempts.len(), 2);
    }

    #[test]
    fn coordinator_skips_when_empty_or_no_destinations() {
        let empty_cfg = UploadersConfig::default();
        let mut coord = UploadCoordinator::from_config(&empty_cfg, true).unwrap();
        assert!(!coord.has_uploaders());

        let temp_file =
            std::env::temp_dir().join(format!("coord_skip_{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&temp_file, b"payload").unwrap();

        let res = coord
            .upload_to_dest(&temp_file, |_| Some("/dest".to_string()))
            .unwrap();
        assert_eq!(
            res.skipped_reason,
            Some(UploadSkippedReason::NoUploadersConfigured)
        );

        let mut coord_with_mock = UploadCoordinator::new(vec![Box::new(MockUploader {
            name: "BaiduPan",
            need_baidu: true,
            need_cloud189: false,
            delay: Duration::ZERO,
            should_fail: false,
            seen_digests: Arc::default(),
            active_count: Arc::default(),
            saw_parallel: Arc::default(),
        })]);

        let res2 = coord_with_mock
            .upload_to_dest(&temp_file, |_| None)
            .unwrap();
        let _ = std::fs::remove_file(&temp_file);
        assert_eq!(
            res2.skipped_reason,
            Some(UploadSkippedReason::NoDestinationConfigured)
        );
    }

    #[test]
    fn coordinator_computes_digests_once_and_runs_providers_in_parallel() {
        let temp_file =
            std::env::temp_dir().join(format!("coord_parallel_{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&temp_file, b"parallel multi-cloud test data").unwrap();

        let active = Arc::new(AtomicUsize::new(0));
        let saw_parallel = Arc::new(AtomicBool::new(false));
        let seen_a = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_b = Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut coord = UploadCoordinator::new(vec![
            Box::new(MockUploader {
                name: "BaiduPan",
                need_baidu: true,
                need_cloud189: false,
                delay: Duration::from_millis(80),
                should_fail: false,
                seen_digests: seen_a.clone(),
                active_count: active.clone(),
                saw_parallel: saw_parallel.clone(),
            }),
            Box::new(MockUploader {
                name: "Cloud189",
                need_baidu: false,
                need_cloud189: true,
                delay: Duration::from_millis(80),
                should_fail: true,
                seen_digests: seen_b.clone(),
                active_count: active,
                saw_parallel: saw_parallel.clone(),
            }),
        ]);

        let res = coord
            .upload_to_dest(&temp_file, |name| Some(format!("/dest/{name}")))
            .unwrap();
        let _ = std::fs::remove_file(&temp_file);

        assert!(saw_parallel.load(Ordering::SeqCst));
        assert!(!res.overall_success);
        assert_eq!(res.attempts.len(), 2);
        assert!(res.attempts[0].success);
        assert!(!res.attempts[1].success);

        // Both uploaders received identical precomputed FileDigests containing both Baidu & Cloud189 hashes.
        let da = seen_a.lock().unwrap();
        let db = seen_b.lock().unwrap();
        assert_eq!(da[0], db[0]);
        assert!(!da[0].baidu_block_md5s.is_empty());
        assert!(!da[0].cloud189_parts.is_empty());
    }
}
