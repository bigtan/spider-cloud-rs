use super::utils::{decode_predictions, load_label_mapping, preprocess_image};
use anyhow::{Context, Result};
use ort::{session::Session, value::Tensor};
use std::collections::HashMap;
use tracing::{debug, info, warn};

pub trait CaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String>;
}

pub struct DamoCaptchaRecognizer {
    model: Session,
    label_mapping: HashMap<usize, String>,
    debug: bool,
}

impl DamoCaptchaRecognizer {
    pub fn new(model_path: &str, vocab_path: &str, debug: bool) -> Result<Self> {
        info!(
            "Initializing DamoCaptchaRecognizer with model: {}, vocab: {}",
            model_path, vocab_path
        );

        let model = Session::builder()
            .with_context(|| "Failed to create session builder".to_string())?
            .commit_from_file(model_path)
            .with_context(|| format!("Failed to load model from: {model_path}"))?;

        let label_mapping = load_label_mapping(vocab_path)
            .with_context(|| format!("Failed to load label mapping from: {vocab_path}"))?;

        info!(
            "DamoCaptchaRecognizer initialized successfully with {} labels",
            label_mapping.len()
        );

        Ok(DamoCaptchaRecognizer {
            model,
            label_mapping,
            debug,
        })
    }
}

impl CaptchaRecognizer for DamoCaptchaRecognizer {
    fn recognize(&mut self, image_bytes: &[u8]) -> Result<String> {
        debug!(
            "Starting CAPTCHA recognition, image size: {} bytes",
            image_bytes.len()
        );

        // Decode image
        let img = image::load_from_memory(image_bytes)
            .context("Failed to decode image bytes")?
            .to_rgb8();

        debug!(
            "Image decoded successfully, dimensions: {}x{}",
            img.width(),
            img.height()
        );

        if self.debug {
            if let Err(e) = img.save("debug_captcha.jpg") {
                warn!("Failed to save debug image: {}", e);
            } else {
                debug!("Debug image saved to debug_captcha.jpg");
            }
        }

        // Preprocess image
        let input_data = preprocess_image(&img, self.debug)?;
        debug!("Image preprocessed successfully");

        // Model inference
        // let input_name = self.model.inputs[0].name.clone();
        // let output_name = self.model.outputs[0].name.clone();
        let input_name = self.model.inputs()[0].name().to_string();
        let output_name = self.model.outputs()[0].name().to_string();

        debug!(
            "Running model inference with input: {}, output: {}",
            input_name, output_name
        );

        let outputs = self
            .model
            .run(ort::inputs![
                input_name.as_str() => Tensor::from_array(input_data)?
            ])
            .context("Model inference failed")?;

        let output = outputs[output_name.as_str()]
            .try_extract_array::<f32>()
            .context("Failed to extract output array")?;

        debug!(
            "Model inference completed, output shape: {:?}",
            output.shape()
        );

        let preds = decode_predictions(&output, &self.label_mapping, self.debug)?;
        let result = preds.join("");

        if result.len() == 6 {
            info!("CAPTCHA recognition successful: {}", result);
        } else {
            warn!(
                "CAPTCHA recognition returned unexpected length: {} (expected 6)",
                result.len()
            );
        }

        Ok(result)
    }
}
