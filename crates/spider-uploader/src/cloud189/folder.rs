use std::thread::sleep;

use quick_xml::de::from_str as xml_from_str;
use spider_common::retry::backoff_with_jitter;
use tracing::warn;
use url::Url;

use super::Cloud189Client;
use super::crypto::{apply_signature, encode_form_urlencoded, with_client_params, with_query};
use super::types::{
    BACKOFF_MAX_MS, DIR_OP_BACKOFF_BASE_MS, DIR_OP_MAX_RETRIES, Folder, ListFilesResp,
    ListFilesResult, MkdirResp, ROOT_FOLDER_ID, XmlListFiles, XmlMkdirResp, classify_res_code,
};
use crate::error::{Result, UploadError, redact_secret_params, snippet};

impl Cloud189Client {
    pub(super) fn resolve_folder(&mut self, remote_dir: &str, create: bool) -> Result<String> {
        let cleaned = remote_dir.trim();
        if cleaned == "/" || cleaned.is_empty() {
            return Ok(ROOT_FOLDER_ID.to_string());
        }
        if !cleaned.starts_with('/') {
            return Err(UploadError::InvalidConfig(format!(
                "Cloud189 destination path must start with '/': {remote_dir}"
            )));
        }

        let segments: Vec<&str> = cleaned.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Ok(ROOT_FOLDER_ID.to_string());
        }

        let canonical = format!("/{}", segments.join("/"));
        if let Some(cached) = self.folder_cache.get(&canonical) {
            return Ok(cached.clone());
        }

