use crate::Result;
use crate::fs::AtomicTempFile;
use anyhow::Context;
use std::fs::{self, File};
use std::path::Path;
use tracing::{info, warn};

/// Supported archive output formats inferred from output path extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarZst,
}

impl ArchiveFormat {
    /// Detect archive format from the output filename (`.tar.zst` or `.tzst`).
    pub fn from_path(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
            Ok(Self::TarZst)
        } else {
            anyhow::bail!(
                "Unsupported archive format for '{}': expected .tar.zst or .tzst",
                path.display()
            )
        }
    }
}

/// Configuration for [`create_archive`].
#[derive(Debug, Clone, Default)]
pub struct ArchiveConfig {
    pub level: Option<i32>,
    pub threads: Option<u32>,
    pub reuse_existing: bool,
}

/// Create an archive at `output` from `source` according to `cfg` and the detected [`ArchiveFormat`].
pub fn create_archive(source: &Path, output: &Path, cfg: &ArchiveConfig) -> Result<()> {
    let format = ArchiveFormat::from_path(output)?;
    if !source.exists() {
        anyhow::bail!("Archive source not found: {}", source.display());
    }
    if !source.is_dir() && !source.is_file() {
        anyhow::bail!(
            "Archive source is not a file or directory: {}",
            source.display()
        );
    }

    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create archive output directory: {}",
                parent.display()
            )
        })?;
    }

    match format {
        ArchiveFormat::TarZst => create_tar_zst(source, output, cfg),
    }
}

fn create_tar_zst(source: &Path, output: &Path, cfg: &ArchiveConfig) -> Result<()> {
    if cfg.reuse_existing
        && let Ok(meta) = fs::metadata(output)
    {
        if meta.len() > 0 && verify_tar_zst(output).is_ok() {
            info!("Reusing verified archive: {}", output.display());
            return Ok(());
        }
        warn!(
            "Existing .tar.zst archive is empty or corrupted, recreating: {}",
            output.display()
        );
        let _ = fs::remove_file(output);
    }

    let level = cfg.level.unwrap_or(6);
    let default_threads = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .clamp(1, 8);
    let threads = cfg.threads.unwrap_or(default_threads).max(1);

    info!(
        "Creating tar.zst archive {} from {} (level={}, threads={})",
        output.display(),
        source.display(),
        level,
        threads
    );

    let mut temp = AtomicTempFile::new(output).with_context(|| {
        format!(
            "Failed to create staging file for archive: {}",
            output.display()
        )
    })?;

    {
        let mut encoder = zstd::Encoder::new(temp.file_mut(), level)
            .context("Failed to initialize zstd encoder")?;
        encoder
            .multithread(threads)
            .context("Failed to configure zstd multithreading")?;
        let mut builder = tar::Builder::new(encoder);

        let base_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("backup");

        if source.is_dir() {
            builder
                .append_dir_all(base_name, source)
                .with_context(|| format!("Failed to append directory: {}", source.display()))?;
        } else {
            builder
                .append_path_with_name(source, base_name)
                .with_context(|| format!("Failed to append file: {}", source.display()))?;
        }

        builder.finish().context("Failed to finish tar archive")?;
        let encoder = builder
            .into_inner()
            .context("Failed to finalize tar builder")?;
        encoder.finish().context("Failed to finish zstd encoding")?;
    }

    temp.persist()
        .with_context(|| format!("Failed to persist archive: {}", output.display()))?;
    Ok(())
}

fn verify_tar_zst(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let decoder = zstd::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        entry?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn archive_format_detection_handles_extensions() {
        assert_eq!(
            ArchiveFormat::from_path(Path::new("out/20260611.tar.zst")).unwrap(),
            ArchiveFormat::TarZst
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("OUT/DATA.TAR.ZST")).unwrap(),
            ArchiveFormat::TarZst
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("out/data.tzst")).unwrap(),
            ArchiveFormat::TarZst
        );
        assert!(ArchiveFormat::from_path(Path::new("out/20260611.7z")).is_err());
        assert!(ArchiveFormat::from_path(Path::new("out/20260611.zip")).is_err());
    }

    #[test]
    fn tar_zst_roundtrip_dir_and_file() {
        let base =
            std::env::temp_dir().join(format!("spider_common_tarzst_{}", Uuid::new_v4().simple()));
        let src_dir = base.join("sample_dir");
        fs::create_dir_all(src_dir.join("sub")).unwrap();
        fs::write(src_dir.join("a.txt"), b"hello-a").unwrap();
        fs::write(src_dir.join("sub").join("b.txt"), b"hello-b").unwrap();

        let archive_path = base.join("out.tar.zst");
        let cfg = ArchiveConfig::default();
        create_archive(&src_dir, &archive_path, &cfg).unwrap();
        assert!(archive_path.exists());
        assert!(verify_tar_zst(&archive_path).is_ok());

        // Verify exact extracted contents
        let extract_dir = base.join("extracted");
        let file = File::open(&archive_path).unwrap();
        let decoder = zstd::Decoder::new(file).unwrap();
        let mut tar_archive = tar::Archive::new(decoder);
        tar_archive.unpack(&extract_dir).unwrap();

        assert_eq!(
            fs::read(extract_dir.join("sample_dir").join("a.txt")).unwrap(),
            b"hello-a"
        );
        assert_eq!(
            fs::read(extract_dir.join("sample_dir").join("sub").join("b.txt")).unwrap(),
            b"hello-b"
        );

        // Also test reuse_existing recovers from a corrupt file
        fs::write(&archive_path, b"corrupt-bytes").unwrap();
        let reuse_cfg = ArchiveConfig {
            reuse_existing: true,
            ..Default::default()
        };
        create_archive(&src_dir, &archive_path, &reuse_cfg).unwrap();
        assert!(verify_tar_zst(&archive_path).is_ok());

        // Also test single-file archive
        let single_out = base.join("single.tar.zst");
        create_archive(&src_dir.join("a.txt"), &single_out, &cfg).unwrap();
        assert!(verify_tar_zst(&single_out).is_ok());

        let _ = fs::remove_dir_all(&base);
    }
}
