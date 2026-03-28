use anyhow::{Result, anyhow};
use rand::RngExt;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONNECTION, HeaderMap, HeaderValue, USER_AGENT};
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::captcha::recognizer::CaptchaRecognizer;

pub struct CFMMCCollector<'a> {
    pub user_id: String,
    pub password: String,
    pub client: Client,
    pub captcha_recognizer: &'a mut dyn CaptchaRecognizer,
    pub base_url: String,
    pub debug: bool,
    pub request_delay_min_ms: u64,
    pub request_delay_max_ms: u64,
}

impl<'a> CFMMCCollector<'a> {
    pub fn new(
        user_id: String,
        password: String,
        captcha_recognizer: &'a mut dyn CaptchaRecognizer,
        debug: bool,
    ) -> Self {
        let client = Client::builder().cookie_store(true).build().unwrap();
        let request_delay_min_ms = env::var("CFMMC_REQUEST_DELAY_MIN_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(500);
        let request_delay_max_ms = env::var("CFMMC_REQUEST_DELAY_MAX_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1500);

        let (request_delay_min_ms, request_delay_max_ms) =
            if request_delay_min_ms <= request_delay_max_ms {
                (request_delay_min_ms, request_delay_max_ms)
            } else {
                warn!(
                    "Invalid delay range: min {} > max {}, swapping values",
                    request_delay_min_ms, request_delay_max_ms
                );
                (request_delay_max_ms, request_delay_min_ms)
            };

        info!("Creating CFMMCCollector for user: {}", user_id);
        info!(
            "Request random delay range enabled: {}-{} ms",
            request_delay_min_ms, request_delay_max_ms
        );
        if debug {
            debug!("Debug mode enabled for CFMMCCollector");
        }

        Self {
            user_id,
            password,
            client,
            captcha_recognizer,
            base_url: "https://investorservice.cfmmc.com".to_string(),
            debug,
            request_delay_min_ms,
            request_delay_max_ms,
        }
    }

    fn jitter_delay(&self, scene: &str) {
        let mut rng = rand::rng();
        let delay_ms = rng.random_range(self.request_delay_min_ms..=self.request_delay_max_ms);
        debug!(
            "Applying random request delay before {}: {} ms",
            scene, delay_ms
        );
        thread::sleep(Duration::from_millis(delay_ms));
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"));
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6"),
        );
        headers
    }

    pub fn login(&mut self) -> Result<()> {
        info!("Starting login process for user: {}", self.user_id);

        // Step 1: Get login page
        let login_url = format!("{}/login.do", self.base_url);
        debug!("Fetching login page: {}", login_url);

        self.jitter_delay("GET /login.do");
        let res = self.client.get(&login_url).headers(self.headers()).send()?;
        let content = res.text()?;
        debug!("Login page fetched successfully");

        // Step 2: Extract CAPTCHA token
        let token = Self::extract_vericode_token(&content)?;
        debug!("CAPTCHA token extracted: {}", token);

        // Step 3: Try login with recognized CAPTCHA
        for attempt in 0..10 {
            info!("Login attempt {} for user: {}", attempt + 1, self.user_id);

            let captcha_url = format!("{}/veriCode.do?t={}", self.base_url, token);
            debug!("Fetching CAPTCHA image: {}", captcha_url);

            self.jitter_delay("GET /veriCode.do");
            let captcha_img = self.client.get(&captcha_url).send()?.bytes()?;
            let captcha_code = self.captcha_recognizer.recognize(&captcha_img)?;

            debug!("Recognized CAPTCHA: {}", captcha_code);

            if captcha_code.len() != 6 {
                warn!("Invalid CAPTCHA length: {}, expected 6", captcha_code.len());
                continue;
            }

            let params = [
                ("showSaveCookies", ""),
                ("userID", &self.user_id),
                ("password", &self.password),
                ("vericode", &captcha_code),
            ];

            debug!("Submitting login form with CAPTCHA: {}", captcha_code);
            self.jitter_delay("POST /login.do");
            let resp = self
                .client
                .post(&login_url)
                .headers(self.headers())
                .form(&params)
                .send()?;
            let resp_text = resp.text()?;

            if resp_text.contains("客户权益") && !resp_text.contains("验证码错误") {
                info!("Login successful for user: {}", self.user_id);
                return Ok(());
            } else {
                warn!(
                    "Login attempt {} failed for user: {}",
                    attempt + 1,
                    self.user_id
                );
                if resp_text.contains("验证码错误") {
                    debug!("CAPTCHA verification failed");
                }
            }
        }

        error!("Login failed after 10 attempts for user: {}", self.user_id);
        Err(anyhow!("Login failed after multiple attempts"))
    }

    pub fn set_parameter(&self, trade_date: &str) -> Result<()> {
        info!("Setting parameter for trade date: {}", trade_date);

        let url = format!("{}/customer/setParameter.do", self.base_url);
        let params = [("byType", "trade"), ("tradeDate", trade_date)];

        debug!("Sending parameter request to: {}", url);
        self.jitter_delay("POST /customer/setParameter.do");
        let resp = self
            .client
            .post(&url)
            .headers(self.headers())
            .form(&params)
            .send()?;
        let text = resp.text()?;

        if text.contains("客户权益") && !text.contains("验证码错误") {
            info!("Parameter set successfully for trade date: {}", trade_date);
            Ok(())
        } else {
            error!("Failed to set parameter for trade date: {}", trade_date);
            if self.debug {
                debug!("Response text: {}", text);
            }
            Err(anyhow!("Set parameter failed"))
        }
    }

    pub fn download_xls(&self, save_path: &Path) -> Result<()> {
        info!("Starting XLS download to: {:?}", save_path);

        let url = format!(
            "{}/customer/setupViewCustomerDetailFromCompanyWithExcel.do?version=7",
            self.base_url
        );
        debug!("Downloading from URL: {}", url);

        self.jitter_delay("GET setupViewCustomerDetailFromCompanyWithExcel.do");
        let resp = self.client.get(&url).headers(self.headers()).send()?;
        let bytes = resp.bytes()?;

        if bytes.len() > 100 {
            let mut file = File::create(save_path)?;
            file.write_all(&bytes)?;
            info!(
                "XLS file downloaded successfully to: {:?} (size: {} bytes)",
                save_path,
                bytes.len()
            );
            Ok(())
        } else {
            error!(
                "Downloaded file too small: {} bytes, expected > 100",
                bytes.len()
            );
            Err(anyhow!("Downloaded file too small or invalid"))
        }
    }

    fn extract_vericode_token(html: &str) -> Result<String> {
        debug!("Extracting CAPTCHA token from HTML");

        let flag = "src=\"/veriCode.do?t=";
        if let Some(idx) = html.find(flag) {
            let start = idx + flag.len();
            if let Some(end) = html[start..].find('"') {
                let token = html[start..start + end].to_string();
                debug!("CAPTCHA token extracted successfully: {}", token);
                return Ok(token);
            }
        }

        error!("Failed to extract CAPTCHA token from HTML");
        Err(anyhow!("Failed to extract CAPTCHA token"))
    }
}
