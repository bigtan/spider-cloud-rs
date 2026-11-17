use crate::Result;
use crate::fs::validate_safe_name;
use anyhow::Context;
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tracing_subscriber::layer::Layer;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize stderr tracing subscriber using `RUST_LOG` if set, otherwise `debug` or `info`.
pub fn init_default(debug: bool) -> Result<()> {
    let filter = default_env_filter(debug)?;
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .try_init()
        .map_err(|err| anyhow::anyhow!("Failed to initialize tracing subscriber: {err}"))
}

/// Initialize stderr + file logging inside `log_dir` (`{prefix}_{timestamp}.log`, mode `0o600` on Unix),
/// pruning `{prefix}_*.log` files older than `retain_days` days when `retain_days > 0`.
pub fn init_with_file(
    prefix: &str,
    log_dir: &Path,
    debug: bool,
    retain_days: u64,
) -> Result<PathBuf> {
    let safe_prefix = validate_safe_name(prefix, "log prefix")?;
    fs::create_dir_all(log_dir)
        .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    if retain_days > 0 {
        let _ = prune_old_logs(&safe_prefix, log_dir, retain_days);
    }

    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let log_file_path = log_dir.join(format!("{safe_prefix}_{timestamp}.log"));

    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(&log_file_path)
        .with_context(|| format!("Failed to open log file: {}", log_file_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&log_file_path, fs::Permissions::from_mode(0o600))?;
    }

    let console_filter = default_env_filter(debug)?;
    let file_filter = default_env_filter(debug)?;

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_filter(console_filter);

    let file_layer = fmt::layer()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_filter(file_filter);

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .try_init()
        .map_err(|err| anyhow::anyhow!("Failed to initialize tracing subscriber: {err}"))?;

    Ok(log_file_path)
}

/// Remove `{prefix}_*.log` files inside `log_dir` older than `retain_days` days.
pub fn prune_old_logs(prefix: &str, log_dir: &Path, retain_days: u64) -> std::io::Result<usize> {
    if retain_days == 0 || !log_dir.exists() {
        return Ok(0);
    }

    let max_age = Duration::from_secs(retain_days.saturating_mul(86_400));
    let now = SystemTime::now();
    let expected_prefix = format!("{prefix}_");
    let mut removed = 0usize;

    for entry in fs::read_dir(log_dir)? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(&expected_prefix) || !name.ends_with(".log") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if let Ok(age) = now.duration_since(modified)
            && age > max_age
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }

    Ok(removed)
}

fn default_env_filter(debug: bool) -> Result<EnvFilter> {
    let default_level = if debug { "debug" } else { "info" };
    match EnvFilter::try_from_default_env() {
        Ok(filter) => Ok(filter),
        Err(_) => Ok(EnvFilter::new(default_level)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;
    use uuid::Uuid;

    #[test]
    fn prune_old_logs_only_removes_expired_matching_files() {
        let dir =
            std::env::temp_dir().join(format!("spider_common_logs_{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).unwrap();

        let old_match = dir.join("cfmmc_20200101_000000.log");
        let fresh_match = dir.join("cfmmc_20260611_000000.log");
        let old_other = dir.join("other_20200101_000000.log");

        fs::write(&old_match, b"old").unwrap();
        fs::write(&fresh_match, b"fresh").unwrap();
        fs::write(&old_other, b"other").unwrap();

        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let times = FileTimes::new().set_modified(ten_days_ago);
        OpenOptions::new()
            .write(true)
            .open(&old_match)
            .unwrap()
            .set_times(times)
            .unwrap();
        OpenOptions::new()
            .write(true)
            .open(&old_other)
            .unwrap()
            .set_times(times)
            .unwrap();

        let removed = prune_old_logs("cfmmc", &dir, 5).unwrap();
        assert_eq!(removed, 1);
        assert!(!old_match.exists());
        assert!(fresh_match.exists());
        assert!(old_other.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_old_logs_noop_when_retain_days_zero() {
        let dir =
            std::env::temp_dir().join(format!("spider_common_logs0_{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cfmmc_old.log");
        fs::write(&file, b"keep").unwrap();

        assert_eq!(prune_old_logs("cfmmc", &dir, 0).unwrap(), 0);
        assert!(file.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
