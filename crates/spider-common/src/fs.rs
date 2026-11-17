use crate::Result;
use chrono::{Local, NaiveDate};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A temporary sibling file that is cleaned up automatically if dropped before [`Self::persist`].
pub struct AtomicTempFile {
    temp_path: PathBuf,
    target_path: PathBuf,
    file: Option<File>,
    persisted: bool,
}

impl AtomicTempFile {
    /// Create a temporary sibling file (`<parent>/.<name>.<uuid>.tmp`) with private permissions on Unix.
    pub fn new(target_path: &Path) -> io::Result<Self> {
        let file_name = target_path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "target path must include a file name: {}",
                    target_path.display()
                ),
            )
        })?;

        let parent = target_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));

        if let Some(dir) = target_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }

        let temp_name = format!(
            ".{}.{}.tmp",
            file_name.to_string_lossy(),
            Uuid::new_v4().simple()
        );
        let temp_path = if target_path
            .parent()
            .is_some_and(|p| !p.as_os_str().is_empty())
        {
            parent.join(temp_name)
        } else {
            PathBuf::from(temp_name)
        };

        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&temp_path)?;

        Ok(Self {
            temp_path,
            target_path: target_path.to_path_buf(),
            file: Some(file),
            persisted: false,
        })
    }

    /// Borrow the underlying temporary [`File`] handle for writing.
    pub fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("AtomicTempFile handle is open until persist")
    }

    /// Return the path of the temporary staging file.
    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    /// Flush to disk, close the file handle (required on Windows before rename),
    /// tighten permissions to `0o600` on Unix, and atomically rename to the target path.
    pub fn persist(mut self) -> io::Result<PathBuf> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
            // Explicitly drop the handle before rename so Windows does not reject
            // the rename with ERROR_SHARING_VIOLATION / ERROR_ACCESS_DENIED.
            drop(file);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.temp_path, fs::Permissions::from_mode(0o600))?;
        }

        fs::rename(&self.temp_path, &self.target_path)?;
        self.persisted = true;
        Ok(self.target_path.clone())
    }
}

