#![forbid(unsafe_code)]

mod config;
mod notification;
mod utils;

use anyhow::{Context, Result};
use spider_common::archive::{ArchiveConfig, create_archive};
use spider_common::cli::{CliAction, parse_cli};
use spider_common::logging;
use spider_uploader::{UploadAttempt, UploadCoordinator, UploadResult, UploadSkippedReason};
use std::path::Path;
use tracing::{error, info, warn};

use crate::config::{Config, UploadMode};
use crate::notification::NotificationManager;
use crate::utils::{find_all_files, find_files_by_pattern};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadTarget {
    Archive,
    Files,
}

fn main() -> Result<()> {
    let args = match parse_cli(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        "Archive local files and upload files or archives to cloud storage",
        std::env::args().skip(1),
    )? {
        CliAction::PrintHelp(text) | CliAction::PrintVersion(text) => {
            println!("{text}");
            return Ok(());
        }
        CliAction::Run(args) => args,
    };

    let mut config = Config::from_toml(&args.config_path, args.date_override.as_deref())
        .context("Failed to load configuration")?;
    logging::init_default(config.debug)?;

    info!("Cloud Uploader started - Processing date: {}", config.date);
    info!("Running on server: {}", config.server_location);

    // Fail-fast configuration validation before any archiving or uploading.
    config.validate()?;

    let notification_manager = NotificationManager::from_config(&config)?;
    let mut coordinator = UploadCoordinator::from_config(&config.uploader, args.dry_run)?;

    if args.dry_run {
        info!(
            "Dry run mode enabled; validated config for mode {:?}",
            config.upload_mode
        );
        if config.upload_mode.needs_archive() {
            info!(
                "[dry-run] Would archive '{}' -> '{}'",
                config.archive.source_folder, config.archive.output
            );
        }
        if config.upload_mode.needs_files() {
            info!(
                "[dry-run] Would upload files from '{}' (pattern={:?})",
                config.files.source_folder, config.files.pattern
            );
        }
        return Ok(());
    }

    match config.upload_mode {
        UploadMode::Archive => {
            let result = match handle_archive_mode(&config, &mut coordinator) {
                Ok(res) => res,
                Err(err) => {
                    notification_manager.send_archive_failure(
                        &config.server_location,
                        &config.date,
                        &err.to_string(),
                    );
                    return Err(err);
                }
            };
            notification_manager.send_upload_result(&config.server_location, &config.date, &result);

            ensure_upload_ok(&result, "Archive upload")?;
            if has_failures(&result) {
                anyhow::bail!("Archive upload failed");
            }
        }
        UploadMode::Files => {
            let results = handle_files_mode(&config, &mut coordinator)?;

            let merged_result = merge_upload_results(results);
            notification_manager.send_upload_result(
                &config.server_location,
                &config.date,
                &merged_result,
            );

            ensure_upload_ok(&merged_result, "File upload")?;
            if has_failures(&merged_result) {
                anyhow::bail!("Some file uploads failed");
            }
        }
        UploadMode::Hybrid => {
            info!("Hybrid mode: Executing both archive and files operations");

            let mut all_results = Vec::new();

            match handle_archive_mode(&config, &mut coordinator) {
                Ok(result) => {
                    all_results.push(result);
                }
                Err(e) => {
                    error!("Archive operation failed: {}", e);
                    if config.archive.require_archive {
                        notification_manager.send_archive_failure(
                            &config.server_location,
                            &config.date,
                            &e.to_string(),
                        );
                        return Err(e);
                    }
                    warn!("Continuing with files operation despite archive failure");
                }
            }

            match handle_files_mode(&config, &mut coordinator) {
                Ok(results) => {
                    all_results.extend(results);
                }
                Err(e) => {
                    error!("Files operation failed: {}", e);
                    all_results.push(UploadResult {
                        overall_success: false,
                        attempts: vec![UploadAttempt::failure("Files", e.to_string())],
                        skipped_reason: None,
                    });
                }
            }

            let merged_result = merge_upload_results(all_results);
            notification_manager.send_upload_result(
                &config.server_location,
                &config.date,
                &merged_result,
            );

            ensure_upload_ok(&merged_result, "Hybrid upload")?;
            if has_failures(&merged_result) {
                anyhow::bail!("Some uploads failed in hybrid mode");
            }
        }
    }

    info!("Cloud Uploader completed successfully");
    Ok(())
}

fn handle_archive_mode(
    config: &Config,
    coordinator: &mut UploadCoordinator,
) -> Result<UploadResult> {
    info!(
        "Archive mode: Compressing folder {} -> {}",
        config.archive.source_folder, config.archive.output
    );

    let archive_cfg = ArchiveConfig {
        level: config.archive.level,
        threads: config.archive.threads,
        reuse_existing: true,
    };

    if let Err(e) = create_archive(
        Path::new(&config.archive.source_folder),
        Path::new(&config.archive.output),
        &archive_cfg,
    ) {
        error!("Archive creation failed: {}", e);
        if config.archive.require_archive {
            anyhow::bail!("Archiving failed and is required: {}", e);
        }
        warn!("Archiving failed but not required, skipping archive upload");
        return Ok(UploadResult::skipped(
            UploadSkippedReason::OptionalArchiveFailed,
        ));
    }

    let output_path = Path::new(&config.archive.output);
    if !output_path.exists() {
        error!("Archive file not found: {}", config.archive.output);
        anyhow::bail!("Archive file not found: {}", config.archive.output);
    }

    upload_file(config, output_path, coordinator, UploadTarget::Archive)
}

