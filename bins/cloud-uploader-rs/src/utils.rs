use anyhow::{Context, Result};
use glob::glob;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Find files matching a glob pattern in `source_folder`.
///
/// Returns `Ok(vec![])` when zero files match so callers can report
/// `UploadSkippedReason::NoFilesFound`. Errors only if `source_folder` does not
/// exist / is not a directory or if the glob pattern syntax is invalid.
pub fn find_files_by_pattern(source_folder: &str, pattern: &str) -> Result<Vec<PathBuf>> {
    let source_path = Path::new(source_folder);
    if !source_path.exists() {
        anyhow::bail!("Folder not found: {}", source_folder);
    }
    if !source_path.is_dir() {
        anyhow::bail!("Source path is not a directory: {}", source_folder);
    }

    let full_pattern = if Path::new(pattern).is_absolute() {
        pattern.to_string()
    } else {
        source_path.join(pattern).to_string_lossy().into_owned()
    };

    debug!("Searching for files with pattern: {}", full_pattern);

    let mut files = Vec::new();
    for entry in glob(&full_pattern).context("Invalid glob pattern")? {
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

    files.sort();
    info!("Found {} file(s) matching pattern", files.len());
    Ok(files)
}

/// Find all regular files directly inside `folder` (non-recursive).
pub fn find_all_files(folder: &str) -> Result<Vec<PathBuf>> {
    let path = Path::new(folder);
    if !path.exists() {
        anyhow::bail!("Folder not found: {}", folder);
    }
    if !path.is_dir() {
        anyhow::bail!("Source path is not a directory: {}", folder);
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_file() {
            files.push(entry_path);
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cloud_uploader_utils_{}_{}_{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn find_files_by_pattern_returns_empty_vec_when_zero_matches() {
        let dir = temp_dir("empty");
        fs::write(dir.join("note.txt"), b"hello").unwrap();

        let matched = find_files_by_pattern(dir.to_str().unwrap(), "*.csv").unwrap();
        assert!(matched.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_files_by_pattern_errors_when_folder_missing_or_glob_invalid() {
        let missing = std::env::temp_dir().join("definitely_missing_cloud_uploader_dir_12345");
        assert!(find_files_by_pattern(missing.to_str().unwrap(), "*.csv").is_err());

        let dir = temp_dir("bad_glob");
        assert!(find_files_by_pattern(dir.to_str().unwrap(), "[unclosed").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_files_by_pattern_returns_sorted_matching_files() {
        let dir = temp_dir("match");
        fs::write(dir.join("b.csv"), b"1").unwrap();
        fs::write(dir.join("a.csv"), b"2").unwrap();
        fs::write(dir.join("c.txt"), b"3").unwrap();

        let matched = find_files_by_pattern(dir.to_str().unwrap(), "*.csv").unwrap();
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].file_name().unwrap(), "a.csv");
        assert_eq!(matched[1].file_name().unwrap(), "b.csv");

        let _ = fs::remove_dir_all(&dir);
    }
}
