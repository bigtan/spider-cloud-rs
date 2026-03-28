use anyhow::{Context, Result};
use image::RgbImage;
use ndarray::s;
use ndarray::{Array, Array4};
use std::collections::HashMap;
use std::fs;
use tracing::{debug, info, warn};

/// Resize image while maintaining aspect ratio, pad to (32, 804)
pub fn keepratio_resize(img: &RgbImage, _debug: bool) -> RgbImage {
    let mask_height = 32;
    let mask_width = 804;
    let cur_ratio = img.width() as f32 / img.height() as f32;

    debug!(
        "Original image dimensions: {}x{}, ratio: {:.3}",
        img.width(),
        img.height(),
        cur_ratio
    );

    let (cur_target_height, cur_target_width) =
        if cur_ratio > mask_width as f32 / mask_height as f32 {
            (mask_height, mask_width)
        } else {
            (mask_height, (mask_height as f32 * cur_ratio) as u32)
        };

    debug!(
        "Target dimensions: {}x{}",
        cur_target_width, cur_target_height
    );

    let resized = image::imageops::resize(
        img,
        cur_target_width,
        cur_target_height,
        image::imageops::FilterType::Lanczos3,
    );
    let mut mask = RgbImage::new(mask_width, mask_height);
    image::imageops::overlay(&mut mask, &resized, 0, 0);

    debug!("Image resized and padded to {}x{}", mask_width, mask_height);
    mask
}

/// Preprocess image for model inference, output shape: [3, 3, 32, 300]
pub fn preprocess_image(img: &RgbImage, debug: bool) -> Result<Array4<f32>> {
    debug!("Starting image preprocessing");

    let mask = keepratio_resize(img, debug);

    // Convert to ndarray
    let arr = Array::from_shape_fn(
        (mask.height() as usize, mask.width() as usize, 3),
        |(y, x, c)| mask.get_pixel(x as u32, y as u32)[c] as f32 / 255.0,
    );

    debug!("Converted to ndarray with shape: {:?}", arr.shape());

    // Chunking logic
    let chunk_width = 300;
    let chunk_overlap = 48;
    let chunk_stride = chunk_width - chunk_overlap;
    let num_chunks = 3;

    debug!(
        "Chunking parameters: width={}, overlap={}, stride={}, num_chunks={}",
        chunk_width, chunk_overlap, chunk_stride, num_chunks
    );

    let mut chunks = Vec::new();
    for i in 0..num_chunks {
        let start = i * chunk_stride;
        let end = (start + chunk_width).min(mask.width() as usize);
        let chunk = arr.slice(s![.., start..end, ..]).to_owned();
        debug!(
            "Chunk {}: range {}..{}, actual shape: {:?}",
            i,
            start,
            end,
            chunk.shape()
        );
        chunks.push(chunk);
    }

    // Stack and reshape
    let mut merged = Array4::<f32>::zeros((num_chunks, 32, chunk_width, 3));
    for (i, chunk) in chunks.iter().enumerate() {
        let width = chunk.shape()[1];
        merged.slice_mut(s![i, .., ..width, ..]).assign(chunk);
    }

    // Permute to (num_chunks, channels, height, width)
    let processed = merged.permuted_axes([0, 3, 1, 2]);

    debug!("Final processed data shape: {:?}", processed.shape());
    Ok(processed)
}

/// Decode predictions using CTC logic
pub fn decode_predictions(
    output: &ndarray::ArrayViewD<f32>,
    label_mapping: &HashMap<usize, String>,
    _debug: bool,
) -> Result<Vec<String>> {
    let shape = output.shape();
    let batch_size = shape[0];
    let length = shape[1];

    debug!("Decoding predictions with shape: {:?}", shape);

    let mut results = Vec::new();
    for i in 0..batch_size {
        let mut last_p = 0;
        let mut str_pred = Vec::new();

        for j in 0..length {
            let probs = output.slice(s![i, j, ..]);
            let p = probs
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .map(|(idx, _)| idx)
                .unwrap_or(0);

            if p != last_p && p != 0 {
                if let Some(label) = label_mapping.get(&p) {
                    str_pred.push(label.clone());
                } else {
                    warn!("Label index {} not found in mapping", p);
                }
            }
            last_p = p;
        }

        let final_str = str_pred.join("");
        results.push(final_str.clone());

        debug!(
            "Batch {} decoded: '{}' (length: {})",
            i,
            final_str,
            final_str.len()
        );
    }

    debug!("Decoded {} batches successfully", results.len());
    Ok(results)
}

pub fn load_label_mapping(vocab_path: &str) -> Result<HashMap<usize, String>> {
    info!("Loading label mapping from: {}", vocab_path);

    let content = fs::read_to_string(vocab_path)
        .with_context(|| format!("Failed to read vocab file: {vocab_path}"))?;

    let mut mapping = HashMap::new();
    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            mapping.insert(i + 2, trimmed.to_string());
        }
    }

    info!("Loaded {} labels from vocab file", mapping.len());
    debug!("Label mapping: {:?}", mapping);

    Ok(mapping)
}