fn handle_files_mode(
    config: &Config,
    coordinator: &mut UploadCoordinator,
) -> Result<Vec<UploadResult>> {
    info!(
        "Files mode: Searching in folder: {}",
        config.files.source_folder
    );

    let files = if let Some(pattern) = &config.files.pattern {
        info!("Matching pattern: {}", pattern);
        find_files_by_pattern(&config.files.source_folder, pattern)?
    } else {
        info!("No pattern specified, uploading all files in folder");
        find_all_files(&config.files.source_folder)?
    };

    if files.is_empty() {
        warn!("No files found to upload");
        return Ok(vec![UploadResult::skipped(
            UploadSkippedReason::NoFilesFound,
        )]);
    }

    info!("Found {} file(s) to upload", files.len());

    let mut results = Vec::with_capacity(files.len());
    for file_path in files {
        let file_display = file_path.display().to_string();
        info!("Uploading file: {}", file_display);

        match upload_file(config, &file_path, coordinator, UploadTarget::Files) {
            Ok(result) => results.push(result),
            Err(e) => {
                error!("Failed to upload {}: {}", file_display, e);
                results.push(UploadResult {
                    overall_success: false,
                    attempts: vec![UploadAttempt::failure(file_display, e.to_string())],
                    skipped_reason: None,
                });
            }
        }
    }

    Ok(results)
}

fn upload_file(
    config: &Config,
    file_path: &Path,
    coordinator: &mut UploadCoordinator,
    target: UploadTarget,
) -> Result<UploadResult> {
    coordinator
        .upload_to_dest(file_path, |provider| match (target, provider) {
            (UploadTarget::Archive, "BaiduPan") => config.baidu_archive_dest(),
            (UploadTarget::Archive, "Cloud189") => config.cloud189_archive_dest(),
            (UploadTarget::Files, "BaiduPan") => config.baidu_files_dest(),
            (UploadTarget::Files, "Cloud189") => config.cloud189_files_dest(),
            _ => None,
        })
        .map_err(anyhow::Error::from)
}

fn has_failures(result: &UploadResult) -> bool {
    result.attempts.iter().any(|attempt| !attempt.success)
}

fn ensure_upload_ok(result: &UploadResult, context: &str) -> Result<()> {
    match &result.skipped_reason {
        Some(UploadSkippedReason::NoUploadersConfigured) => {
            anyhow::bail!("{context}: no cloud uploaders configured")
        }
        Some(UploadSkippedReason::NoDestinationConfigured) => {
            anyhow::bail!("{context}: no destination path configured for this upload mode")
        }
        Some(UploadSkippedReason::OptionalArchiveFailed | UploadSkippedReason::NoFilesFound)
        | None => Ok(()),
    }
}

fn is_config_skip(reason: &UploadSkippedReason) -> bool {
    matches!(
        reason,
        UploadSkippedReason::NoUploadersConfigured | UploadSkippedReason::NoDestinationConfigured
    )
}

fn merge_upload_results(results: Vec<UploadResult>) -> UploadResult {
    if results.is_empty() {
        return UploadResult::empty();
    }

    let mut attempts = Vec::new();
    let mut skipped_reason = None;

    for result in results {
        if let Some(reason) = result.skipped_reason
            && skipped_reason
                .as_ref()
                .map(|current| !is_config_skip(current) && is_config_skip(&reason))
                .unwrap_or(true)
        {
            skipped_reason = Some(reason);
        }
        attempts.extend(result.attempts);
    }

    if attempts.is_empty() {
        if let Some(reason) = skipped_reason {
            UploadResult::skipped(reason)
        } else {
            UploadResult::empty()
        }
    } else {
        UploadResult::from_attempts(attempts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_upload_ok_rejects_config_skips() {
        let no_uploaders = UploadResult::skipped(UploadSkippedReason::NoUploadersConfigured);
        assert!(ensure_upload_ok(&no_uploaders, "upload").is_err());

        let no_dest = UploadResult::skipped(UploadSkippedReason::NoDestinationConfigured);
        assert!(ensure_upload_ok(&no_dest, "upload").is_err());
    }

    #[test]
    fn ensure_upload_ok_allows_business_skips() {
        let optional_archive = UploadResult::skipped(UploadSkippedReason::OptionalArchiveFailed);
        assert!(ensure_upload_ok(&optional_archive, "upload").is_ok());

        let no_files = UploadResult::skipped(UploadSkippedReason::NoFilesFound);
        assert!(ensure_upload_ok(&no_files, "upload").is_ok());
    }

    #[test]
    fn merge_upload_results_prioritizes_config_skip() {
        let merged = merge_upload_results(vec![
            UploadResult::skipped(UploadSkippedReason::OptionalArchiveFailed),
            UploadResult::skipped(UploadSkippedReason::NoDestinationConfigured),
        ]);

        assert_eq!(
            merged.skipped_reason,
            Some(UploadSkippedReason::NoDestinationConfigured)
        );
        assert!(ensure_upload_ok(&merged, "upload").is_err());
    }
}
