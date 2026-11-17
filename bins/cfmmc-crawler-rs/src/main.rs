#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use spider_common::cli::{CliAction, parse_cli};
use spider_common::logging;
use spider_notify::NotificationDispatcher;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

mod captcha;
mod cfmmc;
mod env_config;
mod notifier;
mod xls_parser;

use captcha::recognizer::{
    BaiduOcrCaptchaRecognizer, BaiduOcrOptions, CaptchaRecognizer, FallbackCaptchaRecognizer,
    OnnxCaptchaOptions, OnnxCaptchaRecognizer,
};
use cfmmc::CFMMCCollector;
use env_config::{BaiduOcrConfig, CaptchaProvider, OnnxCaptchaConfig, load_config};
use notifier::build_notification_payload;
use xls_parser::extract_daily_values;

const SETTLEMENT_SHEET_NAME: &str = "客户交易结算日报";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // Logging may not be initialized yet; always write to stderr for cron/CI.
            eprintln!("Error: {err:#}");
            error!("CFMMC Crawler failed: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = match parse_cli(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        "CFMMC daily settlement statement crawler and notifier",
        std::env::args().skip(1),
    )? {
        CliAction::PrintHelp(help) => {
            println!("{help}");
            return Ok(());
        }
        CliAction::PrintVersion(version) => {
            println!("{version}");
            return Ok(());
        }
        CliAction::Run(args) => args,
    };

    let (config, target_date) = load_config(&cli.config_path, cli.date_override.as_deref())
        .with_context(|| format!("Account config missing or invalid: {}", cli.config_path))?;

    if cli.dry_run {
        logging::init_default(config.debug).context("Failed to initialize logging")?;
    } else {
        logging::init_with_file(
            "cfmmc",
            Path::new(&config.log_dir),
            config.debug,
            config.log_retain_days,
        )
        .context("Failed to initialize logging")?;
    }

    let debug = config.debug;
    if debug {
        info!("Debug logging enabled");
    }
    info!("CFMMC Crawler started");
    info!(
        "Account configuration loaded successfully for {} accounts",
        config.account.accounts.len()
    );

    let dispatcher = NotificationDispatcher::from_config(&config.notifier)
        .context("Failed to initialize notification dispatcher")?;
    info!(
        "Notifier config loaded: Chanify={}, Email={}, Pushgo={}",
        config.notifier.chanify.enabled,
        config.notifier.email.enabled,
        config.notifier.pushgo.enabled
    );

    let date = target_date.format("%Y-%m-%d").to_string();

    if cli.dry_run {
        config.validate_runtime_paths()?;
        if config.captcha.provider.uses_baidu() {
            let _ = build_baidu_recognizer(
                &config.baidu_ocr,
                config.onnx_captcha.captcha_length,
                debug,
                PathBuf::from(&config.log_dir),
            )?;
        }
        info!(
            "Dry run validation succeeded: date={}, accounts={}, data_dir={}, log_dir={}, captcha_provider={:?}, notifiers_active={}",
            date,
            config.account.accounts.len(),
            config.data_dir,
            config.log_dir,
            config.captcha.provider,
            dispatcher.has_channels()
        );
        return Ok(());
    }

    config.validate_runtime_paths()?;

    info!("Initializing CAPTCHA recognizer");
    let log_dir = PathBuf::from(&config.log_dir);
    let captcha_length = config.onnx_captcha.captcha_length;
    let mut recognizer = build_captcha_recognizer(
        config.captcha.provider,
        &config.baidu_ocr,
        &config.onnx_captcha,
        debug,
        log_dir,
    )
    .context("Failed to initialize CAPTCHA recognizer")?;
    info!("CAPTCHA recognizer initialized successfully");

    info!("Processing accounts for date: {}", date);

    let data_dir = Path::new(&config.data_dir);
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("Failed to create data folder: {}", data_dir.display()))?;

    let mut accounts_data = IndexMap::new();
    let mut any_settlement_found = false;
    let mut failures: Vec<String> = Vec::new();

    for (account, password) in config
        .account
        .accounts
        .iter()
        .zip(config.account.passwords.iter())
    {
        info!("Processing account: {}", account);
        let xls_path = data_dir.join(format!("{account}_{date}.xlsx"));

        let extraction = load_or_fetch_and_extract(&xls_path, account, |target_path| {
            let mut collector = CFMMCCollector::new(
                account.clone(),
                password.clone(),
                recognizer.as_mut(),
                captcha_length,
                debug,
            )
            .context("create collector failed")?;

            collector.login().context("login failed")?;
            info!("Login successful for account: {}", account);

            collector
                .set_parameter(&date)
                .context("set parameter failed")?;
            info!("Parameter set successfully for account: {}", account);

            collector
                .download_xls(target_path)
                .context("download failed")?;
            info!("Downloaded XLS file for account: {}", account);
            Ok(())
        });

        match extraction {
            Ok(Some((values, found_keys))) if found_keys > 0 => {
                // Keep exact financial amounts at debug! only.
                tracing::debug!("Extracted values for account {}: {:?}", account, values);
                info!(
                    "Extracted {} settlement value(s) for account {}",
                    found_keys, account
                );
                any_settlement_found = true;
                accounts_data.insert(account.clone(), Some(values));
            }
            Ok(Some(_)) => {
                info!("No settlement values found for account {}", account);
                accounts_data.insert(account.clone(), None);
            }
            Ok(None) => {
                error!("Failed to extract values for account {}", account);
                failures.push(format!(
                    "[{account}] failed to parse {}",
                    xls_path.display()
                ));
                accounts_data.insert(account.clone(), None);
            }
            Err(err) => {
                error!("Failed to process account {}: {:#}", account, err);
                failures.push(format!("[{account}] {err:#}"));
            }
        }
    }

    if any_settlement_found {
        if dispatcher.has_channels() {
            let payload = build_notification_payload(&date, &accounts_data);
            info!("Dispatching notifications concurrently");
            for attempt in dispatcher.dispatch(&payload) {
                if attempt.success {
                    info!("{} notification sent successfully", attempt.channel);
                } else {
                    let err_msg = attempt
                        .error
                        .unwrap_or_else(|| "unknown notification error".to_string());
                    error!("{} notification failed: {}", attempt.channel, err_msg);
                    failures.push(format!(
                        "{} notification failed: {err_msg}",
                        attempt.channel
                    ));
                }
            }
        } else {
            info!("No notification channels enabled; skipping notifications");
        }
    } else {
        info!(
            "No settlement info found for date {}, skipping notifications",
            date
        );
    }

    if !failures.is_empty() {
        bail!(
            "CFMMC Crawler finished with {} failure(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    info!("CFMMC Crawler completed successfully");
    Ok(())
}

/// Read settlement values from `xls_path`, downloading it if absent.
///
/// If a pre-existing cached file fails extraction (`None`), logs a warning,
/// removes the corrupted cache file, and invokes `fetch_fn` once to recover.
pub(crate) fn load_or_fetch_and_extract<F>(
    xls_path: &Path,
    account: &str,
    mut fetch_fn: F,
) -> Result<Option<(HashMap<String, f64>, usize)>>
where
    F: FnMut(&Path) -> Result<()>,
{
    let used_cache = xls_path.exists();
    if used_cache {
        info!(
            "Using existing local file for account {}: {}",
            account,
            xls_path.display()
        );
    } else {
        info!("Local file not found, downloading: {}", xls_path.display());
        fetch_fn(xls_path)?;
    }

    match extract_daily_values(xls_path, SETTLEMENT_SHEET_NAME) {
        Some(res) => Ok(Some(res)),
        None if used_cache => {
            warn!(
                "Existing cached file {} for account {} is corrupted or unreadable; removing and re-downloading once",
                xls_path.display(),
                account
            );
            if let Err(err) = std::fs::remove_file(xls_path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err).with_context(|| {
                    format!(
                        "Failed to remove corrupted cached file: {}",
                        xls_path.display()
                    )
                });
            }
            fetch_fn(xls_path)?;
            Ok(extract_daily_values(xls_path, SETTLEMENT_SHEET_NAME))
        }
        None => Ok(None),
    }
}