        let mut current = ROOT_FOLDER_ID.to_string();
        let mut prefix = String::new();
        for segment in segments {
            prefix.push('/');
            prefix.push_str(segment);
            if let Some(cached) = self.folder_cache.get(&prefix) {
                current = cached.clone();
                continue;
            }

            let folders = self.list_folders(&current)?;
            let next_id = if let Some(folder) = folders.iter().find(|f| f.name == segment) {
                folder.id.clone()
            } else if !create {
                return Err(UploadError::Api {
                    code: "FolderNotFound".to_string(),
                    message: format!("remote directory not found: {segment}"),
                });
            } else {
                self.create_folder(&current, segment)?
            };
            self.folder_cache.insert(prefix.clone(), next_id.clone());
            current = next_id;
        }
        Ok(current)
    }

    pub(super) fn invalidate_folder_cache(&mut self, remote_dir: &str) {
        let segments: Vec<&str> = remote_dir
            .trim()
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        if segments.is_empty() {
            self.folder_cache.clear();
            return;
        }
        let canonical = format!("/{}", segments.join("/"));
        self.folder_cache.retain(|cached_path, _| {
            !canonical.starts_with(cached_path.as_str())
                && !cached_path.starts_with(canonical.as_str())
        });
    }

    fn list_folders(&mut self, parent_id: &str) -> Result<Vec<Folder>> {
        match self.list_folders_once(parent_id) {
            Ok(folders) => Ok(folders),
            Err(err) if err.is_auth_error() => {
                warn!(
                    "Cloud189 list files returned auth error, refreshing session and retrying once: {err}"
                );
                self.force_refresh_session()?;
                self.list_folders_once(parent_id)
            }
            Err(err) => Err(err),
        }
    }

    fn list_folders_once(&self, parent_id: &str) -> Result<Vec<Folder>> {
        let mut page = 1;
        let mut folders = Vec::new();
        loop {
            let data = self.list_folders_page(parent_id, page)?;
            folders.extend(data.folders);
            match (data.count, data.page_size) {
                (Some(count), Some(page_size)) if page_size > 0 => {
                    if page * page_size >= count {
                        break;
                    }
                }
                _ => break,
            }
            page += 1;
        }
        Ok(folders)
    }

    fn list_folders_page(&self, parent_id: &str, page: i32) -> Result<ListFilesResult> {
        let mut attempt = 0u32;
        loop {
            let params = vec![
                ("folderId".to_string(), parent_id.to_string()),
                ("fileType".to_string(), "0".to_string()),
                ("mediaType".to_string(), "0".to_string()),
                ("mediaAttr".to_string(), "0".to_string()),
                ("iconOption".to_string(), "0".to_string()),
                ("orderBy".to_string(), "filename".to_string()),
                ("descending".to_string(), "true".to_string()),
                ("pageNum".to_string(), page.to_string()),
                ("pageSize".to_string(), "100".to_string()),
            ];

            let result = (|| -> Result<ListFilesResult> {
                let url = with_query(
                    &format!("{}/listFiles.action", self.endpoints.api_base),
                    &params,
                )?;
                let url = with_client_params(url);
                let mut req = self.api_client.get(url);
                req = apply_signature(
                    req,
                    self.config.session.as_ref(),
                    "GET",
                    "/listFiles.action",
                    None,
                );
                let resp = req
                    .send()
                    .map_err(|e| UploadError::from_reqwest("Cloud189 list files", e))?;
                let status = resp.status();
                let text = resp
                    .text()
                    .map_err(|e| UploadError::from_reqwest("read Cloud189 list files body", e))?;
                if !status.is_success() {
                    return Err(UploadError::from_http_status(status.as_u16(), text));
                }
                parse_list_files_text(&text)
            })();

            match result {
                Ok(data) => return Ok(data),
                Err(err) => {
                    if !err.is_retryable() || attempt >= DIR_OP_MAX_RETRIES {
                        return Err(err);
                    }
                    let delay =
                        backoff_with_jitter(attempt, DIR_OP_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
                    warn!(
                        "Cloud189 list files hit retryable error (attempt {}/{}), retrying in {:?}: {}",
                        attempt + 1,
                        DIR_OP_MAX_RETRIES,
                        delay,
                        err
                    );
                    attempt += 1;
                    sleep(delay);
                }
            }
        }
    }

    fn create_folder(&mut self, parent_id: &str, name: &str) -> Result<String> {
        match self.create_folder_once(parent_id, name) {
            Ok(id) => Ok(id),
            Err(err) if err.is_auth_error() => {
                warn!(
                    "Cloud189 create folder returned auth error, refreshing session and retrying once: {err}"
                );
                self.force_refresh_session()?;
                self.create_folder_once(parent_id, name)
            }
            Err(err) => Err(err),
        }
    }

    fn create_folder_once(&self, parent_id: &str, name: &str) -> Result<String> {
        let mut attempt = 0u32;
        loop {
            let params = vec![
                ("folderName".to_string(), name.to_string()),
                ("relativePath".to_string(), String::new()),
                ("parentFolderId".to_string(), parent_id.to_string()),
            ];

            let result = (|| -> Result<String> {
                let url = with_client_params(Url::parse(&format!(
                    "{}/createFolder.action",
                    self.endpoints.api_base
                ))?);
                let mut req = self
                    .api_client
                    .post(url)
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(encode_form_urlencoded(&params));
                req = apply_signature(
                    req,
                    self.config.session.as_ref(),
                    "POST",
                    "/createFolder.action",
                    None,
                );
                let resp = req
                    .send()
                    .map_err(|e| UploadError::from_reqwest("Cloud189 create folder", e))?;
                let status = resp.status();
                let text = resp.text().map_err(|e| {
                    UploadError::from_reqwest("read Cloud189 create folder body", e)
                })?;
                if !status.is_success() {
                    return Err(UploadError::from_http_status(status.as_u16(), text));
                }
                parse_mkdir_text(&text)
            })();

            match result {
                Ok(id) => return Ok(id),
                Err(err) => {
                    if !err.is_retryable() || attempt >= DIR_OP_MAX_RETRIES {
                        return Err(err);
                    }
                    let delay =
                        backoff_with_jitter(attempt, DIR_OP_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
                    warn!(
                        "Cloud189 create folder hit retryable error (attempt {}/{}), retrying in {:?}: {}",
                        attempt + 1,
                        DIR_OP_MAX_RETRIES,
                        delay,
                        err
                    );
                    attempt += 1;
                    sleep(delay);
                }
            }
        }
    }
}

pub(super) fn parse_list_files_text(text: &str) -> Result<ListFilesResult> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('<') {
        if is_html_error_body(trimmed) {
            return Err(UploadError::TransientServer {
                status: 503,
                body: snippet(trimmed),
            });
        }
        let data: XmlListFiles = xml_from_str(text).map_err(|e| {
            UploadError::Protocol(format!(
                "decode Cloud189 list files xml: {e}; body: {}",
                redact_secret_params(&snippet(text))
            ))
        })?;
        let folders = data
            .file_list
            .folders
            .into_iter()
            .map(|f| Folder {
                id: f.id,
                name: f.name,
            })
            .collect();
        return Ok(ListFilesResult {
            folders,
            count: data.file_list.count,
            page_size: data.file_list.file_list_size,
        });
    }

    let data: ListFilesResp = serde_json::from_str(text).map_err(|e| {
        UploadError::Protocol(format!(
            "decode Cloud189 list files json: {e}; body: {}",
            redact_secret_params(&snippet(text))
        ))
    })?;
    if data.res_code != 0 {
        return Err(classify_res_code(data.res_code, &data.res_message));
    }
    Ok(ListFilesResult {
        folders: data.file_list.folders,
        count: Some(data.file_list.count),
        page_size: Some(data.file_list.file_list_size),
    })
}

pub(super) fn parse_mkdir_text(text: &str) -> Result<String> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('<') {
        if is_html_error_body(trimmed) {
            return Err(UploadError::TransientServer {
                status: 503,
                body: snippet(trimmed),
            });
        }
        let data: XmlMkdirResp = xml_from_str(text).map_err(|e| {
            UploadError::Protocol(format!(
                "decode Cloud189 create folder xml: {e}; body: {}",
                redact_secret_params(&snippet(text))
            ))
        })?;
        let res_code = data.res_code.unwrap_or(0);
        if res_code != 0 {
            let msg = data
                .res_message
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(classify_res_code(res_code, &msg));
        }
        return data
            .id
            .ok_or_else(|| UploadError::Protocol("create folder response missing id".to_string()));
    }

    let data: MkdirResp = serde_json::from_str(text).map_err(|e| {
        UploadError::Protocol(format!(
            "decode Cloud189 create folder json: {e}; body: {}",
            redact_secret_params(&snippet(text))
        ))
    })?;
    if data.res_code != 0 {
        return Err(classify_res_code(data.res_code, &data.res_message));
    }
    Ok(data.id)
}

pub(super) fn is_html_error_body(trimmed: &str) -> bool {
    let lower = trimmed
        .chars()
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase();
    lower.starts_with("<!doctype html") || lower.starts_with("<html")
}
