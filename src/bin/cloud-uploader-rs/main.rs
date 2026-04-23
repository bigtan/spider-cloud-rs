mod archive;
mod config;
mod notification;
mod uploader;
mod utils;

use anyhow::{Context, Result};
use spider_cloud_rs::logging;
use spider_cloud_rs::uploader::UploadAttempt;
use std::path::Path;
use tracing::{error, info, warn};

use crate::archive::create_archive;
use crate::config::{Config, UploadMode};
use crate::notification::NotificationManager;
use crate::uploader::{UploadManagerWrapper, UploadResult};
use crate::utils::find_files_by_pattern;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().unwrap_or_else(|| "config.toml".to_string());
    if args.next().is_some() {
        anyhow::bail!("Too many arguments");
    }

    let config = Config::from_toml(&config_path).context("Failed to load configuration")?;
    logging::init_default(config.debug)?;

    info!("Cloud Uploader started - Processing date: {}", config.date);
    info!("Running on server: {}", config.server_location);

    // Validate configuration
    config.validate()?;

    // Initialize services
    let notification_manager = NotificationManager::from_config(&config);
    let mut upload_manager = UploadManagerWrapper::from_config(&config)?;

    // Process based on upload mode
    match config.upload_mode {
        UploadMode::Archive => {
            let result = handle_archive_mode(&config, &mut upload_manager)?;
            notification_manager.send_upload_result(&config.server_location, &config.date, &result);

            if !result.overall_success {
                anyhow::bail!("Archive upload failed");
            }
        }
        UploadMode::Files => {
            let results = handle_files_mode(&config, &mut upload_manager)?;

            // Merge all results
            let merged_result = merge_upload_results(results);
            notification_manager.send_upload_result(
                &config.server_location,
                &config.date,
                &merged_result,
            );

            if !merged_result.overall_success {
                anyhow::bail!("Some file uploads failed");
            }
        }
        UploadMode::Hybrid => {
            // Execute both archive and files mode
            info!("Hybrid mode: Executing both archive and files operations");

            let mut all_results = Vec::new();

            // First, handle archive mode
            match handle_archive_mode(&config, &mut upload_manager) {
                Ok(result) => {
                    all_results.push(result);
                }
                Err(e) => {
                    error!("Archive operation failed: {}", e);
                    if config.require_archive {
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

            // Then, handle files mode
            match handle_files_mode(&config, &mut upload_manager) {
                Ok(results) => {
                    all_results.extend(results);
                }
                Err(e) => {
                    error!("Files operation failed: {}", e);
                    // Continue to send notification with whatever we have
                }
            }

            // Merge all results and send single notification
            let merged_result = merge_upload_results(all_results);
            notification_manager.send_upload_result(
                &config.server_location,
                &config.date,
                &merged_result,
            );

            if !merged_result.overall_success {
                anyhow::bail!("Some uploads failed in hybrid mode");
            }
        }
    }

    info!("Cloud Uploader completed successfully");
    Ok(())
}

/// Handle archive mode: compress entire folder and upload
fn handle_archive_mode(
    config: &Config,
    upload_manager: &mut UploadManagerWrapper,
) -> Result<UploadResult> {
    info!(
        "Archive mode: Compressing folder {}",
        config.archive_source_folder
    );

    // Create archive
    match create_archive(
        &config.archive_source_folder,
        &config.archive_output,
        config.archive_password.as_deref(),
    ) {
        Ok(_) => {}
        Err(e) => {
            error!("Archive creation failed: {}", e);

            if config.require_archive {
                anyhow::bail!("Archiving failed and is required: {}", e);
            } else {
                warn!("Archiving failed but not required, skipping upload");
                // Return empty result
                return Ok(UploadResult::from_attempts(vec![UploadAttempt::failure(
                    "Archive creation",
                    e.to_string(),
                )]));
            }
        }
    }

    // Check if archive exists
    if !Path::new(&config.archive_output).exists() {
        error!("Archive file not found: {}", config.archive_output);
        anyhow::bail!("Archive file not found: {}", config.archive_output);
    }

    // Upload archive
    upload_file(
        &config.archive_output,
        &config.date,
        upload_manager,
        UploadMode::Archive,
    )
}

/// Handle files mode: upload specific files matching pattern (no archiving)
fn handle_files_mode(
    config: &Config,
    upload_manager: &mut UploadManagerWrapper,
) -> Result<Vec<UploadResult>> {
    info!(
        "Files mode: Searching in folder: {}",
        config.files_source_folder
    );

    // Find files
    let files = if let Some(pattern) = &config.file_pattern {
        info!("Matching pattern: {}", pattern);
        find_files_by_pattern(&config.files_source_folder, pattern)?
    } else {
        info!("No pattern specified, uploading all files in folder");
        find_all_files(&config.files_source_folder)?
    };

    if files.is_empty() {
        warn!("No files found to upload");
        return Ok(vec![]);
    }

    info!("Found {} file(s) to upload", files.len());

    // Upload each file and collect results
    let mut results = Vec::new();
    for file_path in files {
        let file_str = file_path.to_string_lossy().to_string();
        info!("Uploading file: {}", file_str);

        match upload_file(&file_str, &config.date, upload_manager, UploadMode::Files) {
            Ok(result) => {
                results.push(result);
            }
            Err(e) => {
                error!("Failed to upload {}: {}", file_str, e);
                results.push(UploadResult {
                    overall_success: false,
                    attempts: vec![UploadAttempt::failure(file_str.clone(), e.to_string())],
                });
            }
        }
    }

    Ok(results)
}

/// Find all files in a directory (non-recursive for now)
fn find_all_files(folder: &str) -> Result<Vec<std::path::PathBuf>> {
    use std::fs;

    let path = Path::new(folder);
    if !path.exists() {
        anyhow::bail!("Folder not found: {}", folder);
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }

    Ok(files)
}

/// Upload a file without sending notification
fn upload_file(
    file_path: &str,
    date_str: &str,
    upload_manager: &mut UploadManagerWrapper,
    mode: UploadMode,
) -> Result<UploadResult> {
    if !upload_manager.has_uploaders() {
        warn!("No cloud uploaders configured, skipping upload");
        return Ok(UploadResult::empty());
    }

    // Upload file
    upload_manager.upload_file(file_path, date_str, mode)
}

/// Merge multiple upload results into one
fn merge_upload_results(results: Vec<UploadResult>) -> UploadResult {
    if results.is_empty() {
        return UploadResult::empty();
    }

    let mut attempts = Vec::new();

    for result in results {
        attempts.extend(result.attempts);
    }

    UploadResult::from_attempts(attempts)
}