impl Drop for AtomicTempFile {
    fn drop(&mut self) {
        // Close any open handle before removing the temp file on Windows.
        drop(self.file.take());
        if !self.persisted {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

/// Write `data` atomically to `path` via a sibling temporary file with `0o600` permissions on Unix.
pub fn write_private_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut temp = AtomicTempFile::new(path)?;
    temp.file_mut().write_all(data)?;
    temp.persist()?;
    Ok(())
}

/// Read until `buf` is full or EOF, retrying across `Interrupted` errors.
pub fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// Validate a user-configured single path-component name (e.g. account ID or archive name)
/// against traversal, CLI option injection, and OS-forbidden filename characters.
pub fn validate_safe_name(value: &str, field_name: &str) -> Result<String> {
    if value.chars().any(|c| c.is_control() || c == '\0') {
        anyhow::bail!("{field_name} contains control characters");
    }

    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{field_name} cannot be empty or whitespace");
    }
    if trimmed == "." || trimmed == ".." {
        anyhow::bail!("{field_name} cannot be '.' or '..'");
    }
    if trimmed.starts_with('-') {
        anyhow::bail!("{field_name} cannot start with '-'");
    }

    const FORBIDDEN: &[char] = &['/', '\\', '<', '>', ':', '"', '|', '?', '*'];
    if let Some(bad) = trimmed.chars().find(|c| FORBIDDEN.contains(c)) {
        anyhow::bail!("{field_name} contains forbidden character '{bad}'");
    }

    Ok(trimmed.to_string())
}

/// Parse an optional date string (`YYYYMMDD` or `YYYY-MM-DD`), defaulting to today's local date
/// when `None` or blank, and returning an error for malformed non-empty strings.
pub fn parse_normalized_date(custom_date: Option<&str>) -> Result<NaiveDate> {
    let Some(raw) = custom_date.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Local::now().date_naive());
    };

    if raw.len() == 8
        && raw.bytes().all(|b| b.is_ascii_digit())
        && let Ok(date) = NaiveDate::parse_from_str(raw, "%Y%m%d")
    {
        return Ok(date);
    }

    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(date);
    }

    anyhow::bail!("Invalid date '{raw}': expected YYYYMMDD or YYYY-MM-DD")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn write_private_atomic_creates_and_overwrites_safely() {
        let dir =
            std::env::temp_dir().join(format!("spider_common_atomic_{}", Uuid::new_v4().simple()));
        let target = dir.join("nested").join("secret.json");

        write_private_atomic(&target, b"first-payload").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first-payload");

        // Overwrite existing file (verifies Windows handle drop before rename)
        write_private_atomic(&target, b"second-payload").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second-payload");

        // Verify no leftover .tmp files
        let leftovers: Vec<_> = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_temp_file_cleans_up_on_drop_without_persist() {
        let dir =
            std::env::temp_dir().join(format!("spider_common_drop_{}", Uuid::new_v4().simple()));
        let target = dir.join("artifact.bin");
        let temp_path;
        {
            let mut temp = AtomicTempFile::new(&target).unwrap();
            temp.file_mut().write_all(b"partial").unwrap();
            temp_path = temp.temp_path().to_path_buf();
            assert!(temp_path.exists());
        }
        assert!(!temp_path.exists());
        assert!(!target.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_temp_file_persists_written_content() {
        let dir =
            std::env::temp_dir().join(format!("spider_common_persist_{}", Uuid::new_v4().simple()));
        let target = dir.join("report.xlsx");

        let mut temp = AtomicTempFile::new(&target).unwrap();
        temp.file_mut().write_all(b"workbook-bytes").unwrap();
        let temp_path = temp.temp_path().to_path_buf();
        let persisted = temp.persist().unwrap();

        assert_eq!(persisted, target);
        assert!(!temp_path.exists());
        assert_eq!(fs::read(&target).unwrap(), b"workbook-bytes");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_full_handles_short_reads() {
        struct ChunkedReader<'a> {
            data: &'a [u8],
            pos: usize,
        }
        impl Read for ChunkedReader<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let n = (self.data.len() - self.pos).min(buf.len()).min(2);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        let mut reader = ChunkedReader {
            data: b"abcdefg",
            pos: 0,
        };
        let mut buf = [0u8; 5];
        assert_eq!(read_full(&mut reader, &mut buf).unwrap(), 5);
        assert_eq!(&buf, b"abcde");
        assert_eq!(read_full(&mut reader, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"fg");
    }

    #[test]
    fn validate_safe_name_accepts_valid_and_rejects_unsafe() {
        assert_eq!(
            validate_safe_name(" account_01 ", "account").unwrap(),
            "account_01"
        );
        assert_eq!(
            validate_safe_name("backup.2026", "archive_name").unwrap(),
            "backup.2026"
        );

        for invalid in [
            "",
            "   ",
            ".",
            "..",
            "..",
            "-rf",
            " -v",
            "a/b",
            "a\\b",
            "../secret",
            "a<b",
            "a>b",
            "a:b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "bad\0name",
            "bad\nname",
            "bad\tname",
        ] {
            assert!(
                validate_safe_name(invalid, "field").is_err(),
                "expected rejection for {invalid:?}"
            );
        }
    }

    #[test]
    fn parse_normalized_date_parses_formats_and_rejects_invalid() {
        let today = Local::now().date_naive();
        assert_eq!(parse_normalized_date(None).unwrap(), today);
        assert_eq!(parse_normalized_date(Some("   ")).unwrap(), today);

        let d1 = parse_normalized_date(Some("20260611")).unwrap();
        assert_eq!((d1.year(), d1.month(), d1.day()), (2026, 6, 11));

        let d2 = parse_normalized_date(Some(" 2026-06-11 ")).unwrap();
        assert_eq!(d2, d1);

        assert!(parse_normalized_date(Some("2026061")).is_err());
        assert!(parse_normalized_date(Some("2026-02-30")).is_err());
        assert!(parse_normalized_date(Some("not-a-date")).is_err());
    }
}