fn build_captcha_recognizer(
    provider: CaptchaProvider,
    baidu_config: &BaiduOcrConfig,
    onnx_config: &OnnxCaptchaConfig,
    debug: bool,
    log_dir: PathBuf,
) -> Result<Box<dyn CaptchaRecognizer>> {
    let expected_len = onnx_config.captcha_length;
    match provider {
        CaptchaProvider::Baidu => {
            info!("Using Baidu OCR CAPTCHA recognizer");
            Ok(Box::new(build_baidu_recognizer(
                baidu_config,
                expected_len,
                debug,
                log_dir,
            )?))
        }
        CaptchaProvider::Onnx => {
            info!("Using local ONNX CAPTCHA recognizer");
            Ok(Box::new(build_onnx_recognizer(
                onnx_config,
                debug,
                log_dir,
            )?))
        }
        CaptchaProvider::OnnxThenBaidu => {
            info!("Using local ONNX CAPTCHA recognizer with Baidu OCR fallback");
            Ok(Box::new(FallbackCaptchaRecognizer::new(
                Box::new(build_onnx_recognizer(onnx_config, debug, log_dir.clone())?),
                Box::new(build_baidu_recognizer(
                    baidu_config,
                    expected_len,
                    debug,
                    log_dir,
                )?),
                expected_len,
            )))
        }
        CaptchaProvider::BaiduThenOnnx => {
            info!("Using Baidu OCR CAPTCHA recognizer with local ONNX fallback");
            Ok(Box::new(FallbackCaptchaRecognizer::new(
                Box::new(build_baidu_recognizer(
                    baidu_config,
                    expected_len,
                    debug,
                    log_dir.clone(),
                )?),
                Box::new(build_onnx_recognizer(onnx_config, debug, log_dir)?),
                expected_len,
            )))
        }
    }
}

