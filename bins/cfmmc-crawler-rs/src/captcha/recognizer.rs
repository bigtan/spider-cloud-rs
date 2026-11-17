use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::imageops::FilterType;
use ndarray::Array4;
use ort::{session::Session, value::TensorRef};
use reqwest::blocking::Client;
use serde::Deserialize;
use spider_common::fs::write_private_atomic;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use url::Url;
use zeroize::Zeroize;

use crate::env_config::redact_secret;

const DEFAULT_TOKEN_URL: &str = "https://aip.baidubce.com/oauth/2.0/token";
const DEFAULT_OCR_URL: &str = "https://aip.baidubce.com/rest/2.0/ocr/v1/accurate_basic";
const TOKEN_EXPIRY_SKEW: Duration = Duration::from_secs(300);
// PP-OCRv6 rec preprocessing spec (from inference.yml: RecResizeImg image_shape [3, 48, 320]).
const REC_HEIGHT: usize = 48;
const REC_MAX_WIDTH: usize = 320;
const CHANNELS: usize = 3;

pub trait CaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String>;
}

#[derive(Clone)]
pub struct BaiduOcrOptions {
    pub api_key: String,
    pub secret_key: String,
    pub ocr_url: String,
    pub captcha_length: usize,
    pub debug: bool,
    pub log_dir: PathBuf,
}

impl fmt::Debug for BaiduOcrOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BaiduOcrOptions")
            .field("api_key", &redact_secret(&self.api_key))
            .field("secret_key", &redact_secret(&self.secret_key))
            .field("ocr_url", &self.ocr_url)
            .field("captcha_length", &self.captcha_length)
            .field("debug", &self.debug)
            .field("log_dir", &self.log_dir)
            .finish()
    }
}

impl Drop for BaiduOcrOptions {
    fn drop(&mut self) {
        self.api_key.zeroize();
        self.secret_key.zeroize();
    }
}

impl Default for BaiduOcrOptions {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            ocr_url: DEFAULT_OCR_URL.to_string(),
            captcha_length: 6,
            debug: false,
            log_dir: PathBuf::from("logs"),
        }
    }
}

pub struct BaiduOcrCaptchaRecognizer {
    client: Client,
    options: BaiduOcrOptions,
    token_url: String,
    token: Option<CachedToken>,
}

