use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::sleep;

use reqwest::blocking::{Body, Client, Response};
use serde::de::DeserializeOwned;
use spider_common::retry::backoff_with_jitter;
use tracing::{info, warn};
use url::Url;

use super::Cloud189Client;
use super::auth::api_error;
use super::crypto::{
    aes_ecb_hex, apply_signature, encode_param_plain, parse_request_headers, with_client_params,
};
use super::folder::is_html_error_body;
use super::types::{
    BACKOFF_MAX_MS, COMMIT_UPLOAD_BACKOFF_BASE_MS, COMMIT_UPLOAD_MAX_RETRIES, CommitUploadResp,
    InitUploadResp, UPLOAD_PART_BACKOFF_BASE_MS, UPLOAD_PART_MAX_RETRIES,
    UPLOAD_REQ_BACKOFF_BASE_MS, UPLOAD_REQ_MAX_RETRIES, UploadUrl, UploadUrlsResp,
    classify_api_error, is_retryable_commit_code, is_success_code,
};
use crate::digest::{CLOUD189_SLICE_SIZE as SLICE_SIZE, FileDigests, PartSpec};
use crate::error::{Result, UploadError, redact_secret_params, redact_url, snippet};

impl Cloud189Client {
    pub(super) fn upload_file_with_digests(
        &self,
        local: &Path,
        parent_id: &str,
        digests: &FileDigests,
    ) -> Result<()> {
        if !local.is_file() {
            return Err(UploadError::InvalidConfig(format!(
                "local file not found or not a regular file: {}",
                local.display()
            )));
        }
        if digests.file_size > 0 && digests.cloud189_parts.is_empty() {
            return Err(UploadError::InvalidConfig(
                "FileDigests missing cloud189_parts for non-empty file".to_string(),
            ));
        }

        let file_name = local
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| UploadError::InvalidConfig("invalid local file name".to_string()))?;

        // `fileName` is included in Cloud189's `decodefields` header, so URL-encoding
        // prevents `&` or `=` in filenames from corrupting parameter boundaries.
        let encoded_file_name = urlencoding::encode(file_name).into_owned();
        let init_params = vec![
            ("parentFolderId".to_string(), parent_id.to_string()),
            ("fileName".to_string(), encoded_file_name),
            ("fileSize".to_string(), digests.file_size.to_string()),
            ("sliceSize".to_string(), SLICE_SIZE.to_string()),
            ("fileMd5".to_string(), digests.full_md5_hex.clone()),
            ("sliceMd5".to_string(), digests.cloud189_slice_md5.clone()),
            (
                "extend".to_string(),
                r#"{"opScene":"1","relativepath":"","rootfolderid":""}"#.to_string(),
            ),
        ];

        let init_resp: InitUploadResp =
            self.upload_json_with_retry("init upload", "/person/initMultiUpload", &init_params)?;
        if !is_success_code(&init_resp.code) {
            return Err(classify_api_error(
                &init_resp.code,
                "initMultiUpload failed",
            ));
        }

        let upload_id = init_resp.data.upload_file_id;
        if init_resp.data.file_data_exists == 1 {
            info!(
                "Cloud189 rapid upload (秒传) succeeded for {}",
                local.display()
            );
            self.commit_upload(&upload_id, None, None, None)?;
            return Ok(());
        }

        if !digests.cloud189_parts.is_empty() {
            // `partInfo` is NOT in `decodefields` and contains base64 `=`, `+`, `/`;
            // do NOT URL-encode `partInfo`.
            let mut part_info = Vec::with_capacity(digests.cloud189_parts.len());
            for part in &digests.cloud189_parts {
                part_info.push(format!("{}-{}", part.index + 1, part.name));
            }
            let url_params = vec![
                ("partInfo".to_string(), part_info.join(",")),
                ("uploadFileId".to_string(), upload_id.clone()),
            ];
            let urls: UploadUrlsResp = self.upload_json_with_retry(
                "upload urls",
                "/person/getMultiUploadUrls",
                &url_params,
            )?;
            if !is_success_code(&urls.code) {
                return Err(classify_api_error(&urls.code, "getMultiUploadUrls failed"));
            }

            // Validate every UploadUrl upfront before spawning transfer threads.
            for part in &digests.cloud189_parts {
                let key = format!("partNumber_{}", part.index + 1);
                let info = urls.upload_urls.get(&key).ok_or_else(|| {
                    UploadError::Protocol(format!("missing Cloud189 upload url for {key}"))
                })?;
                validate_upload_url(&info.request_url, self.endpoints.allow_loopback_http)?;
            }

            info!(
                "Uploading {} Cloud189 part(s) (concurrency={})",
                digests.cloud189_parts.len(),
                self.max_concurrent_chunks
            );
            self.upload_parts_parallel(local, &digests.cloud189_parts, &urls.upload_urls)?;
        }

