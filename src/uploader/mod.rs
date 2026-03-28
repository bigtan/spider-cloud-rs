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

pub struct UploadManager {
    uploaders: Vec<(Box<dyn Uploader>, String)>,
}

impl UploadManager {
    pub fn new() -> Self {
        Self {
            uploaders: Vec::new(),
        }
    }

    pub fn add<U>(&mut self, uploader: U, dest_path: impl Into<String>) -> &mut Self
    where
        U: Uploader + 'static,
    {
        self.uploaders.push((Box::new(uploader), dest_path.into()));
        self
    }

    pub fn has_uploaders(&self) -> bool {
        !self.uploaders.is_empty()
    }

    pub fn upload_file(&mut self, file_path: &str, ctx: &UploadContext) -> Result<UploadResult> {
        if self.uploaders.is_empty() {
            return Ok(UploadResult::empty());
        }

        let mut results = Vec::with_capacity(self.uploaders.len());

        for (uploader, dest_path) in &mut self.uploaders {
            let expanded_path = ctx.expand(dest_path);
            let name = uploader.name().to_string();
            match uploader.upload(file_path, &expanded_path) {
                Ok(()) => results.push(UploadAttempt::success(name)),
                Err(err) => results.push(UploadAttempt::failure(name, err.to_string())),
            }
        }

        Ok(UploadResult::from_attempts(results))
    }
}

impl Default for UploadManager {
    fn default() -> Self {
        Self::new()
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
pub struct UploadResult {
    pub overall_success: bool,
    pub attempts: Vec<UploadAttempt>,
}

impl UploadResult {
    pub fn empty() -> Self {
        Self {
            overall_success: false,
            attempts: Vec::new(),
        }
    }

    pub fn from_attempts(attempts: Vec<UploadAttempt>) -> Self {
        let overall_success =
            !attempts.is_empty() && attempts.iter().all(|attempt| attempt.success);
        Self {
            overall_success,
            attempts,
        }
    }
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

    struct FakeUploader {
        name: String,
        uploads: Vec<(String, String)>,
        succeed: bool,
    }

    impl FakeUploader {
        fn new(name: &str, succeed: bool) -> Self {
            Self {
                name: name.to_string(),
                uploads: Vec::new(),
                succeed,
            }
        }
    }

    impl Uploader for FakeUploader {
        fn name(&self) -> &str {
            &self.name
        }

        fn upload(&mut self, file_path: &str, dest_path: &str) -> Result<()> {
            self.uploads
                .push((file_path.to_string(), dest_path.to_string()));
            if self.succeed {
                Ok(())
            } else {
                anyhow::bail!("{} failed", self.name);
            }
        }
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
    fn upload_manager_reports_results() {
        let mut manager = UploadManager::new();
        let mut ctx = UploadContext::new();
        ctx.insert("date", "20250203");

        manager.add(FakeUploader::new("A", true), "/x/{date}");
        manager.add(FakeUploader::new("B", false), "/y/{date}");

        let result = manager.upload_file("file.tar.zst", &ctx).unwrap();
        assert!(!result.overall_success);
        assert_eq!(result.attempts.len(), 2);
        assert_eq!(result.attempts[0].name, "A");
        assert!(result.attempts[0].success);
        assert_eq!(result.attempts[1].name, "B");
        assert!(!result.attempts[1].success);
        assert!(result.attempts[1].error.is_some());
    }

    #[test]
    fn upload_manager_continues_after_failure() {
        let mut manager = UploadManager::new();
        let ctx = UploadContext::with_date("20250203");

        manager.add(FakeUploader::new("A", false), "/x/{date}");
        manager.add(FakeUploader::new("B", true), "/y/{date}");

        let result = manager.upload_file("file.tar.zst", &ctx).unwrap();
        assert_eq!(result.attempts.len(), 2);
        assert_eq!(result.attempts[0].name, "A");
        assert!(!result.attempts[0].success);
        assert_eq!(result.attempts[1].name, "B");
        assert!(result.attempts[1].success);
    }
}
