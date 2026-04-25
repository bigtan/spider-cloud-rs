use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use url::Url;

const TOKEN_URL: &str = "https://aip.baidubce.com/oauth/2.0/token";
const DEFAULT_OCR_URL: &str = "https://aip.baidubce.com/rest/2.0/ocr/v1/accurate_basic";
const TOKEN_EXPIRY_SKEW: Duration = Duration::from_secs(300);

pub trait CaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String>;
}

#[derive(Debug, Clone)]
pub struct BaiduOcrOptions {
    pub api_key: String,
    pub secret_key: String,
    pub ocr_url: String,
    pub debug: bool,
}

pub struct BaiduOcrCaptchaRecognizer {
    client: Client,
    options: BaiduOcrOptions,
    token: Option<CachedToken>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OcrResponse {
    words_result: Option<Vec<OcrWord>>,
    error_code: Option<u64>,
    error_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OcrWord {
    words: String,
}

impl BaiduOcrCaptchaRecognizer {
    pub fn new(options: BaiduOcrOptions) -> Result<Self> {
        if options.api_key.trim().is_empty() {
            bail!("Baidu OCR API key is empty");
        }
        if options.secret_key.trim().is_empty() {
            bail!("Baidu OCR secret key is empty");
        }
        if options.ocr_url.trim().is_empty() {
            bail!("Baidu OCR URL is empty");
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .context("Failed to build Baidu OCR HTTP client")?;

        info!("Baidu OCR CAPTCHA recognizer initialized");
        Ok(Self {
            client,
            options,
            token: None,
        })
    }

    fn access_token(&mut self) -> Result<String> {
        if let Some(token) = &self.token
            && Instant::now() < token.expires_at
        {
            return Ok(token.value.clone());
        }

        self.fetch_access_token()
    }

    fn fetch_access_token(&mut self) -> Result<String> {
        if !self.has_token_credentials() {
            bail!("Baidu OCR api_key/secret_key are required to fetch access token");
        }

        debug!("Fetching Baidu OCR access token");
        let mut token_url = Url::parse(TOKEN_URL).context("Invalid Baidu OCR token URL")?;
        token_url
            .query_pairs_mut()
            .append_pair("grant_type", "client_credentials")
            .append_pair("client_id", &self.options.api_key)
            .append_pair("client_secret", &self.options.secret_key);

        let response = self
            .client
            .post(token_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .context("Failed to request Baidu OCR access token")?
            .error_for_status()
            .context("Baidu OCR token endpoint returned an error status")?
            .json::<TokenResponse>()
            .context("Failed to parse Baidu OCR access token response")?;

        if let Some(error) = response.error {
            let description = response.error_description.unwrap_or_default();
            bail!("Baidu OCR token error: {error} {description}");
        }

        let value = response
            .access_token
            .ok_or_else(|| anyhow!("Baidu OCR token response missing access_token"))?;
        let expires_in = response.expires_in.unwrap_or(2_592_000);
        let ttl = Duration::from_secs(expires_in).saturating_sub(TOKEN_EXPIRY_SKEW);
        self.token = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + ttl,
        });

        Ok(value)
    }

    fn has_token_credentials(&self) -> bool {
        !self.options.api_key.trim().is_empty() && !self.options.secret_key.trim().is_empty()
    }

    fn request_ocr(&self, access_token: &str, image: &str) -> Result<OcrResponse> {
        let mut ocr_url = Url::parse(&self.options.ocr_url).context("Invalid Baidu OCR URL")?;
        ocr_url
            .query_pairs_mut()
            .append_pair("access_token", access_token);

        self.client
            .post(ocr_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("image", image),
                ("detect_direction", "false"),
                ("paragraph", "false"),
                ("probability", "false"),
                ("multidirectional_recognize", "false"),
            ])
            .send()
            .context("Failed to request Baidu OCR recognition")?
            .error_for_status()
            .context("Baidu OCR endpoint returned an error status")?
            .json::<OcrResponse>()
            .context("Failed to parse Baidu OCR recognition response")
    }
}

impl Default for BaiduOcrOptions {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            ocr_url: DEFAULT_OCR_URL.to_string(),
            debug: false,
        }
    }
}

impl CaptchaRecognizer for BaiduOcrCaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String> {
        debug!(
            "Starting CAPTCHA recognition through Baidu OCR, image size: {} bytes",
            image_bytes.len()
        );

        if self.options.debug {
            if let Err(err) = std::fs::write("debug_captcha.jpg", image_bytes) {
                warn!("Failed to save debug CAPTCHA image: {}", err);
            } else {
                debug!("Debug CAPTCHA image saved to debug_captcha.jpg");
            }
        }

        let image = BASE64_STANDARD.encode(image_bytes);
        let access_token = self.access_token()?;
        let mut response = self.request_ocr(&access_token, &image)?;

        if response.error_code.is_some_and(is_token_error) {
            warn!("Baidu OCR access token is invalid or expired, refreshing and retrying once");
            self.token = None;
            let access_token = self.fetch_access_token()?;
            response = self.request_ocr(&access_token, &image)?;
        }

        if let Some(error_code) = response.error_code {
            let message = response.error_msg.unwrap_or_default();
            bail!("Baidu OCR recognition error {error_code}: {message}");
        }

        let result = response
            .words_result
            .unwrap_or_default()
            .into_iter()
            .map(|word| word.words)
            .collect::<Vec<_>>()
            .join("")
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();

        if result.len() == 6 {
            info!("CAPTCHA recognition successful: {}", result);
        } else {
            warn!(
                "CAPTCHA recognition returned unexpected length: {} (expected 6): {}",
                result.len(),
                result
            );
        }

        Ok(result)
    }
}

fn is_token_error(error_code: u64) -> bool {
    matches!(error_code, 110 | 111)
}