        self.commit_upload(&upload_id, None, None, None)?;
        Ok(())
    }

    fn upload_parts_parallel(
        &self,
        local: &Path,
        parts: &[PartSpec],
        upload_urls: &std::collections::HashMap<String, UploadUrl>,
    ) -> Result<()> {
        let worker_count = self.max_concurrent_chunks.min(parts.len()).max(1);
        let next_pos = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let allow_loopback_http = self.endpoints.allow_loopback_http;

        let results: Vec<Result<()>> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let next_pos = &next_pos;
                let cancelled = &cancelled;
                let transfer_client = &self.transfer_client;

                handles.push(scope.spawn(move || -> Result<()> {
                    let mut file = File::open(local)?;

                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        let pos = next_pos.fetch_add(1, Ordering::Relaxed);
                        if pos >= parts.len() {
                            return Ok(());
                        }
                        let part = &parts[pos];
                        let key = format!("partNumber_{}", part.index + 1);
                        let info = upload_urls.get(&key).ok_or_else(|| {
                            UploadError::Protocol(format!("missing Cloud189 upload url for {key}"))
                        })?;

                        if let Err(err) = upload_single_part(
                            transfer_client,
                            &mut file,
                            part,
                            info,
                            allow_loopback_http,
                            cancelled,
                        ) {
                            cancelled.store(true, Ordering::Relaxed);
                            return Err(err);
                        }
                    }
                }));
            }

            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(UploadError::Protocol(
                            "Cloud189 part upload worker panicked".to_string(),
                        ))
                    })
                })
                .collect()
        });

        for res in results {
            res?;
        }
        Ok(())
    }

    fn commit_upload(
        &self,
        upload_id: &str,
        file_md5: Option<&str>,
        slice_md5: Option<&str>,
        lazy_check: Option<&str>,
    ) -> Result<()> {
        let mut params = vec![("uploadFileId".to_string(), upload_id.to_string())];
        if let (Some(f), Some(s), Some(l)) = (file_md5, slice_md5, lazy_check) {
            params.push(("fileMd5".to_string(), f.to_string()));
            params.push(("sliceMd5".to_string(), s.to_string()));
            params.push(("lazyCheck".to_string(), l.to_string()));
        }

        let mut attempt = 0u32;
        loop {
            let result: CommitUploadResp = self.upload_json_with_retry(
                "commit upload",
                "/person/commitMultiUploadFile",
                &params,
            )?;
            if is_success_code(&result.code) {
                return Ok(());
            }

            if !is_retryable_commit_code(&result.code) || attempt >= COMMIT_UPLOAD_MAX_RETRIES {
                return Err(classify_api_error(&result.code, "commit upload failed"));
            }

            let delay = backoff_with_jitter(attempt, COMMIT_UPLOAD_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
            warn!(
                "Cloud189 commit upload returned '{}' (attempt {}/{}), retrying in {:?}",
                result.code,
                attempt + 1,
                COMMIT_UPLOAD_MAX_RETRIES,
                delay
            );
            attempt += 1;
            sleep(delay);
        }
    }

    pub(super) fn upload_get(&self, path: &str, params: &[(String, String)]) -> Result<Response> {
        let secret = self
            .config
            .session
            .as_ref()
            .filter(|s| s.is_valid())
            .ok_or_else(|| UploadError::AuthExpired("missing Cloud189 session".to_string()))?
            .secret
            .clone();
        let plain = encode_param_plain(params)?;
        let encrypted = aes_ecb_hex(plain.as_bytes(), &secret)?;
        let mut url = Url::parse(&format!(
            "{}{path}?params={encrypted}",
            self.endpoints.upload_base
        ))?;
        url = with_client_params(url);
        let mut req = self.api_client.get(url);
        req = apply_signature(
            req,
            self.config.session.as_ref(),
            "GET",
            path,
            Some(&encrypted),
        );
        req = req
            .header(
                "decodefields",
                "familyId,parentFolderId,fileName,fileMd5,fileSize,sliceMd5,sliceSize,albumId,extend,lazyCheck,isLog",
            )
            .header(reqwest::header::ACCEPT, "application/json;charset=UTF-8")
            .header(reqwest::header::CACHE_CONTROL, "no-cache");
        req.send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 upload API request", e))
    }

    fn upload_json_with_retry<T>(
        &self,
        label: &str,
        path: &str,
        params: &[(String, String)],
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let mut attempt = 0u32;
        loop {
            match self.upload_json(label, path, params) {
                Ok(data) => return Ok(data),
                Err(err) => {
                    if !err.is_retryable() || attempt >= UPLOAD_REQ_MAX_RETRIES {
                        return Err(err);
                    }
                    let delay =
                        backoff_with_jitter(attempt, UPLOAD_REQ_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
                    warn!(
                        "Cloud189 {label} hit retryable error (attempt {}/{}), retrying in {:?}: {}",
                        attempt + 1,
                        UPLOAD_REQ_MAX_RETRIES,
                        delay,
                        err
                    );
                    attempt += 1;
                    sleep(delay);
                }
            }
        }
    }

    fn upload_json<T>(&self, label: &str, path: &str, params: &[(String, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let resp = self.upload_get(path, params)?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| UploadError::from_reqwest(&format!("read {label} body"), e))?;
        if !status.is_success() {
            return Err(UploadError::from_http_status(status.as_u16(), text));
        }

        let trimmed = text.trim_start();
        if is_html_error_body(trimmed) {
            return Err(UploadError::TransientServer {
                status: status.as_u16(),
                body: snippet(trimmed),
            });
        }

        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            let msg = format!(
                "decode {label} json: {e}; body: {}",
                redact_secret_params(&snippet(&text))
            );
            if e.is_eof() {
                UploadError::TransientServer {
                    status: status.as_u16(),
                    body: msg,
                }
            } else {
                UploadError::Protocol(msg)
            }
        })?;

        if let Some(err) = api_error(&value) {
            return Err(classify_api_error(&err.code, &err.message));
        }

        serde_json::from_value(value).map_err(|e| {
            UploadError::Protocol(format!(
                "decode {label} body: {e}; body: {}",
                redact_secret_params(&snippet(&text))
            ))
        })
    }
}

