#![forbid(unsafe_code)]

mod config;
mod notification;

use anyhow::{Context, Result};
use spider_common::archive::{ArchiveConfig, create_archive};
use spider_common::cli::{CliAction, parse_cli};
use spider_common::fs::validate_safe_name;
use spider_common::logging;
use spider_common::template::TemplateContext;
use spider_uploader::UploadCoordinator;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{error, info, warn};

use crate::config::{BackupItem, Config};
use crate::notification::{BackupItemOutcome, NotificationManager};

fn main() -> Result<()> {
    let args = match parse_cli(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        "Create compressed backup archives and upload them to cloud storage",
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

    info!("Backup to Cloud started - Processing date: {}", config.date);
    info!("Running on server: {}", config.server_location);

    config.validate()?;

    let notification_manager = NotificationManager::from_config(&config)?;
    let mut coordinator = UploadCoordinator::from_config(&config.uploader, args.dry_run)?;

    if args.dry_run {
        info!(
            "Dry run mode enabled; validated {} backup item(s) using work_dir '{}'",
            config.backups.len(),
            config.work_dir.display()
        );
        for item in &config.backups {
            let safe_name = validate_safe_name(&item.archive_name, "archive_name")?;
            let mut ctx = TemplateContext::with_date(config.date.as_str());
            ctx.insert("server_location", config.server_location.as_str());
            ctx.insert("archive_name", safe_name.as_str());
            let source_path = resolve_source_path(item, &ctx)?;
            let remote_dir = ctx.expand(&item.remote_dir);
            info!(
                "[dry-run] Would archive '{}' from '{}' -> '{}' (remote: '{}')",
                safe_name,
                source_path.display(),
                config
                    .work_dir
                    .join(format!("{safe_name}-{}.tar.zst", config.date))
                    .display(),
                remote_dir
            );
        }
        return Ok(());
    }

    fs::create_dir_all(&config.work_dir).with_context(|| {
        format!(
            "Failed to create work directory: {}",
            config.work_dir.display()
        )
    })?;

    let archive_cfg = ArchiveConfig {
        level: Some(config.compression_level),
        threads: config.threads,
        reuse_existing: false,
    };

    let mut failures: Vec<String> = Vec::new();
    let mut outcomes: Vec<BackupItemOutcome> = Vec::with_capacity(config.backups.len());

    for item in &config.backups {
        let base_name = validate_safe_name(&item.archive_name, "archive_name")?;
        let mut ctx = TemplateContext::with_date(config.date.as_str());
        ctx.insert("server_location", config.server_location.as_str());
        ctx.insert("archive_name", base_name.as_str());

        let remote_dir = ctx.expand(&item.remote_dir);
        let source_path = match resolve_source_path(item, &ctx) {
            Ok(path) => path,
            Err(err) => {
                let message = format!("[{base_name}] invalid source path: {err}");
                error!("{}", message);
                failures.push(message.clone());
                outcomes.push(BackupItemOutcome {
                    archive_name: base_name,
                    remote_dir,
                    stage_error: Some(err.to_string()),
                    attempts: Vec::new(),
                });
                continue;
            }
        };

        if let Some(command) = item.command.as_deref() {
            let expanded_command = ctx.expand(command);
            info!("Running command for backup item: {}", base_name);
            let workdir = item.command_workdir.as_deref().map(|dir| ctx.expand(dir));
            if let Err(err) = run_command(&expanded_command, workdir.as_deref()) {
                let message = format!("[{base_name}] command failed: {err}");
                error!("{}", message);
                failures.push(message);
                outcomes.push(BackupItemOutcome {
                    archive_name: base_name,
                    remote_dir,
                    stage_error: Some(format!("command failed: {err}")),
                    attempts: Vec::new(),
                });
                continue;
            }
        }

        if !source_path.exists() {
            let message = format!(
                "[{base_name}] source path not found: {}",
                source_path.display()
            );
            error!("{}", message);
            failures.push(message);
            outcomes.push(BackupItemOutcome {
                archive_name: base_name,
                remote_dir,
                stage_error: Some(format!("source path not found: {}", source_path.display())),
                attempts: Vec::new(),
            });
            continue;
        }
        if !source_path.is_dir() && !source_path.is_file() {
            let message = format!(
                "[{base_name}] source path is not a file or directory: {}",
                source_path.display()
            );
            error!("{}", message);
            failures.push(message);
            outcomes.push(BackupItemOutcome {
                archive_name: base_name,
                remote_dir,
                stage_error: Some(format!(
                    "source path is not a file or directory: {}",
                    source_path.display()
                )),
                attempts: Vec::new(),
            });
            continue;
        }

        let archive_path = build_archive_path(&config.work_dir, &base_name, &config.date);
        info!("Creating archive: {}", archive_path.display());
        if let Err(err) = create_archive(&source_path, &archive_path, &archive_cfg) {
            let message = format!("[{base_name}] create archive failed: {err}");
            error!("{}", message);
            failures.push(message);
            outcomes.push(BackupItemOutcome {
                archive_name: base_name,
                remote_dir,
                stage_error: Some(format!("create archive failed: {err}")),
                attempts: Vec::new(),
            });
            continue;
        }

        let upload_result =
            match coordinator.upload_to_dest(&archive_path, |_| Some(remote_dir.clone())) {
                Ok(res) => res,
                Err(err) => {
                    let message = format!("[{base_name}] upload coordinator error: {err}");
                    error!("{}", message);
                    failures.push(message);
                    warn!(
                        "Archive retained because upload errored: {}",
                        archive_path.display()
                    );
                    outcomes.push(BackupItemOutcome {
                        archive_name: base_name,
                        remote_dir,
                        stage_error: Some(err.to_string()),
                        attempts: Vec::new(),
                    });
                    continue;
                }
            };

        let upload_failed = upload_result.attempts.is_empty()
            || !upload_result.overall_success
            || upload_result.attempts.iter().any(|a| !a.success);

        for attempt in &upload_result.attempts {
            if !attempt.success {
                let err_text = attempt.error.as_deref().unwrap_or("unknown error");
                let message = format!(
                    "[{base_name}] upload failed on {}: {err_text}",
                    attempt.name
                );
                error!("{}", message);
                failures.push(message);
            }
        }
        if upload_result.attempts.is_empty() {
            let message = format!("[{base_name}] no cloud uploaders executed");
            error!("{}", message);
            failures.push(message);
        }

        outcomes.push(BackupItemOutcome {
            archive_name: base_name,
            remote_dir,
            stage_error: None,
            attempts: upload_result.attempts,
        });

        if upload_failed {
            warn!(
                "Archive retained because one or more uploads failed: {}",
                archive_path.display()
            );
            continue;
        }

        if !item.keep_archive.unwrap_or(false) {
            fs::remove_file(&archive_path).with_context(|| {
                format!(
                    "Failed to remove archive file after upload: {}",
                    archive_path.display()
                )
            })?;
        }
        if item.command.is_some()
            && !item.keep_command_source.unwrap_or(true)
            && source_path.is_file()
        {
            fs::remove_file(&source_path).with_context(|| {
                format!(
                    "Failed to remove command output file: {}",
                    source_path.display()
                )
            })?;
        }
    }

    notification_manager.send_backup_result(&config.server_location, &config.date, &outcomes);

    if !failures.is_empty() {
        anyhow::bail!(
            "Backup finished with {} failure(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    info!("Backup uploaded successfully");
    Ok(())
}

fn build_archive_path(work_dir: &Path, archive_name: &str, date: &str) -> PathBuf {
    let base_file_name = format!("{archive_name}-{date}.tar.zst");
    let mut output_path = work_dir.join(base_file_name);
    if output_path.exists() {
        let mut counter = 1usize;
        loop {
            let candidate = work_dir.join(format!("{archive_name}-{date}-{counter}.tar.zst"));
            if !candidate.exists() {
                output_path = candidate;
                break;
            }
            counter += 1;
        }
    }
    output_path
}

fn resolve_source_path(item: &BackupItem, ctx: &TemplateContext) -> Result<PathBuf> {
    let candidate = item
        .source_path
        .as_deref()
        .or(item.source_dir.as_deref())
        .context("Missing source_path/source_dir in backup item")?;
    let expanded = ctx.expand(candidate);
    let trimmed = expanded.trim();
    if trimmed.is_empty() {
        anyhow::bail!("source_path/source_dir cannot be empty");
    }
    Ok(PathBuf::from(trimmed))
}

fn run_command(command: &str, workdir: Option<&str>) -> Result<()> {
    let mut cmd = if cfg!(windows) {
        let mut command_builder = Command::new("cmd");
        command_builder.args(["/C", command]);
        command_builder
    } else {
        let mut command_builder = Command::new("sh");
        command_builder.args(["-c", command]);
        command_builder
    };

    if let Some(dir) = workdir {
        let dir_path = Path::new(dir);
        if !dir_path.is_dir() {
            anyhow::bail!("Command workdir is not a directory: {}", dir);
        }
        cmd.current_dir(dir_path);
    }

    let status = cmd
        .status()
        .with_context(|| format!("Failed to run command: {}", command))?;
    if !status.success() {
        anyhow::bail!("Command failed with exit code: {}", status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_archive_path_uses_work_dir_and_avoids_collisions() {
        let dir = std::env::temp_dir().join(format!(
            "backup_to_cloud_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();

        let p1 = build_archive_path(&dir, "demo", "20260211");
        assert_eq!(p1, dir.join("demo-20260211.tar.zst"));
        fs::write(&p1, b"existing").unwrap();

        let p2 = build_archive_path(&dir, "demo", "20260211");
        assert_eq!(p2, dir.join("demo-20260211-1.tar.zst"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_context_expands_item_placeholders() {
        let mut ctx = TemplateContext::with_date("20260211");
        ctx.insert("archive_name", "demo");
        assert_eq!(ctx.expand("/a/{archive_name}/{date}"), "/a/demo/20260211");
    }
}
