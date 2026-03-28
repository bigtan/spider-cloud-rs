use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use tracing::info;

/// Create a 7z archive from a source folder
pub fn create_archive(
    source_folder: &str,
    output_archive: &str,
    password: Option<&str>,
) -> Result<bool> {
    // If archive already exists, skip creation
    if Path::new(output_archive).exists() {
        info!("Archive already exists: {}", output_archive);
        return Ok(true);
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
        .arg("-ms=off"); // Solid compression

    if let Some(pwd) = password {
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to create archive: {}", stderr);
    }
}