impl fmt::Debug for BaiduOcrCaptchaRecognizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BaiduOcrCaptchaRecognizer")
            .field("options", &self.options)
            .field("token_url", &self.token_url)
            .field("has_token", &self.token.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct OnnxCaptchaOptions {
    pub model_path: PathBuf,
    pub vocab_path: PathBuf,
    pub captcha_length: usize,
    pub debug: bool,
    pub log_dir: PathBuf,
}

pub struct OnnxCaptchaRecognizer {
    session: Session,
    vocab: Vec<String>,
    options: OnnxCaptchaOptions,
}

pub struct FallbackCaptchaRecognizer {
    primary: Box<dyn CaptchaRecognizer>,
    fallback: Box<dyn CaptchaRecognizer>,
    expected_len: usize,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

impl fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedToken")
            .field("value", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Drop for CachedToken {
    fn drop(&mut self) {
        self.value.zeroize();
    }
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
        if options.captcha_length == 0 {
            bail!("Baidu OCR CAPTCHA length must be greater than zero");
        }
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
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .context("Failed to build Baidu OCR HTTP client")?;

        info!("Baidu OCR CAPTCHA recognizer initialized");
        Ok(Self {
            client,
            options,
            token_url: DEFAULT_TOKEN_URL.to_string(),
            token: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_token_url(&mut self, url: impl Into<String>) {
        self.token_url = url.into();
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
        let token_url = Url::parse(&self.token_url).context("Invalid Baidu OCR token URL")?;

        let resp = self
            .client
            .post(token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.options.api_key.as_str()),
                ("client_secret", self.options.secret_key.as_str()),
            ])
            .send()
            .map_err(|err| {
                anyhow!(
                    "Failed to request Baidu OCR access token: {}",
                    err.without_url()
                )
            })?;

        let status = resp.status();
        if !status.is_success() {
            bail!("Baidu OCR token endpoint returned HTTP status {status}");
        }

        let response = resp
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

        let resp = self
            .client
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
            .map_err(|err| {
                anyhow!(
                    "Failed to request Baidu OCR recognition: {}",
                    err.without_url()
                )
            })?;

        let status = resp.status();
        if !status.is_success() {
            bail!("Baidu OCR endpoint returned HTTP status {status}");
        }

        resp.json::<OcrResponse>()
            .context("Failed to parse Baidu OCR recognition response")
    }
}

impl OnnxCaptchaRecognizer {
    pub fn new(options: OnnxCaptchaOptions) -> Result<Self> {
        if options.captcha_length == 0 {
            bail!("ONNX CAPTCHA length must be greater than zero");
        }

        let env = ort::init()
            .build()
            .context("Failed to initialize ONNX Runtime environment")?;
        let session = Session::builder(&env)
            .context("Failed to create ONNX Runtime session builder")?
            .commit_from_file(&options.model_path)
            .with_context(|| {
                format!(
                    "Failed to load ONNX CAPTCHA model from {}",
                    options.model_path.display()
                )
            })?;
        let vocab = load_vocab(&options.vocab_path)?;

        info!(
            "ONNX CAPTCHA recognizer initialized with model: {}",
            options.model_path.display()
        );
        Ok(Self {
            session,
            vocab,
            options,
        })
    }
}

impl CaptchaRecognizer for OnnxCaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String> {
        debug!(
            "Starting CAPTCHA recognition through local ONNX model, image size: {} bytes",
            image_bytes.len()
        );

        if self.options.debug {
            save_debug_captcha(&self.options.log_dir, "debug_captcha_onnx.jpg", image_bytes);
        }

        let input = preprocess_image(image_bytes)?;
        let input_tensor = TensorRef::from_array_view(&input)
            .context("Failed to create ONNX tensor for CAPTCHA image")?;
        let outputs = self
            .session
            .run(ort::inputs![input_tensor])
            .context("Failed to run ONNX CAPTCHA inference")?;
        let (shape, probabilities) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("Failed to extract ONNX CAPTCHA output tensor")?;
        let dims = shape
            .iter()
            .map(|&dim| {
                usize::try_from(dim)
                    .map_err(|_| anyhow!("invalid negative output dimension: {dim}"))
            })
            .collect::<Result<Vec<_>>>()?;

        if dims.len() != 3 {
            bail!("expected ONNX output rank 3 [batch, length, classes], got {dims:?}");
        }

        let preds = argmax_predictions(&dims, probabilities)?;
        let result = ctc_decode_captcha(&preds, &self.vocab, self.options.captcha_length);

        let char_count = result.chars().count();
        if char_count == self.options.captcha_length {
            info!("ONNX CAPTCHA recognition successful: {}", result);
        } else {
            warn!(
                "ONNX CAPTCHA recognition returned unexpected length: {} (expected {}): {}",
                char_count, self.options.captcha_length, result
            );
        }

        Ok(result)
    }
}

impl FallbackCaptchaRecognizer {
    pub fn new(
        primary: Box<dyn CaptchaRecognizer>,
        fallback: Box<dyn CaptchaRecognizer>,
        expected_len: usize,
    ) -> Self {
        Self {
            primary,
            fallback,
            expected_len,
        }
    }
}

impl CaptchaRecognizer for FallbackCaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String> {
        match self.primary.recognize(image_bytes) {
            Ok(result) if result.chars().count() == self.expected_len => Ok(result),
            Ok(result) => {
                warn!(
                    "Primary CAPTCHA recognizer returned invalid length {}: {}; trying fallback",
                    result.chars().count(),
                    result
                );
                self.fallback.recognize(image_bytes)
            }
            Err(err) => {
                warn!("Primary CAPTCHA recognizer failed: {err}; trying fallback");
                self.fallback.recognize(image_bytes)
            }
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
            save_debug_captcha(
                &self.options.log_dir,
                "debug_captcha_baidu.jpg",
                image_bytes,
            );
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

        let result = filter_baidu_ocr_words(
            response
                .words_result
                .unwrap_or_default()
                .into_iter()
                .map(|word| word.words),
        );

        let char_count = result.chars().count();
        if char_count == self.options.captcha_length {
            info!("CAPTCHA recognition successful: {}", result);
        } else {
            warn!(
                "CAPTCHA recognition returned unexpected length: {} (expected {}): {}",
                char_count, self.options.captcha_length, result
            );
        }

        Ok(result)
    }
}

pub(crate) fn filter_baidu_ocr_words<I>(words: I) -> String
where
    I: IntoIterator<Item = String>,
{
    words
        .into_iter()
        .flat_map(|word| word.chars().collect::<Vec<_>>())
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

fn save_debug_captcha(log_dir: &Path, file_name: &str, image_bytes: &[u8]) {
    let path = log_dir.join(file_name);
    if let Err(err) = write_private_atomic(&path, image_bytes) {
        warn!(
            "Failed to save debug CAPTCHA image to {}: {}",
            path.display(),
            err
        );
    } else {
        debug!("Debug CAPTCHA image saved to {}", path.display());
    }
}

fn preprocess_image(image_bytes: &[u8]) -> Result<Array4<f32>> {
    let img = image::load_from_memory(image_bytes)
        .context("Failed to decode CAPTCHA image")?
        .to_rgb8();

    // PP-OCRv6 RecResizeImg: keep aspect ratio, resize to height 48, cap width at
    // 320, then right-pad. Normalization is (pixel/255 - 0.5) / 0.5 -> [-1, 1].
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        bail!("CAPTCHA image has zero dimensions");
    }
    let ratio = width as f32 / height as f32;
    let resized_w = ((REC_HEIGHT as f32 * ratio).ceil() as usize).clamp(1, REC_MAX_WIDTH);
    let resized = image::imageops::resize(
        &img,
        resized_w as u32,
        REC_HEIGHT as u32,
        FilterType::Triangle,
    );

    // The padded region stays 0.0: PaddleOCR pads the already-normalized array with
    // zeros (which is distinct from a normalized black pixel at -1.0).
    let mut data = Array4::<f32>::zeros((1, CHANNELS, REC_HEIGHT, REC_MAX_WIDTH));
    for y in 0..REC_HEIGHT {
        for x in 0..resized_w {
            // image crate yields RGB; the model expects BGR channel order.
            let pixel = resized.get_pixel(x as u32, y as u32).0;
            data[[0, 0, y, x]] = normalize(pixel[2]);
            data[[0, 1, y, x]] = normalize(pixel[1]);
            data[[0, 2, y, x]] = normalize(pixel[0]);
        }
    }

    Ok(data)
}

fn normalize(value: u8) -> f32 {
    f32::from(value) / 127.5 - 1.0
}

fn load_vocab(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read ONNX CAPTCHA vocab from {}", path.display()))?;
    // CTCLabelDecode prepends a single blank token at index 0; the character dict
    // then occupies indices 1..=N.
    let mut vocab = vec![String::new()];
    vocab.extend(content.lines().map(str::to_owned));
    Ok(vocab)
}

fn argmax_predictions(dims: &[usize], probabilities: &[f32]) -> Result<Vec<Vec<usize>>> {
    let batch = dims[0];
    let length = dims[1];
    let classes = dims[2];
    if probabilities.len() != batch * length * classes {
        bail!(
            "ONNX output shape {:?} does not match tensor length {}",
            dims,
            probabilities.len()
        );
    }

    let mut preds = vec![vec![0; length]; batch];
    for (b, row) in preds.iter_mut().enumerate() {
        for (t, slot) in row.iter_mut().enumerate() {
            let offset = (b * length + t) * classes;
            let mut best_idx = 0;
            let mut best_score = f32::NEG_INFINITY;
            for cls in 0..classes {
                let score = probabilities[offset + cls];
                if score > best_score {
                    best_score = score;
                    best_idx = cls;
                }
            }
            *slot = best_idx;
        }
    }

    Ok(preds)
}

fn ctc_decode_captcha(preds: &[Vec<usize>], vocab: &[String], captcha_length: usize) -> String {
    let mut decoded = String::new();
    // 依次解码每个滑动窗口的预测，而不是只取第一个窗口
    for row in preds {
        let mut last = 0;
        for &idx in row {
            if idx != last
                && idx != 0
                && let Some(token) = vocab.get(idx)
            {
                for ch in token.chars().filter(char::is_ascii_alphanumeric) {
                    if decoded.chars().count() < captcha_length {
                        decoded.push(ch);
                    }
                }
            }
            last = idx;
        }
    }

    decoded
}

fn is_token_error(error_code: u64) -> bool {
    matches!(error_code, 110 | 111)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cfmmc_recognizer_test_{name}_{nanos}"))
    }

    fn spawn_http_sequence(
        responses: Vec<(u16, &'static str, String)>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for (status, reason, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = crate::cfmmc::tests::read_full_http_request(&mut stream);
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
                stream.flush().unwrap();
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn ctc_decode_collapses_repeats_and_spans_rows() {
        let vocab: Vec<String> = vec!["".into(), "".into(), "a".into(), "b".into()];
        // blank=0；重复索引折叠；第二个窗口的预测也应被解码
        let preds = vec![vec![0, 2, 2, 0, 3], vec![2, 0, 2]];
        assert_eq!(ctc_decode_captcha(&preds, &vocab, 6), "abaa");
    }

    #[test]
    fn ctc_decode_caps_at_captcha_length() {
        let vocab: Vec<String> = vec!["".into(), "".into(), "a".into()];
        let preds = vec![vec![2, 0, 2, 0, 2, 0, 2]];
        assert_eq!(ctc_decode_captcha(&preds, &vocab, 3), "aaa");
    }

    #[test]
    fn baidu_filter_strips_non_ascii_and_punctuation() {
        let words = vec![
            " A1中 ".to_string(),
            "文-b2！".to_string(),
            "C_3。".to_string(),
        ];
        let filtered = filter_baidu_ocr_words(words);
        assert_eq!(filtered, "A1b2C3");
        assert_eq!(filtered.chars().count(), 6);
    }

    #[test]
    fn baidu_options_debug_redacts_credentials() {
        let mut opts = BaiduOcrOptions::default();
        opts.api_key = "my-api-key".to_string();
        opts.secret_key = "my-secret-key".to_string();
        let rendered = format!("{opts:?}");
        assert!(!rendered.contains("my-api-key"));
        assert!(!rendered.contains("my-secret-key"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn baidu_recognizer_filters_non_ascii_and_writes_debug_image_in_log_dir() {
        let log_dir = unique_temp_dir("baidu_debug");
        let (base_url, handle) = spawn_http_sequence(vec![
            (
                200,
                "OK",
                r#"{"access_token":"tok-1","expires_in":3600}"#.to_string(),
            ),
            (
                200,
                "OK",
                r#"{"words_result":[{"words":"a1中文"},{"words":" B2-c3!"}]}"#.to_string(),
            ),
        ]);

        let mut recognizer = BaiduOcrCaptchaRecognizer::new(BaiduOcrOptions {
            api_key: "key".to_string(),
            secret_key: "sec".to_string(),
            ocr_url: format!("{base_url}/ocr"),
            captcha_length: 6,
            debug: true,
            log_dir: log_dir.clone(),
        })
        .unwrap();
        recognizer.set_token_url(format!("{base_url}/token"));

        let code = recognizer.recognize(b"fake-jpg-bytes").unwrap();
        handle.join().unwrap();

        assert_eq!(code, "a1B2c3");
        let debug_img = log_dir.join("debug_captcha_baidu.jpg");
        assert!(debug_img.is_file());
        assert_eq!(std::fs::read(&debug_img).unwrap(), b"fake-jpg-bytes");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn baidu_recognizer_refreshes_expired_token_and_fails_on_http_error() {
        let (base_url, handle) = spawn_http_sequence(vec![
            (
                200,
                "OK",
                r#"{"access_token":"old-tok","expires_in":3600}"#.to_string(),
            ),
            (
                200,
                "OK",
                r#"{"error_code":110,"error_msg":"Access token invalid"}"#.to_string(),
            ),
            (
                200,
                "OK",
                r#"{"access_token":"new-tok","expires_in":3600}"#.to_string(),
            ),
            (
                200,
                "OK",
                r#"{"words_result":[{"words":"X9Y8Z7"}]}"#.to_string(),
            ),
            (500, "Internal Server Error", "boom".to_string()),
        ]);

        let mut recognizer = BaiduOcrCaptchaRecognizer::new(BaiduOcrOptions {
            api_key: "key".to_string(),
            secret_key: "sec".to_string(),
            ocr_url: format!("{base_url}/ocr"),
            captcha_length: 6,
            debug: false,
            log_dir: unique_temp_dir("unused"),
        })
        .unwrap();
        recognizer.set_token_url(format!("{base_url}/token"));

        let code = recognizer.recognize(b"img1").unwrap();
        assert_eq!(code, "X9Y8Z7");

        // Second call uses cached token, receives HTTP 500 from OCR endpoint
        let err = recognizer.recognize(b"img2").unwrap_err();
        assert!(err.to_string().contains("500"));
        handle.join().unwrap();
    }

    #[test]
    fn fallback_recognizer_checks_character_count() {
        struct Fixed(&'static str);
        impl CaptchaRecognizer for Fixed {
            fn recognize(&mut self, _: &[u8]) -> Result<String> {
                Ok(self.0.to_string())
            }
        }

        // 4 bytes ("a中") != 4 chars ("a中" has 2 chars!)
        let mut fb =
            FallbackCaptchaRecognizer::new(Box::new(Fixed("a中")), Box::new(Fixed("ABCD")), 4);
        assert_eq!(fb.recognize(b"img").unwrap(), "ABCD");
    }

    #[test]
    fn save_debug_captcha_writes_to_log_dir() {
        let log_dir = unique_temp_dir("onnx_debug");
        save_debug_captcha(&log_dir, "debug_captcha_onnx.jpg", b"onnx-img");
        let saved = log_dir.join("debug_captcha_onnx.jpg");
        assert!(saved.is_file());
        assert_eq!(std::fs::read(&saved).unwrap(), b"onnx-img");
        let _ = std::fs::remove_dir_all(&log_dir);
    }
}
