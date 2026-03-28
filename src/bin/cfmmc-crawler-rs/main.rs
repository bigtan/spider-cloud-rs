use indexmap::IndexMap;
use spider_cloud_rs::logging;
use std::path::Path;
use tracing::{error, info};

mod captcha;
mod cfmmc;
mod env_config;
mod notifier;
mod xls_parser;

use captcha::recognizer::DamoCaptchaRecognizer;
use cfmmc::CFMMCCollector;
use env_config::load_config;
use notifier::{ChanifyNotifier, Notifier, PushgoNotifier, QQEmailNotifier};
use xls_parser::extract_daily_values;

fn main() {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().unwrap_or_else(|| "config.toml".to_string());
    if args.next().is_some() {
        eprintln!("Too many arguments");
        return;
    }

    let config = match load_config(&config_path) {
        Some(config) => config,
        None => {
            eprintln!("Account config missing or invalid");
            return;
        }
    };

    if let Err(err) = logging::init_with_file("cfmmc", config.debug) {
        eprintln!("Failed to initialize logging: {}", err);
        return;
    }

    let debug = config.debug;
    if debug {
        info!("Debug logging enabled");
    }
    info!("CFMMC Crawler started");

    let account_config = config.account;
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
                .expect("Chanify token not configured"),
        }));
    }

    if notifier_config.email.enabled {
        info!("Initializing Email notifier");
        notifiers.push(Box::new(QQEmailNotifier {
            sender: notifier_config
                .email
                .sender
                .expect("Email sender not configured"),
            password: notifier_config
                .email
                .password
                .expect("Email password not configured"),
            recipient: notifier_config
                .email
                .recipient
                .expect("Email recipient not configured"),
        }));
    }

    if notifier_config.pushgo.enabled {
        info!("Initializing Pushgo notifier");
        notifiers.push(Box::new(PushgoNotifier {
            api_token: notifier_config
                .pushgo
                .api_token
                .expect("Pushgo API token not configured"),
            url: notifier_config.pushgo.url,
            channel_id: notifier_config
                .pushgo
                .channel_id
                .expect("Pushgo channel id not configured"),
            password: notifier_config
                .pushgo
                .password
                .expect("Pushgo password not configured"),
            hex_key: notifier_config
                .pushgo
                .hex_key
                .expect("Pushgo hex key not configured"),
            icon: notifier_config.pushgo.icon,
            image: notifier_config.pushgo.image,
        }));
    }

    // 初始化验证码识别器
    info!("Initializing CAPTCHA recognizer");
    let model_path = config.captcha_model_path;
    let vocab_path = config.captcha_vocab_path;

    let mut recognizer = match DamoCaptchaRecognizer::new(&model_path, &vocab_path, debug) {
        Ok(r) => {
            info!("CAPTCHA recognizer initialized successfully");
            r
        }
        Err(e) => {
            error!("Failed to initialize CAPTCHA recognizer: {}", e);
            return;
        }
    };

    // 对每个账号进行爬取和通知
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    //let date = "2025-07-16".to_string();
    info!("Processing accounts for date: {}", date);

    let mut accounts_data = IndexMap::new();
    let mut any_settlement_found = false;
    for (account, password) in account_config
        .accounts
        .iter()
        .zip(account_config.passwords.iter())
    {
        info!("Processing account: {}", account);

        let xls_folder = "data";
        if !std::path::Path::new(xls_folder).exists() {
            std::fs::create_dir_all(xls_folder).expect("Failed to create data folder");
            info!("Created data folder: {}", xls_folder);
        }

        let xls_path = format!("{xls_folder}/{account}_{date}.xlsx");

        // 检查本地文件是否存在
        if !std::path::Path::new(&xls_path).exists() {
            info!("Local file not found, downloading: {}", xls_path);

            let mut collector =
                CFMMCCollector::new(account.clone(), password.clone(), &mut recognizer, debug);

            if let Err(e) = collector.login() {
                error!("Login failed for account {}: {}", account, e);
                continue;
            }
            info!("Login successful for account: {}", account);

            if let Err(e) = collector.set_parameter(&date) {
                error!("Set parameter failed for account {}: {}", account, e);
                continue;
            }
            info!("Parameter set successfully for account: {}", account);

            if let Err(e) = collector.download_xls(Path::new(&xls_path)) {
                error!("Download failed for account {}: {}", account, e);
                continue;
            }
            info!("Downloaded XLS file for account: {}", account);
        } else {
            info!(
                "Using existing local file for account {}: {}",
                account, xls_path
            );
        }

        let (values, found_keys) = extract_daily_values(&xls_path, "客户交易结算日报");
        info!("Extracted values for account {}: {:?}", account, values);

        if found_keys > 0 {
            any_settlement_found = true;
        }
        accounts_data.insert(account.clone(), values);
    }

    if !any_settlement_found {
        info!(
            "No settlement info found for date {}, skipping notifications",
            date
        );
        info!("CFMMC Crawler completed successfully");
        return;
    }

    // Send notifications
    info!("Sending notifications to {} notifiers", notifiers.len());
    for notifier in &notifiers {
        match notifier.send(&date, &accounts_data) {
            true => info!("Notification sent successfully"),
            false => error!("Failed to send notification"),
        }
    }

    info!("CFMMC Crawler completed successfully");
}