pub(super) fn validate_upload_url(request_url: &str, allow_loopback_http: bool) -> Result<()> {
    if request_url.starts_with("https://") {
        return Ok(());
    }
    if allow_loopback_http
        && (request_url.starts_with("http://127.0.0.1")
            || request_url.starts_with("http://localhost"))
    {
        return Ok(());
    }
    Err(UploadError::Protocol(format!(
        "refusing non-HTTPS Cloud189 upload URL: {}",
        redact_url(request_url)
    )))
}

fn upload_single_part(
    transfer_client: &Client,
    file: &mut File,
    part: &PartSpec,
    info: &UploadUrl,
    allow_loopback_http: bool,
    cancelled: &AtomicBool,
) -> Result<()> {
    validate_upload_url(&info.request_url, allow_loopback_http)?;
    let headers = parse_request_headers(&info.request_header)?;
    info!(
        "Starting Cloud189 part upload {} (offset={}, len={})",
        part.index + 1,
        part.offset,
        part.len
    );

    let mut attempt = 0u32;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(UploadError::Protocol(
                "Cloud189 part upload aborted due to sibling part failure".to_string(),
            ));
        }

        file.seek(SeekFrom::Start(part.offset))?;
        let part_reader = file.try_clone()?.take(part.len);
        let body = Body::sized(part_reader, part.len);
        let result = transfer_client
            .put(&info.request_url)
            .headers(headers.clone())
            .body(body)
            .send();

        let err = match result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    info!("Cloud189 part {} upload completed", part.index + 1);
                    return Ok(());
                }
                let body = resp.text().unwrap_or_default();
                UploadError::from_http_status(status.as_u16(), body)
            }
            Err(e) => {
                UploadError::from_reqwest(&format!("Cloud189 part {} upload", part.index + 1), e)
            }
        };

        if err.is_retryable() && attempt < UPLOAD_PART_MAX_RETRIES {
            let delay = backoff_with_jitter(attempt, UPLOAD_PART_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
            warn!(
                "Cloud189 part {} failed (attempt {}/{}), retrying in {:?}: {}",
                part.index + 1,
                attempt + 1,
                UPLOAD_PART_MAX_RETRIES,
                delay,
                err
            );
            attempt += 1;
            sleep(delay);
            continue;
        }

        return Err(err);
    }
}
