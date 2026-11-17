use anyhow::{Context, Result, anyhow, bail};
use rand::RngExt;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, CONNECTION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT,
};
use spider_common::fs::AtomicTempFile;
use std::env;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use zeroize::Zeroizing;

use crate::captcha::recognizer::CaptchaRecognizer;

const DEFAULT_BASE_URL: &str = "https://investorservice.cfmmc.com";
const MAX_LOGIN_ATTEMPTS: usize = 10;

pub struct CFMMCCollector<'a> {
    pub user_id: String,
    password: Zeroizing<String>,
    pub client: Client,
    pub captcha_recognizer: &'a mut dyn CaptchaRecognizer,
    pub captcha_length: usize,
    pub base_url: String,
    pub debug: bool,
    pub request_delay_min_ms: u64,
    pub request_delay_max_ms: u64,
}

impl fmt::Debug for CFMMCCollector<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CFMMCCollector")
            .field("user_id", &self.user_id)
            .field("password", &"[REDACTED]")
            .field("captcha_length", &self.captcha_length)
            .field("base_url", &self.base_url)
            .field("debug", &self.debug)
            .field("request_delay_min_ms", &self.request_delay_min_ms)
            .field("request_delay_max_ms", &self.request_delay_max_ms)
            .finish()
    }
}

