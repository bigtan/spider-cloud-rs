use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::{RgbImage, imageops::FilterType};
use ndarray::Array4;
use ort::{session::Session, value::TensorRef};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use url::Url;

const TOKEN_URL: &str = "https://aip.baidubce.com/oauth/2.0/token";
const DEFAULT_OCR_URL: &str = "https://aip.baidubce.com/rest/2.0/ocr/v1/accurate_basic";
const TOKEN_EXPIRY_SKEW: Duration = Duration::from_secs(300);
const MASK_HEIGHT: usize = 32;
const MASK_WIDTH: usize = 804;
const CHUNK_WIDTH: usize = 300;
const CHUNK_STRIDE: usize = 252;
const CHUNK_COUNT: usize = 3;
const CHANNELS: usize = 3;

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
pub struct OnnxCaptchaOptions {
    pub model_path: PathBuf,
    pub vocab_path: PathBuf,
    pub captcha_length: usize,
    pub debug: bool,
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

impl OnnxCaptchaRecognizer {
    pub fn new(options: OnnxCaptchaOptions) -> Result<Self> {
        if options.captcha_length == 0 {
            bail!("ONNX CAPTCHA length must be greater than zero");
        }

        let session = Session::builder()
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
            if let Err(err) = std::fs::write("debug_captcha_onnx.jpg", image_bytes) {
                warn!("Failed to save debug ONNX CAPTCHA image: {}", err);
            } else {
                debug!("Debug ONNX CAPTCHA image saved to debug_captcha_onnx.jpg");
            }
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

        if result.len() == self.options.captcha_length {
            info!("ONNX CAPTCHA recognition successful: {}", result);
        } else {
            warn!(
                "ONNX CAPTCHA recognition returned unexpected length: {} (expected {}): {}",
                result.len(),
                self.options.captcha_length,
                result
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
            Ok(result) if result.len() == self.expected_len => Ok(result),
            Ok(result) => {
                warn!(
                    "Primary CAPTCHA recognizer returned invalid length {}: {}; trying fallback",
                    result.len(),
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

fn preprocess_image(image_bytes: &[u8]) -> Result<Array4<f32>> {
    let img = image::load_from_memory(image_bytes)
        .context("Failed to decode CAPTCHA image")?
        .to_rgb8();
    let (resized, content_width) = keep_ratio_resize(&img);

    // 只为覆盖实际内容的滑动窗口构建 batch：CFMMC 验证码缩放后远窄于
    // 一个窗口，固定 3 个窗口会让 2/3 的推理算空白填充
    let chunk_count = if content_width <= CHUNK_WIDTH {
        1
    } else {
        (1 + (content_width - CHUNK_WIDTH).div_ceil(CHUNK_STRIDE)).min(CHUNK_COUNT)
    };

    let mut data = Array4::<f32>::zeros((chunk_count, CHANNELS, MASK_HEIGHT, CHUNK_WIDTH));
    for chunk in 0..chunk_count {
        let left = CHUNK_STRIDE * chunk;
        for y in 0..MASK_HEIGHT {
            for x in 0..CHUNK_WIDTH {
                let pixel = resized.get_pixel((left + x) as u32, y as u32).0;
                data[[chunk, 0, y, x]] = f32::from(pixel[2]) / 255.0;
                data[[chunk, 1, y, x]] = f32::from(pixel[1]) / 255.0;
                data[[chunk, 2, y, x]] = f32::from(pixel[0]) / 255.0;
            }
        }
    }

    Ok(data)
}

/// Resize keeping aspect ratio onto a fixed-size canvas; also returns the
/// width actually covered by image content (the rest is black padding).
fn keep_ratio_resize(img: &RgbImage) -> (RgbImage, usize) {
    let (width, height) = img.dimensions();
    let cur_ratio = width as f32 / height as f32;
    let max_ratio = MASK_WIDTH as f32 / MASK_HEIGHT as f32;
    let target_width = if cur_ratio > max_ratio {
        MASK_WIDTH
    } else {
        (MASK_HEIGHT as f32 * cur_ratio) as usize
    }
    .max(1);

    let resized = image::imageops::resize(
        img,
        target_width as u32,
        MASK_HEIGHT as u32,
        FilterType::Triangle,
    );

    let mut canvas = RgbImage::new(MASK_WIDTH as u32, MASK_HEIGHT as u32);
    for y in 0..MASK_HEIGHT as u32 {
        for x in 0..target_width as u32 {
            canvas.put_pixel(x, y, *resized.get_pixel(x, y));
        }
    }

    (canvas, target_width)
}

fn load_vocab(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read ONNX CAPTCHA vocab from {}", path.display()))?;
    let mut vocab = vec![String::new(), String::new()];
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
                    if decoded.len() < captcha_length {
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
    use super::ctc_decode_captcha;

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
}