fn build_baidu_recognizer(
    config: &BaiduOcrConfig,
    captcha_length: usize,
    debug: bool,
    log_dir: PathBuf,
) -> Result<BaiduOcrCaptchaRecognizer> {
    BaiduOcrCaptchaRecognizer::new(BaiduOcrOptions {
        api_key: config.api_key.clone(),
        secret_key: config.secret_key.clone(),
        ocr_url: config.url.clone(),
        captcha_length,
        debug,
        log_dir,
    })
}

fn build_onnx_recognizer(
    config: &OnnxCaptchaConfig,
    debug: bool,
    log_dir: PathBuf,
) -> Result<OnnxCaptchaRecognizer> {
    OnnxCaptchaRecognizer::new(OnnxCaptchaOptions {
        model_path: PathBuf::from(&config.model_path),
        vocab_path: PathBuf::from(&config.vocab_path),
        captcha_length: config.captcha_length,
        debug,
        log_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xls_parser::test_helpers::build_minimal_xlsx;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cfmmc_main_test_{name}_{nanos}.xlsx"))
    }

    #[test]
    fn corrupt_cache_is_removed_and_redownloaded_once() {
        let path = unique_temp_path("corrupt_recover");
        std::fs::write(&path, b"<html>corrupted cache</html>").unwrap();

        let valid_xlsx = build_minimal_xlsx(
            SETTLEMENT_SHEET_NAME,
            &[vec![("A", "客户权益"), ("C", "55555.00")]],
        );

        let mut calls = 0usize;
        let result = load_or_fetch_and_extract(&path, "acc1", |dest| {
            calls += 1;
            // Ensure corrupted cache file was deleted before re-downloading
            assert!(!dest.exists());
            std::fs::write(dest, &valid_xlsx)?;
            Ok(())
        })
        .unwrap()
        .unwrap();

        let _ = std::fs::remove_file(&path);
        assert_eq!(calls, 1);
        assert_eq!(result.1, 1);
        assert!((result.0["客户权益"] - 55_555.0).abs() < 1e-6);
    }

    #[test]
    fn corrupt_cache_recovery_does_not_loop_if_redownload_still_unparseable() {
        let path = unique_temp_path("corrupt_twice");
        std::fs::write(&path, b"bad1").unwrap();

        let mut calls = 0usize;
        let result = load_or_fetch_and_extract(&path, "acc1", |dest| {
            calls += 1;
            std::fs::write(dest, b"bad2")?;
            Ok(())
        })
        .unwrap();

        let _ = std::fs::remove_file(&path);
        assert_eq!(calls, 1);
        assert!(result.is_none());
    }

    #[test]
    fn valid_cache_skips_fetch_fn() {
        let path = unique_temp_path("valid_cache");
        let valid_xlsx = build_minimal_xlsx(
            SETTLEMENT_SHEET_NAME,
            &[vec![("A", "客户权益"), ("C", "100.00")]],
        );
        std::fs::write(&path, valid_xlsx).unwrap();

        let mut calls = 0usize;
        let result = load_or_fetch_and_extract(&path, "acc1", |_| {
            calls += 1;
            Ok(())
        })
        .unwrap()
        .unwrap();

        let _ = std::fs::remove_file(&path);
        assert_eq!(calls, 0);
        assert_eq!(result.1, 1);
    }
}
