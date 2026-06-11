use std::collections::HashMap;

use crate::Result;

pub trait Uploader: Send {
    fn name(&self) -> &str;
    fn upload(&mut self, file_path: &str, dest_path: &str) -> Result<()>;
}

#[derive(Debug, Default, Clone)]
pub struct UploadContext {
    vars: HashMap<String, String>,
}

impl UploadContext {
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }

    pub fn with_date(date: impl Into<String>) -> Self {
        let mut ctx = Self::new();
        ctx.insert("date", date);
        ctx
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    pub fn expand(&self, template: &str) -> String {
        expand_placeholders(template, &self.vars)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadAttempt {
    pub name: String,
    pub success: bool,
    pub error: Option<String>,
}

impl UploadAttempt {
    pub fn success(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            success: true,
            error: None,
        }
    }

    pub fn failure(name: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            success: false,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadSkippedReason {
    NoUploadersConfigured,
    NoDestinationConfigured,
    OptionalArchiveFailed,
    NoFilesFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub overall_success: bool,
    pub attempts: Vec<UploadAttempt>,
    pub skipped_reason: Option<UploadSkippedReason>,
}

impl UploadResult {
    pub fn empty() -> Self {
        Self {
            overall_success: false,
            attempts: Vec::new(),
            skipped_reason: None,
        }
    }

    pub fn skipped(reason: UploadSkippedReason) -> Self {
        Self {
            overall_success: false,
            attempts: Vec::new(),
            skipped_reason: Some(reason),
        }
    }

    pub fn from_attempts(attempts: Vec<UploadAttempt>) -> Self {
        let overall_success =
            !attempts.is_empty() && attempts.iter().all(|attempt| attempt.success);
        Self {
            overall_success,
            attempts,
            skipped_reason: None,
        }
    }
}

/// Write a credentials file with owner-only permissions on Unix; the default
/// umask would otherwise leave session secrets world-readable.
pub(crate) fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}

/// Read until `buf` is full or EOF. `Read::read` may return short reads even
/// mid-file, which would silently desync fixed-size chunk boundaries.
pub(crate) fn read_full(reader: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

fn expand_placeholders(template: &str, vars: &HashMap<String, String>) -> String {
    let mut output = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            let mut key = String::new();
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == '}' {
                    break;
                }
                key.push(next);
            }

            if key.is_empty() {
                output.push('{');
            } else if let Some(value) = vars.get(&key) {
                output.push_str(value);
            } else {
                output.push('{');
                output.push_str(&key);
                output.push('}');
            }
        } else {
            output.push(ch);
        }
    }

    output
}

pub mod baidu;

pub use baidu::BaiduPanUploader;

pub mod cloud189;

pub use cloud189::Cloud189Uploader;

#[cfg(test)]
mod tests {
    use super::*;

    /// Reader that returns at most 3 bytes per read call to simulate short reads.
    struct ShortReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl std::io::Read for ShortReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (self.data.len() - self.pos).min(buf.len()).min(3);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn read_full_fills_buffer_across_short_reads() {
        let mut reader = ShortReader {
            data: (0..10).collect(),
            pos: 0,
        };
        let mut buf = [0u8; 8];
        assert_eq!(read_full(&mut reader, &mut buf).unwrap(), 8);
        assert_eq!(&buf, &[0, 1, 2, 3, 4, 5, 6, 7]);
        let mut rest = [0u8; 8];
        assert_eq!(read_full(&mut reader, &mut rest).unwrap(), 2);
        assert_eq!(&rest[..2], &[8, 9]);
    }

    #[test]
    fn expand_placeholders_replaces_known_vars() {
        let mut ctx = UploadContext::new();
        ctx.insert("date", "20250203");
        ctx.insert("name", "backup");
        let out = ctx.expand("/data/{date}/{name}/");
        assert_eq!(out, "/data/20250203/backup/");
    }

    #[test]
    fn expand_placeholders_keeps_unknown_vars() {
        let ctx = UploadContext::new();
        let out = ctx.expand("/data/{missing}/");
        assert_eq!(out, "/data/{missing}/");
    }

    #[test]
    fn upload_result_aggregates_attempts() {
        let result = UploadResult::from_attempts(vec![
            UploadAttempt::success("A"),
            UploadAttempt::failure("B", "B failed"),
        ]);
        assert!(!result.overall_success);
        assert_eq!(result.attempts.len(), 2);
        assert!(result.attempts[0].success);
        assert!(!result.attempts[1].success);
        assert!(result.attempts[1].error.is_some());

        let all_ok = UploadResult::from_attempts(vec![UploadAttempt::success("A")]);
        assert!(all_ok.overall_success);

        assert!(!UploadResult::empty().overall_success);
        let skipped = UploadResult::skipped(UploadSkippedReason::NoUploadersConfigured);
        assert_eq!(
            skipped.skipped_reason,
            Some(UploadSkippedReason::NoUploadersConfigured)
        );
    }
}
