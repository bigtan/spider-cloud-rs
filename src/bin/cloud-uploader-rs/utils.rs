use anyhow::{Context, Result};
use glob::glob;
use std::path::PathBuf;
use tracing::{debug, info};

/// Find files matching a glob pattern in a directory
pub fn find_files_by_pattern(source_folder: &str, pattern: &str) -> Result<Vec<PathBuf>> {
    // Combine source folder and pattern
    let full_pattern = if pattern.starts_with('/') || pattern.starts_with('\\') {
        pattern.to_string()
    } else {
        format!("{}/{}", source_folder.trim_end_matches('/'), pattern)
    };

    debug!("Searching for files with pattern: {}", full_pattern);

    let mut files = Vec::new();
    for entry in glob(&full_pattern).context("Failed to read glob pattern")? {
        match entry {
            Ok(path) => {
                if path.is_file() {
                    info!("Found file: {}", path.display());
                    files.push(path);
                }
            }
            Err(e) => {
                tracing::warn!("Error reading glob entry: {}", e);
            }
        }
    }

    if files.is_empty() {
        anyhow::bail!("No files found matching pattern: {}", full_pattern);
    }

    info!("Found {} file(s) matching pattern", files.len());
    Ok(files)
}

