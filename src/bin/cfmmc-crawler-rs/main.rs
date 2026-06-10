use indexmap::IndexMap;
use spider_cloud_rs::logging;
use std::path::Path;
use tracing::{error, info};

mod captcha;
mod cfmmc;
mod env_config;
mod notifier;
mod xls_parser;

use anyhow::{Context, Result};
use captcha::recognizer::{
    BaiduOcrCaptchaRecognizer, BaiduOcrOptions, CaptchaRecognizer, FallbackCaptchaRecognizer,
    OnnxCaptchaOptions, OnnxCaptchaRecognizer,
};
use cfmmc::CFMMCCollector;
use env_config::{BaiduOcrConfig, CaptchaProvider, OnnxCaptchaConfig, load_config};
use notifier::{ChanifyNotifier, Notifier, PushgoNotifier, QQEmailNotifier};
use std::path::PathBuf;
use xls_parser::extract_daily_values;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // 日志可能尚未初始化，stderr 兜底；定时任务依赖非零退出码感知失败
            eprintln!("Error: {err:#}");
            error!("CFMMC Crawler failed: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().unwrap_or_else(|| "config.toml".to_string());
    if args.next().is_some() {
        anyhow::bail!("Too many arguments");
    }

    let config = load_config(&config_path)
        .with_context(|| format!("Account config missing or invalid: {config_path}"))?;

    logging::init_with_file("cfmmc", config.debug).context("Failed to initialize logging")?;

    let debug = config.debug;
    if debug {
        info!("Debug logging enabled");
    }
    info!("CFMMC Crawler started");

    let account_config = config.account;
    let captcha_config = config.captcha;
    let baidu_ocr_config = config.baidu_ocr;
    let onnx_captcha_config = config.onnx_captcha;
    let notifier_config = config.notifier;
    info!(
        "Notifier config loaded: Chanify={}, Email={}, Pushgo={}",
        notifier_config.chanify.enabled,
        notifier_config.email.enabled,
        notifier_config.pushgo.enabled
    );

    // Initialize notifiers
    let mut notifiers: Vec<Box<dyn Notifier>> = Vec::new();

    if notifier_config.chanify.enabled {
        info!("Initializing Chanify notifier");
        notifiers.push(Box::new(ChanifyNotifier {
            chanify_url: notifier_config.chanify.url,
            chanify_token: notifier_config
                .chanify
                .token
                .context("Chanify enabled but token not configured")?,
        }));
    }

    if notifier_config.email.enabled {
        info!("Initializing Email notifier");
        notifiers.push(Box::new(QQEmailNotifier {
            sender: notifier_config
                .email
                .sender
                .context("Email enabled but sender not configured")?,
            password: notifier_config
                .email
                .password
                .context("Email enabled but password not configured")?,
            recipient: notifier_config
                .email
                .recipient
                .context("Email enabled but recipient not configured")?,
        }));
    }

    if notifier_config.pushgo.enabled {
        info!("Initializing Pushgo notifier");
        notifiers.push(Box::new(PushgoNotifier {
            api_token: notifier_config
                .pushgo
                .api_token
                .context("Pushgo enabled but API token not configured")?,
            url: notifier_config.pushgo.url,
            channel_id: notifier_config
                .pushgo
                .channel_id
                .context("Pushgo enabled but channel id not configured")?,
            password: notifier_config
                .pushgo
                .password
                .context("Pushgo enabled but password not configured")?,
            hex_key: notifier_config
                .pushgo
                .hex_key
                .context("Pushgo enabled but hex key not configured")?,
            icon: notifier_config.pushgo.icon,
            image: notifier_config.pushgo.image,
        }));
    }

    // 初始化验证码识别器
    info!("Initializing CAPTCHA recognizer");

    let mut recognizer = build_captcha_recognizer(
        captcha_config.provider,
        baidu_ocr_config,
        onnx_captcha_config,
        debug,
    )
    .context("Failed to initialize CAPTCHA recognizer")?;
    info!("CAPTCHA recognizer initialized successfully");

    // 对每个账号进行爬取和通知
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    //let date = "2025-07-16".to_string();
    info!("Processing accounts for date: {}", date);

    let mut accounts_data = IndexMap::new();
    let mut any_settlement_found = false;
    let mut failures: Vec<String> = Vec::new();
    for (account, password) in account_config
        .accounts
        .iter()
        .zip(account_config.passwords.iter())
    {
        info!("Processing account: {}", account);

        let xls_folder = "data";
        if let Err(e) = std::fs::create_dir_all(xls_folder) {
            error!("Failed to create data folder for account {}: {}", account, e);
            failures.push(format!("[{account}] create data folder failed: {e}"));
            continue;
        }

        let xls_path = format!("{xls_folder}/{account}_{date}.xlsx");

        // 检查本地文件是否存在
        if !std::path::Path::new(&xls_path).exists() {
            info!("Local file not found, downloading: {}", xls_path);

            let mut collector = match CFMMCCollector::new(
                account.clone(),
                password.clone(),
                recognizer.as_mut(),
                debug,
            ) {
                Ok(collector) => collector,
                Err(e) => {
                    error!("Failed to create collector for account {}: {}", account, e);
                    failures.push(format!("[{account}] create collector failed: {e}"));
                    continue;
                }
            };

            if let Err(e) = collector.login() {
                error!("Login failed for account {}: {}", account, e);
                failures.push(format!("[{account}] login failed: {e}"));
                continue;
            }
            info!("Login successful for account: {}", account);

            if let Err(e) = collector.set_parameter(&date) {
                error!("Set parameter failed for account {}: {}", account, e);
                failures.push(format!("[{account}] set parameter failed: {e}"));
                continue;
            }
            info!("Parameter set successfully for account: {}", account);

            if let Err(e) = collector.download_xls(Path::new(&xls_path)) {
                error!("Download failed for account {}: {}", account, e);
                failures.push(format!("[{account}] download failed: {e}"));
                continue;
            }
            info!("Downloaded XLS file for account: {}", account);
        } else {
            info!(
                "Using existing local file for account {}: {}",
                account, xls_path
            );
        }

        match extract_daily_values(&xls_path, "客户交易结算日报") {
            Some((values, found_keys)) if found_keys > 0 => {
                info!("Extracted values for account {}: {:?}", account, values);
                any_settlement_found = true;
                accounts_data.insert(account.clone(), Some(values));
            }
            Some(_) => {
                info!("No settlement values found for account {}", account);
                accounts_data.insert(account.clone(), None);
            }
            None => {
                error!("Failed to extract values for account {}", account);
                failures.push(format!("[{account}] failed to parse {xls_path}"));
                accounts_data.insert(account.clone(), None);
            }
        }
    }

    if any_settlement_found {
        // Send notifications
        info!("Sending notifications to {} notifiers", notifiers.len());
        for notifier in &notifiers {
            match notifier.send(&date, &accounts_data) {
                true => info!("Notification sent successfully"),
                false => {
                    error!("Failed to send notification");
                    failures.push("notification send failed".to_string());
                }
            }
        }
    } else {
        info!(
            "No settlement info found for date {}, skipping notifications",
            date
        );
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "CFMMC Crawler finished with {} failure(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    info!("CFMMC Crawler completed successfully");
    Ok(())
}

fn build_captcha_recognizer(
    provider: CaptchaProvider,
    baidu_config: BaiduOcrConfig,
    onnx_config: OnnxCaptchaConfig,
    debug: bool,
) -> Result<Box<dyn CaptchaRecognizer>> {
    match provider {
        CaptchaProvider::Baidu => {
            info!("Using Baidu OCR CAPTCHA recognizer");
            Ok(Box::new(build_baidu_recognizer(baidu_config, debug)?))
        }
        CaptchaProvider::Onnx => {
            info!("Using local ONNX CAPTCHA recognizer");
            Ok(Box::new(build_onnx_recognizer(onnx_config, debug)?))
        }
        CaptchaProvider::OnnxThenBaidu => {
            info!("Using local ONNX CAPTCHA recognizer with Baidu OCR fallback");
            let expected_len = onnx_config.captcha_length;
            Ok(Box::new(FallbackCaptchaRecognizer::new(
                Box::new(build_onnx_recognizer(onnx_config, debug)?),
                Box::new(build_baidu_recognizer(baidu_config, debug)?),
                expected_len,
            )))
        }
        CaptchaProvider::BaiduThenOnnx => {
            info!("Using Baidu OCR CAPTCHA recognizer with local ONNX fallback");
            let expected_len = onnx_config.captcha_length;
            Ok(Box::new(FallbackCaptchaRecognizer::new(
                Box::new(build_baidu_recognizer(baidu_config, debug)?),
                Box::new(build_onnx_recognizer(onnx_config, debug)?),
                expected_len,
            )))
        }
    }
}

fn build_baidu_recognizer(
    config: BaiduOcrConfig,
    debug: bool,
) -> Result<BaiduOcrCaptchaRecognizer> {
    BaiduOcrCaptchaRecognizer::new(BaiduOcrOptions {
        api_key: config.api_key,
        secret_key: config.secret_key,
        ocr_url: config.url,
        debug,
    })
}

fn build_onnx_recognizer(config: OnnxCaptchaConfig, debug: bool) -> Result<OnnxCaptchaRecognizer> {
    OnnxCaptchaRecognizer::new(OnnxCaptchaOptions {
        model_path: PathBuf::from(config.model_path),
        vocab_path: PathBuf::from(config.vocab_path),
        captcha_length: config.captcha_length,
        debug,
    })
}