impl<'a> CFMMCCollector<'a> {
    pub fn new(
        user_id: String,
        password: String,
        captcha_recognizer: &'a mut dyn CaptchaRecognizer,
        captcha_length: usize,
        debug: bool,
    ) -> Result<Self> {
        let client = Client::builder()
            .cookie_store(true)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .context("Failed to build HTTP client")?;
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

        Ok(Self {
            user_id,
            password: Zeroizing::new(password),
            client,
            captcha_recognizer,
            captcha_length,
            base_url: DEFAULT_BASE_URL.to_string(),
            debug,
            request_delay_min_ms,
            request_delay_max_ms,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_base_url_for_test(&mut self, base_url: impl Into<String>) {
        self.base_url = base_url.into();
        self.request_delay_min_ms = 0;
        self.request_delay_max_ms = 0;
    }

    fn jitter_delay(&self, scene: &str) {
        if self.request_delay_max_ms == 0 {
            return;
        }
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

    fn fetch_login_token(&self) -> Result<String> {
        let login_url = format!("{}/login.do", self.base_url);
        debug!("Fetching login page: {}", login_url);

        self.jitter_delay("GET /login.do");
        let resp = self
            .client
            .get(&login_url)
            .headers(self.headers())
            .send()
            .context("Failed to send GET /login.do request")?;
        let resp = ensure_success(resp, "GET /login.do")?;
        let content = resp.text().context("Failed to read login page response")?;
        debug!("Login page fetched successfully");

        Self::extract_vericode_token(&content)
    }

    pub fn login(&mut self) -> Result<()> {
        info!("Starting login process for user: {}", self.user_id);

        let login_url = format!("{}/login.do", self.base_url);
        let mut token = self.fetch_login_token()?;

        for attempt in 0..MAX_LOGIN_ATTEMPTS {
            info!("Login attempt {} for user: {}", attempt + 1, self.user_id);

            let captcha_url = format!("{}/veriCode.do?t={}", self.base_url, token);
            debug!("Fetching CAPTCHA image: {}", captcha_url);

            self.jitter_delay("GET /veriCode.do");
            let captcha_resp = self
                .client
                .get(&captcha_url)
                .headers(self.headers())
                .send()
                .context("Failed to fetch CAPTCHA image")?;
            if !captcha_resp.status().is_success() {
                let status = captcha_resp.status();
                if attempt + 1 < MAX_LOGIN_ATTEMPTS {
                    warn!(
                        "CAPTCHA fetch failed with HTTP {}; refreshing login page/token",
                        status
                    );
                    token = self.fetch_login_token()?;
                    continue;
                }
                bail!("GET /veriCode.do failed with HTTP status {status}");
            }

            let captcha_ct = response_content_type(&captcha_resp);
            let captcha_img = captcha_resp
                .bytes()
                .context("Failed to read CAPTCHA image bytes")?;
            if is_html_response(captcha_ct.as_deref(), &captcha_img) {
                if attempt + 1 < MAX_LOGIN_ATTEMPTS {
                    warn!(
                        "CAPTCHA endpoint returned HTML instead of image; refreshing login token"
                    );
                    token = self.fetch_login_token()?;
                    continue;
                }
                bail!("CAPTCHA endpoint returned HTML instead of an image");
            }

            let captcha_code = self.captcha_recognizer.recognize(&captcha_img)?;
            debug!("Recognized CAPTCHA: {}", captcha_code);

            let char_count = captcha_code.chars().count();
            if char_count != self.captcha_length
                || !captcha_code.chars().all(|ch| ch.is_ascii_alphanumeric())
            {
                warn!(
                    "Invalid CAPTCHA recognition result (len={}, expected={}): {}",
                    char_count, self.captcha_length, captcha_code
                );
                if attempt + 1 < MAX_LOGIN_ATTEMPTS {
                    token = self.fetch_login_token()?;
                }
                continue;
            }

            let params = [
                ("showSaveCookies", ""),
                ("userID", self.user_id.as_str()),
                ("password", self.password.as_str()),
                ("vericode", captcha_code.as_str()),
            ];

            debug!("Submitting login form with CAPTCHA: {}", captcha_code);
            self.jitter_delay("POST /login.do");
            let resp = self
                .client
                .post(&login_url)
                .headers(self.headers())
                .form(&params)
                .send()
                .context("Failed to submit login form")?;
            let resp = ensure_success(resp, "POST /login.do")?;
            let resp_text = resp.text().context("Failed to read login response")?;

            if resp_text.contains("客户权益") && !resp_text.contains("验证码错误") {
                info!("Login successful for user: {}", self.user_id);
                return Ok(());
            }

            warn!(
                "Login attempt {} failed for user: {}",
                attempt + 1,
                self.user_id
            );
            if resp_text.contains("验证码错误") {
                debug!("CAPTCHA verification failed");
            }
            if attempt + 1 < MAX_LOGIN_ATTEMPTS {
                // Refresh login page & token in case the server invalidated the previous token.
                token = self.fetch_login_token()?;
            }
        }

        error!(
            "Login failed after {} attempts for user: {}",
            MAX_LOGIN_ATTEMPTS, self.user_id
        );
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
            .send()
            .context("Failed to send setParameter request")?;
        let resp = ensure_success(resp, "POST /customer/setParameter.do")?;
        let text = resp
            .text()
            .context("Failed to read setParameter response")?;

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
        let resp = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .context("Failed to send XLS download request")?;
        let resp = ensure_success(resp, "GET setupViewCustomerDetailFromCompanyWithExcel.do")?;
        let content_type = response_content_type(&resp);
        let bytes = resp.bytes().context("Failed to read XLS response bytes")?;

        validate_xls_response_bytes(content_type.as_deref(), &bytes)?;

        if let Some(parent) = save_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory: {}", parent.display())
            })?;
        }

        let mut temp = AtomicTempFile::new(save_path).with_context(|| {
            format!(
                "Failed to create atomic temp file for {}",
                save_path.display()
            )
        })?;
        temp.file_mut()
            .write_all(&bytes)
            .context("Failed to write XLS bytes to temp file")?;
        temp.file_mut()
            .flush()
            .context("Failed to flush XLS temp file")?;
        temp.file_mut()
            .sync_all()
            .context("Failed to sync XLS temp file")?;

        {
            // Close the calamine file handle before persist() renames the temp file (required on Windows).
            let _workbook = calamine::open_workbook_auto(temp.temp_path())
                .context("Downloaded file is not a valid Excel workbook")?;
        }

        temp.persist()
            .with_context(|| format!("Failed to persist XLS file to {}", save_path.display()))?;

        info!(
            "XLS file downloaded successfully to: {:?} (size: {} bytes)",
            save_path,
            bytes.len()
        );
        Ok(())
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

fn ensure_success(resp: Response, action: &str) -> Result<Response> {
    let status = resp.status();
    if !status.is_success() {
        bail!("{action} failed with HTTP status {status}");
    }
    Ok(resp)
}

fn response_content_type(resp: &Response) -> Option<String> {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .map(str::to_owned)
}

fn starts_with_angle_bracket_ignoring_ws(bytes: &[u8]) -> bool {
    let mut slice = bytes;
    while let Some((&b, rest)) = slice.split_first() {
        if b.is_ascii_whitespace() {
            slice = rest;
        } else {
            break;
        }
    }
    if let Some(rest) = slice.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        slice = rest;
        while let Some((&b, r)) = slice.split_first() {
            if b.is_ascii_whitespace() {
                slice = r;
            } else {
                break;
            }
        }
    }
    slice.first().copied() == Some(b'<')
}

fn is_html_response(content_type: Option<&str>, bytes: &[u8]) -> bool {
    let html_ct = content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html"));
    html_ct || starts_with_angle_bracket_ignoring_ws(bytes)
}

/// Validate that a downloaded Excel payload is neither an HTML error/login page nor truncated.
pub(crate) fn validate_xls_response_bytes(content_type: Option<&str>, bytes: &[u8]) -> Result<()> {
    if let Some(ct) = content_type
        && ct.to_ascii_lowercase().contains("text/html")
    {
        bail!("Downloaded response has HTML content-type ({ct}), expected Excel workbook");
    }
    if starts_with_angle_bracket_ignoring_ws(bytes) {
        bail!("Downloaded response body starts with '<' (HTML/XML), expected Excel workbook");
    }
    if bytes.len() <= 100 {
        error!(
            "Downloaded file too small: {} bytes, expected > 100",
            bytes.len()
        );
        bail!(
            "Downloaded file too small or invalid ({} bytes)",
            bytes.len()
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::xls_parser::test_helpers::build_minimal_xlsx;
    use std::io::Read;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct DummyRecognizer(String);
    impl CaptchaRecognizer for DummyRecognizer {
        fn recognize(&mut self, _: &[u8]) -> Result<String> {
            Ok(self.0.clone())
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cfmmc_client_test_{name}_{nanos}.xlsx"))
    }

    pub(crate) fn read_full_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut header_end = None;
        let mut content_length = 0usize;

        loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if header_end.is_none()
                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                for line in headers.lines() {
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if let Some(end) = header_end
                && buf.len() >= end + content_length
            {
                break;
            }
        }

        String::from_utf8_lossy(&buf).into_owned()
    }

    fn spawn_scripted_server(
        responses: Vec<(u16, &'static str, &'static str, Vec<u8>)>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs_clone = Arc::clone(&requests);

        let handle = thread::spawn(move || {
            for (status, reason, ct, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let req_str = read_full_http_request(&mut stream);
                let first_line = req_str.lines().next().unwrap_or("").to_string();
                reqs_clone.lock().unwrap().push(first_line);

                let header = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });

        (format!("http://{addr}"), requests, handle)
    }

    #[test]
    fn validate_xls_response_rejects_html_content_type_and_leading_angle_bracket() {
        let valid = build_minimal_xlsx("客户交易结算日报", &[]);
        assert!(
            validate_xls_response_bytes(
                Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
                &valid
            )
            .is_ok()
        );

        let err_ct =
            validate_xls_response_bytes(Some("text/html; charset=utf-8"), &valid).unwrap_err();
        assert!(err_ct.to_string().contains("HTML content-type"));

        let err_body = validate_xls_response_bytes(
            Some("application/octet-stream"),
            b"  \r\n\t<html><body>Login required</body></html>",
        )
        .unwrap_err();
        assert!(err_body.to_string().contains("starts with '<'"));

        let err_bom =
            validate_xls_response_bytes(None, b"\xEF\xBB\xBF  <!DOCTYPE html>").unwrap_err();
        assert!(err_bom.to_string().contains("starts with '<'"));

        let err_small =
            validate_xls_response_bytes(Some("application/octet-stream"), b"PK\x03\x04")
                .unwrap_err();
        assert!(err_small.to_string().contains("too small"));
    }

    #[test]
    fn collector_debug_redacts_password() {
        let mut rec = DummyRecognizer("ABC123".to_string());
        let collector = CFMMCCollector::new(
            "user1".to_string(),
            "my-secret-pw".to_string(),
            &mut rec,
            6,
            false,
        )
        .unwrap();
        let dbg = format!("{collector:?}");
        assert!(dbg.contains("user1"));
        assert!(!dbg.contains("my-secret-pw"));
        assert!(dbg.contains("[REDACTED]"));
    }

    #[test]
    fn download_xls_rejects_html_and_corrupt_zip_before_persisting() {
        let save_path = unique_temp_path("reject_corrupt");
        let fake_bytes = vec![b'X'; 512];
        let (base_url, _, handle) = spawn_scripted_server(vec![
            (
                200,
                "OK",
                "text/html; charset=utf-8",
                b"<html>login</html>".to_vec(),
            ),
            (200, "OK", "application/octet-stream", fake_bytes),
            (502, "Bad Gateway", "text/plain", b"bad gateway".to_vec()),
        ]);

        let mut rec = DummyRecognizer("ABC123".to_string());
        let mut collector =
            CFMMCCollector::new("user1".to_string(), "pw".to_string(), &mut rec, 6, false).unwrap();
        collector.set_base_url_for_test(base_url);

        assert!(collector.download_xls(&save_path).is_err());
        assert!(!save_path.exists());

        // Non-HTML >100 bytes that is NOT a valid Excel workbook must fail calamine validation
        let err = collector.download_xls(&save_path).unwrap_err();
        assert!(err.to_string().contains("valid Excel workbook"));
        assert!(!save_path.exists());

        // Non-success HTTP status fails
        let err_http = collector.download_xls(&save_path).unwrap_err();
        assert!(err_http.to_string().contains("502"));
        assert!(!save_path.exists());

        handle.join().unwrap();
    }

    #[test]
    fn download_xls_persists_valid_xlsx_atomically() {
        let save_path = unique_temp_path("valid_persist");
        let valid_xlsx = build_minimal_xlsx(
            "客户交易结算日报",
            &[vec![("A", "客户权益"), ("C", "8888.00")]],
        );
        let (base_url, _, handle) = spawn_scripted_server(vec![(
            200,
            "OK",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            valid_xlsx.clone(),
        )]);

        let mut rec = DummyRecognizer("ABC123".to_string());
        let mut collector =
            CFMMCCollector::new("user1".to_string(), "pw".to_string(), &mut rec, 6, false).unwrap();
        collector.set_base_url_for_test(base_url);

        collector.download_xls(&save_path).unwrap();
        assert!(save_path.is_file());
        assert_eq!(std::fs::read(&save_path).unwrap(), valid_xlsx);

        let _ = std::fs::remove_file(&save_path);
        handle.join().unwrap();
    }

    #[test]
    fn login_refreshes_token_after_failed_attempt_and_validates_http_statuses() {
        let (base_url, requests, handle) = spawn_scripted_server(vec![
            // Attempt 1: initial login page with tok1
            (
                200,
                "OK",
                "text/html",
                br#"<img src="/veriCode.do?t=tok1">"#.to_vec(),
            ),
            // GET /veriCode.do?t=tok1
            (200, "OK", "image/jpeg", b"img1".to_vec()),
            // POST /login.do returns captcha error
            (200, "OK", "text/html", "验证码错误".as_bytes().to_vec()),
            // Attempt 2: refreshed login page with tok2
            (
                200,
                "OK",
                "text/html",
                br#"<img src="/veriCode.do?t=tok2">"#.to_vec(),
            ),
            // GET /veriCode.do?t=tok2
            (200, "OK", "image/jpeg", b"img2".to_vec()),
            // POST /login.do succeeds
            (200, "OK", "text/html", "客户权益".as_bytes().to_vec()),
            // setParameter returning HTTP 500 should fail
            (500, "Internal Server Error", "text/plain", b"err".to_vec()),
        ]);

        let mut rec = DummyRecognizer("123456".to_string());
        let mut collector =
            CFMMCCollector::new("user1".to_string(), "pw".to_string(), &mut rec, 6, false).unwrap();
        collector.set_base_url_for_test(base_url);

        collector.login().unwrap();
        let reqs = requests.lock().unwrap().clone();
        assert!(reqs.iter().any(|r| r.contains("/veriCode.do?t=tok1")));
        assert!(reqs.iter().any(|r| r.contains("/veriCode.do?t=tok2")));

        let err = collector.set_parameter("2026-01-15").unwrap_err();
        assert!(err.to_string().contains("500"));

        handle.join().unwrap();
    }

    #[test]
    fn login_fails_on_http_error_and_recovers_when_vericode_returns_html() {
        let (base_url, _, handle) = spawn_scripted_server(vec![
            // First login(): GET /login.do returns 503
            (503, "Service Unavailable", "text/plain", b"down".to_vec()),
            // Second login(): GET /login.do returns tok1
            (
                200,
                "OK",
                "text/html",
                br#"<img src="/veriCode.do?t=tok1">"#.to_vec(),
            ),
            // GET /veriCode.do?t=tok1 returns HTML (expired token) -> should refresh login page
            (
                200,
                "OK",
                "text/html",
                b"<html>session expired</html>".to_vec(),
            ),
            // Refreshed GET /login.do returns tok2
            (
                200,
                "OK",
                "text/html",
                br#"<img src="/veriCode.do?t=tok2">"#.to_vec(),
            ),
            // GET /veriCode.do?t=tok2 returns image
            (200, "OK", "image/jpeg", b"img".to_vec()),
            // POST /login.do returns 500 -> fails cleanly
            (500, "Internal Server Error", "text/plain", b"err".to_vec()),
        ]);

        let mut rec = DummyRecognizer("123456".to_string());
        let mut collector =
            CFMMCCollector::new("user1".to_string(), "pw".to_string(), &mut rec, 6, false).unwrap();
        collector.set_base_url_for_test(base_url);

        let err1 = collector.login().unwrap_err();
        assert!(err1.to_string().contains("503"));

        let err2 = collector.login().unwrap_err();
        assert!(err2.to_string().contains("500"));

        handle.join().unwrap();
    }
}
