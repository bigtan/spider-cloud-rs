use serde::Deserialize;
use std::fs;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct AccountConfig {
    pub accounts: Vec<String>,
    pub passwords: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChanifyConfig {
    #[serde(default)]
    pub enabled: bool,
    pub token: Option<String>,
    #[serde(default = "default_chanify_url")]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EmailConfig {
    #[serde(default)]
    pub enabled: bool,
    pub sender: Option<String>,
    pub password: Option<String>,
    pub recipient: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PushgoConfig {
    #[serde(default)]
    pub enabled: bool,
    pub api_token: Option<String>,
    #[serde(default = "default_pushgo_url")]
    pub url: String,
    pub channel_id: Option<String>,
    pub password: Option<String>,
    pub hex_key: Option<String>,
    pub icon: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BaiduOcrConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default = "default_baidu_ocr_url")]
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptchaProvider {
    #[default]
    Baidu,
    Onnx,
    OnnxThenBaidu,
    BaiduThenOnnx,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaptchaConfig {
    #[serde(default)]
    pub provider: CaptchaProvider,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnnxCaptchaConfig {
    #[serde(default = "default_onnx_captcha_model_path")]
    pub model_path: String,
    #[serde(default = "default_onnx_captcha_vocab_path")]
    pub vocab_path: String,
    #[serde(default = "default_onnx_captcha_length")]
    pub captcha_length: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct NotifierConfig {
    #[serde(default)]
    pub chanify: ChanifyConfig,
    #[serde(default)]
    pub email: EmailConfig,
    #[serde(default)]
    pub pushgo: PushgoConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub captcha: CaptchaConfig,
    #[serde(default)]
    pub baidu_ocr: BaiduOcrConfig,
    #[serde(default)]
    pub onnx_captcha: OnnxCaptchaConfig,
    pub account: AccountConfig,
    #[serde(default)]
    pub notifier: NotifierConfig,
}

impl Default for ChanifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            url: default_chanify_url(),
        }
    }
}

impl Default for PushgoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_token: None,
            url: default_pushgo_url(),
            channel_id: None,
            password: None,
            hex_key: None,
            icon: None,
            image: None,
        }
    }
}

impl Default for BaiduOcrConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            url: default_baidu_ocr_url(),
        }
    }
}

impl Default for CaptchaConfig {
    fn default() -> Self {
        Self {
            provider: CaptchaProvider::Baidu,
        }
    }
}

impl Default for OnnxCaptchaConfig {
    fn default() -> Self {
        Self {
            model_path: default_onnx_captcha_model_path(),
            vocab_path: default_onnx_captcha_vocab_path(),
            captcha_length: default_onnx_captcha_length(),
        }
    }
}

pub fn load_config(path: &str) -> Option<Config> {
    debug!("Reading CFMMC configuration from TOML: {}", path);

    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            error!("Failed to read config file {}: {}", path, err);
            return None;
        }
    };

    let config: Config = match toml::from_str(&contents) {
        Ok(config) => config,
        Err(err) => {
            error!("Failed to parse config file {}: {}", path, err);
            return None;
        }
    };

    if config.account.accounts.len() != config.account.passwords.len()
        || config.account.accounts.is_empty()
    {
        warn!(
            "Account and password counts don't match or are empty: accounts={}, passwords={}",
            config.account.accounts.len(),
            config.account.passwords.len()
        );
        return None;
    }

    info!(
        "Account configuration loaded successfully for {} accounts",
        config.account.accounts.len()
    );
    Some(config)
}

fn default_chanify_url() -> String {
    "https://chanify.estan.cn/v1/sender/".to_string()
}

fn default_pushgo_url() -> String {
    "https://pushgo.estan.cn/message".to_string()
}

fn default_baidu_ocr_url() -> String {
    "https://aip.baidubce.com/rest/2.0/ocr/v1/accurate_basic".to_string()
}

fn default_onnx_captcha_model_path() -> String {
    "models/model.onnx".to_string()
}

fn default_onnx_captcha_vocab_path() -> String {
    "models/vocab.txt".to_string()
}

fn default_onnx_captcha_length() -> usize {
    6
}
