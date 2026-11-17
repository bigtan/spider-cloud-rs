use crate::Result;
use crate::fs::AtomicTempFile;
use anyhow::Context;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};
use tracing::{info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Supported archive output formats inferred from output path extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarZst,
    SevenZip,
}

impl ArchiveFormat {
    /// Detect archive format from the output filename (`.tar.zst`, `.tzst`, or `.7z`).
    pub fn from_path(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
            Ok(Self::TarZst)
        } else if name.ends_with(".7z") {
            Ok(Self::SevenZip)
        } else {
            anyhow::bail!(
                "Unsupported archive format for '{}': expected .tar.zst or .7z",
                path.display()
            )
        }
    }
}

/// Configuration for [`create_archive`].
#[derive(Clone, Default)]
pub struct ArchiveConfig {
    pub level: Option<i32>,
    pub dict_size: Option<String>,
    pub password: Option<Zeroizing<String>>,
    pub threads: Option<u32>,
    pub reuse_existing: bool,
}

impl fmt::Debug for ArchiveConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchiveConfig")
            .field("level", &self.level)
            .field("dict_size", &self.dict_size)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("threads", &self.threads)
            .field("reuse_existing", &self.reuse_existing)
            .finish()
    }
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
        ArchiveFormat::SevenZip => create_seven_zip(source, output, cfg),
    }
}

fn non_empty_password(cfg: &ArchiveConfig) -> Option<&str> {
    cfg.password
        .as_deref()
        .map(String::as_str)
        .filter(|p| !p.trim().is_empty())
}

fn create_tar_zst(source: &Path, output: &Path, cfg: &ArchiveConfig) -> Result<()> {
    if non_empty_password(cfg).is_some() {
        anyhow::bail!(
            "Password protection is not supported for .tar.zst archives; use .7z or omit password"
        );
    }

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

fn create_seven_zip(source: &Path, output: &Path, cfg: &ArchiveConfig) -> Result<()> {
    let password = non_empty_password(cfg);

    if cfg.reuse_existing && output.exists() {
        let valid_size = fs::metadata(output).map(|m| m.len() > 0).unwrap_or(false);
        if valid_size && test_seven_zip(output, password).is_ok() {
            info!("Reusing verified 7z archive: {}", output.display());
            return Ok(());
        }
        warn!(
            "Existing 7z archive failed verification or is empty, removing and recreating: {}",
            output.display()
        );
        let _ = fs::remove_file(output);
    }

    let file_name = output
        .file_name()
        .and_then(|n| n.to_str())
        .context("7z output path must have a valid file name")?;
    let partial_name = format!(".{file_name}.{}.partial.7z", Uuid::new_v4().simple());
    let partial_path = match output.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => parent.join(&partial_name),
        None => std::path::PathBuf::from(&partial_name),
    };

    #[cfg(unix)]
    if password.is_some() {
        warn!(
            "Passing 7z archive password via CLI argument on Unix may expose it to local process listing"
        );
    }

    info!(
        "Creating 7z archive {} from {}",
        output.display(),
        source.display()
    );

    let args = build_seven_zip_create_args(source, &partial_path, cfg);
    let result = Command::new("7z")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .context("Failed to execute 7z command. Make sure 7-Zip is installed and in PATH");

    let cmd_out = match result {
        Ok(out) => out,
        Err(err) => {
            let _ = fs::remove_file(&partial_path);
            return Err(err);
        }
    };

    if !cmd_out.status.success() {
        let _ = fs::remove_file(&partial_path);
        let stderr = String::from_utf8_lossy(&cmd_out.stderr);
        anyhow::bail!("Failed to create 7z archive: {}", stderr.trim());
    }

    if let Err(err) = fs::rename(&partial_path, output) {
        let _ = fs::remove_file(&partial_path);
        return Err(err).with_context(|| {
            format!(
                "Failed to rename partial 7z archive {} to {}",
                partial_path.display(),
                output.display()
            )
        });
    }

    Ok(())
}

fn build_seven_zip_create_args(
    source: &Path,
    partial_output: &Path,
    cfg: &ArchiveConfig,
) -> Vec<OsString> {
    let level = cfg.level.unwrap_or(5);
    let dict = cfg.dict_size.as_deref().unwrap_or("32m");

    let mut args: Vec<OsString> = vec![
        "a".into(),
        format!("-mx={level}").into(),
        "-m0=lzma2".into(),
        format!("-md={dict}").into(),
        "-ms=off".into(),
        "-y".into(),
    ];

    if let Some(threads) = cfg.threads {
        args.push(format!("-mmt={}", threads.max(1)).into());
    }

    if let Some(pwd) = non_empty_password(cfg) {
        args.push(format!("-p{pwd}").into());
        args.push("-mhe=on".into());
    }

    args.push("--".into());
    args.push(partial_output.as_os_str().to_os_string());
    args.push(source.as_os_str().to_os_string());
    args
}

fn build_seven_zip_test_args(archive_path: &Path, password: Option<&str>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["t".into(), "-y".into()];
    if let Some(pwd) = password.filter(|p| !p.trim().is_empty()) {
        args.push(format!("-p{pwd}").into());
    }
    args.push("--".into());
    args.push(archive_path.as_os_str().to_os_string());
    args
}

fn test_seven_zip(archive_path: &Path, password: Option<&str>) -> Result<()> {
    let args = build_seven_zip_test_args(archive_path, password);
    let out = Command::new("7z")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .context("Failed to execute `7z t` to verify existing archive")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("7z integrity check failed: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            ArchiveFormat::from_path(Path::new("out/20260611.7z")).unwrap(),
            ArchiveFormat::SevenZip
        );
        assert!(ArchiveFormat::from_path(Path::new("out/20260611.zip")).is_err());
    }

    #[test]
    fn archive_config_debug_redacts_password() {
        let cfg = ArchiveConfig {
            password: Some(Zeroizing::new("super-secret".to_string())),
            ..Default::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("<redacted>"));
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

    #[test]
    fn tar_zst_rejects_password() {
        let base =
            std::env::temp_dir().join(format!("spider_common_pwd_{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&base).unwrap();
        let src = base.join("f.txt");
        fs::write(&src, b"secret").unwrap();
        let cfg = ArchiveConfig {
            password: Some(Zeroizing::new("pw".to_string())),
            ..Default::default()
        };
        let err = create_archive(&src, &base.join("out.tar.zst"), &cfg).unwrap_err();
        assert!(err.to_string().contains("not supported"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn seven_zip_args_include_defaults_mhe_and_double_dash() {
        let cfg = ArchiveConfig {
            password: Some(Zeroizing::new("my-pass".to_string())),
            ..Default::default()
        };
        let args: Vec<String> = build_seven_zip_create_args(
            Path::new("-tricky-source"),
            Path::new("out/.partial.7z"),
            &cfg,
        )
        .into_iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();

        assert!(args.contains(&"-mx=5".to_string()));
        assert!(args.contains(&"-md=32m".to_string()));
        assert!(args.contains(&"-mhe=on".to_string()));
        assert!(args.contains(&"-pmy-pass".to_string()));
        let dd_pos = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dd_pos + 1], "out/.partial.7z");
        assert_eq!(args[dd_pos + 2], "-tricky-source");

        let test_args: Vec<String> =
            build_seven_zip_test_args(Path::new("-archive.7z"), Some("my-pass"))
                .into_iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
        assert_eq!(test_args[0], "t");
        assert!(test_args.contains(&"-pmy-pass".to_string()));
        let t_dd = test_args.iter().position(|a| a == "--").unwrap();
        assert_eq!(test_args[t_dd + 1], "-archive.7z");
    }
}
