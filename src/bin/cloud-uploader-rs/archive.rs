use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use tracing::info;

/// Create a 7z archive from a source folder
pub fn create_archive(
    source_folder: &str,
    output_archive: &str,
    password: Option<&str>,
) -> Result<bool> {
    let output_path = Path::new(output_archive);

    // If a non-empty archive already exists, skip creation
    if let Ok(meta) = fs::metadata(output_path) {
        if meta.len() > 0 {
            info!("Archive already exists: {}", output_archive);
            return Ok(true);
        }
        let _ = fs::remove_file(output_path);
    }

    let source_path = Path::new(source_folder);
    if !source_path.exists() {
        anyhow::bail!("Archive source folder not found: {}", source_folder);
    }

    if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create archive output directory: {}",
                parent.display()
            )
        })?;
    }

    info!(
        "Creating archive: {} from {}",
        output_archive, source_folder
    );

    // Build 7z command
    let mut command = Command::new("7z");
    command
        .arg("a")
        .arg("-mx=9") // Maximum compression
        .arg("-m0=lzma2") // LZMA2 algorithm
        .arg("-md=256m") // 256MB dictionary
        .arg("-ms=off"); // Disable solid compression

    if let Some(pwd) = password.map(str::trim).filter(|p| !p.is_empty()) {
        command.arg(format!("-p{}", pwd));
    }

    command.arg(output_archive).arg(source_folder);

    // Execute command
    let output = command
        .output()
        .context("Failed to execute 7z command. Make sure 7-Zip is installed and in PATH")?;

    if output.status.success() {
        info!("Archive created successfully");
        Ok(true)
    } else {
        let _ = fs::remove_file(output_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to create archive: {}", stderr);
    }
}
