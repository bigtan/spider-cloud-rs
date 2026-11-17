use std::fmt;
use std::fs::File;
use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::{Client, multipart};
use serde::{Deserialize, Serialize};
use spider_common::fs::write_private_atomic;
use spider_common::retry::backoff_with_jitter;
use tracing::{debug, info, warn};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

use crate::config::{DEFAULT_MAX_CONCURRENT_CHUNKS, MAX_CONCURRENT_CHUNKS, MIN_CONCURRENT_CHUNKS};
use crate::coordinator::Uploader;
use crate::digest::{BAIDU_CHUNK_SIZE, FileDigests, compute_file_digests};
use crate::error::{RedactedStr, Result, UploadError, redact_secret_params, redacted_opt, snippet};

const BASE_URL: &str = "https://pan.baidu.com/rest/2.0/xpan";
const OAUTH_URL: &str = "https://openapi.baidu.com/oauth/2.0";
const PCS_BASE_URL: &str = "https://d.pcs.baidu.com/rest/2.0/pcs";
const API_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const TRANSFER_TIMEOUT: StdDuration = StdDuration::from_secs(300);
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(15);
const CHUNK_UPLOAD_MAX_RETRIES: u32 = 3;
const CHUNK_UPLOAD_BACKOFF_BASE_MS: u64 = 1000;
const API_MAX_RETRIES: u32 = 3;
const API_BACKOFF_BASE_MS: u64 = 800;
const BACKOFF_MAX_MS: u64 = 30_000;

#[derive(Clone)]
struct BaiduEndpoints {
    api_base: String,
    oauth_base: String,
    pcs_base: String,
    allow_loopback_http: bool,
}

impl Default for BaiduEndpoints {
    fn default() -> Self {
        Self {
            api_base: BASE_URL.to_string(),
            oauth_base: OAUTH_URL.to_string(),
            pcs_base: PCS_BASE_URL.to_string(),
            allow_loopback_http: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TokenData {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: DateTime<Utc>,
}

impl fmt::Debug for TokenData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenData")
            .field("access_token", &RedactedStr(&self.access_token))
            .field("refresh_token", &redacted_opt(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Drop for TokenData {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(rt) = self.refresh_token.as_mut() {
            rt.zeroize();
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    errno: i32,
    baidu_name: Option<String>,
    total: Option<u64>,
    used: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PrecreateResponse {
    errno: i32,
    uploadid: Option<String>,
    return_type: Option<i32>,
    block_list: Option<Vec<usize>>,
}

#[derive(Debug, Deserialize)]
struct CreateResponse {
    errno: i32,
    #[allow(dead_code)]
    fs_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LocateUploadServer {
    server: String,
}

#[derive(Debug, Deserialize)]
struct LocateUploadResponse {
    error_code: i32,
    error_msg: Option<String>,
    servers: Option<Vec<LocateUploadServer>>,
}

/// Baidu Pan uploader with OAuth2 authentication and parallel chunk upload.
pub struct BaiduPanUploader {
    app_key: Zeroizing<String>,
    app_secret: Zeroizing<String>,
    config_file: PathBuf,
    token_data: Option<TokenData>,
    api_client: Client,
    transfer_client: Client,
    max_concurrent_chunks: usize,
    dry_run: bool,
    endpoints: BaiduEndpoints,
}

impl fmt::Debug for BaiduPanUploader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BaiduPanUploader")
            .field("app_key", &"[REDACTED]")
            .field("app_secret", &"[REDACTED]")
            .field("config_file", &self.config_file)
            .field("token_data", &self.token_data)
            .field("max_concurrent_chunks", &self.max_concurrent_chunks)
            .field("dry_run", &self.dry_run)
            .finish()
    }
}

impl BaiduPanUploader {
    /// Create a new Baidu Pan uploader with default chunk concurrency (`4`).
    pub fn new(app_key: String, app_secret: String, config_file: Option<PathBuf>) -> Result<Self> {
        Self::with_concurrency(
            app_key,
            app_secret,
            config_file,
            DEFAULT_MAX_CONCURRENT_CHUNKS,
        )
    }

    /// Create a new Baidu Pan uploader with explicit bounded chunk concurrency (`1..=16`).
    pub fn with_concurrency(
        app_key: String,
        app_secret: String,
        config_file: Option<PathBuf>,
        max_concurrent_chunks: usize,
    ) -> Result<Self> {
        let mut uploader = Self::build_client(
            app_key,
            app_secret,
            config_file,
            max_concurrent_chunks,
            false,
            BaiduEndpoints::default(),
        )?;

        uploader.load_tokens()?;

        if uploader.token_data.is_none() {
            info!("No local Baidu Pan token found. Starting authorization flow.");
            uploader.perform_authorization()?;
        } else if !uploader.is_token_valid() {
            info!("Baidu Pan access token expired. Attempting to refresh...");
            if let Err(err) = uploader.refresh_access_token() {
                warn!(
                    "Failed to refresh Baidu Pan token ({err}); falling back to manual authorization."
                );
                uploader.perform_authorization()?;
            } else {
                info!("Baidu Pan access token refreshed successfully during init.");
            }
        }

        Ok(uploader)
    }

    /// Construct a dry-run Baidu Pan uploader without performing network I/O or interactive login.
    pub fn new_dry_run(
        app_key: String,
        app_secret: String,
        config_file: Option<PathBuf>,
        max_concurrent_chunks: usize,
    ) -> Result<Self> {
        Self::build_client(
            app_key,
            app_secret,
            config_file,
            max_concurrent_chunks,
            true,
            BaiduEndpoints::default(),
        )
    }

    fn build_client(
        app_key: String,
        app_secret: String,
        config_file: Option<PathBuf>,
        max_concurrent_chunks: usize,
        dry_run: bool,
        endpoints: BaiduEndpoints,
    ) -> Result<Self> {
        if app_key.trim().is_empty() || app_secret.trim().is_empty() {
            return Err(UploadError::InvalidConfig(
                "Baidu Pan app_key and app_secret must be non-empty".to_string(),
            ));
        }
        if !(MIN_CONCURRENT_CHUNKS..=MAX_CONCURRENT_CHUNKS).contains(&max_concurrent_chunks) {
            return Err(UploadError::InvalidConfig(format!(
                "max_concurrent_chunks must be in {MIN_CONCURRENT_CHUNKS}..={MAX_CONCURRENT_CHUNKS}, got {max_concurrent_chunks}"
            )));
        }

        let config_file = match config_file.filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => path,
            None => dirs::home_dir()
                .ok_or_else(|| {
                    UploadError::InvalidConfig("cannot determine home directory".to_string())
                })?
                .join(".config")
                .join("spider-cloud")
                .join("baidu")
                .join("config.json"),
        };

        let api_client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(API_TIMEOUT)
            .build()
            .map_err(|e| UploadError::from_reqwest("failed to build Baidu API HTTP client", e))?;

        let transfer_client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TRANSFER_TIMEOUT)
            .build()
            .map_err(|e| {
                UploadError::from_reqwest("failed to build Baidu transfer HTTP client", e)
            })?;

        Ok(Self {
            app_key: Zeroizing::new(app_key),
            app_secret: Zeroizing::new(app_secret),
            config_file,
            token_data: None,
            api_client,
            transfer_client,
            max_concurrent_chunks,
            dry_run,
            endpoints,
        })
    }

    fn load_tokens(&mut self) -> Result<()> {
        if !self.config_file.exists() {
            warn!("Baidu token config file not found");
            return Ok(());
        }

        let contents = std::fs::read_to_string(&self.config_file)?;
        let token_data: TokenData = serde_json::from_str(&contents).map_err(|e| {
            UploadError::Protocol(format!(
                "failed to parse Baidu token config {}: {e}",
                self.config_file.display()
            ))
        })?;

        self.token_data = Some(token_data);
        info!("Baidu Pan tokens loaded from config file");
        Ok(())
    }

    fn save_tokens(&mut self, token_response: TokenResponse) -> Result<()> {
        let access_token = token_response.access_token.ok_or_else(|| {
            UploadError::Protocol("Baidu OAuth response missing access_token".to_string())
        })?;
        let expires_in = token_response.expires_in.unwrap_or(2_592_000);
        let expires_at = Utc::now() + Duration::seconds(expires_in.saturating_sub(300));

        let token_data = TokenData {
            access_token,
            refresh_token: token_response.refresh_token.or_else(|| {
                self.token_data
                    .as_ref()
                    .and_then(|t| t.refresh_token.clone())
            }),
            expires_at,
        };

        let json = serde_json::to_string_pretty(&token_data).map_err(|e| {
            UploadError::Protocol(format!("failed to serialize Baidu token data: {e}"))
        })?;
        write_private_atomic(&self.config_file, json.as_bytes())?;

        self.token_data = Some(token_data);
        info!("Baidu Pan tokens saved atomically");
        Ok(())
    }

    fn is_token_valid(&self) -> bool {
        self.token_data
            .as_ref()
            .is_some_and(|t| !t.access_token.is_empty() && Utc::now() < t.expires_at)
    }

    fn get_authorization_url(&self) -> Result<String> {
        let mut url = Url::parse(&format!("{}/authorize", self.endpoints.oauth_base))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", self.app_key.as_str())
            .append_pair("redirect_uri", "oob")
            .append_pair("scope", "basic,netdisk");
        Ok(url.into())
    }

    fn get_access_token(&mut self, code: &str) -> Result<()> {
        info!("Requesting Baidu Pan access token with authorization code");
        let url = format!("{}/token", self.endpoints.oauth_base);
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", self.app_key.as_str()),
            ("client_secret", self.app_secret.as_str()),
            ("redirect_uri", "oob"),
        ];

        let token_response = self.send_oauth_token_request(&url, &params)?;
        self.save_tokens(token_response)?;
        info!("Baidu Pan access token obtained successfully");
        Ok(())
    }

    fn refresh_access_token(&mut self) -> Result<()> {
        let refresh_token = self
            .token_data
            .as_ref()
            .and_then(|t| t.refresh_token.as_ref())
            .filter(|t| !t.is_empty())
            .cloned()
            .ok_or_else(|| {
                UploadError::AuthExpired("no Baidu Pan refresh token available".to_string())
            })?;

        info!("Refreshing Baidu Pan access token");
        let url = format!("{}/token", self.endpoints.oauth_base);
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", self.app_key.as_str()),
            ("client_secret", self.app_secret.as_str()),
        ];

        let token_response = self.send_oauth_token_request(&url, &params)?;
        self.save_tokens(token_response)?;
        info!("Baidu Pan access token refreshed successfully");
        Ok(())
    }

    fn send_oauth_token_request(
        &self,
        url: &str,
        params: &[(&str, &str)],
    ) -> Result<TokenResponse> {
        let response = self
            .api_client
            .post(url)
            .form(params)
            .send()
            .map_err(|e| UploadError::from_reqwest("Baidu OAuth token request failed", e))?;

        let status = response.status();
        let body = response
            .text()
            .map_err(|e| UploadError::from_reqwest("read Baidu OAuth response", e))?;
        if !status.is_success() {
            return Err(UploadError::from_http_status(status.as_u16(), body));
        }

        let token_response: TokenResponse = serde_json::from_str(&body).map_err(|e| {
            UploadError::Protocol(format!(
                "decode Baidu OAuth response: {e}; body: {}",
                redact_secret_params(&snippet(&body))
            ))
        })?;

        if let Some(error) = token_response.error {
            let desc = redact_secret_params(
                token_response
                    .error_description
                    .as_deref()
                    .unwrap_or(&error),
            );
            return Err(classify_oauth_error(&error, &desc));
        }

        Ok(token_response)
    }

    fn get_valid_access_token(&mut self) -> Result<String> {
        if !self.is_token_valid() {
            self.refresh_access_token()?;
        }

        self.token_data
            .as_ref()
            .filter(|t| !t.access_token.is_empty())
            .map(|t| t.access_token.clone())
            .ok_or_else(|| UploadError::AuthExpired("no Baidu Pan access token available".into()))
    }

    fn perform_authorization(&mut self) -> Result<()> {
        let auth_url = self.get_authorization_url()?;
        if !std::io::stdin().is_terminal() {
            return Err(UploadError::AuthExpired(format!(
                "Baidu Pan manual authorization required, but standard input is not an interactive terminal. Open {auth_url} to authorize interactively."
            )));
        }

        info!("Authorization required for first use or expired token");
        info!("1. Open the following link in your browser: {}", auth_url);
        info!("2. After login and authorization, you'll get an authorization code (code).");

        println!("3. Paste the authorization code here and press Enter: ");
        let mut code = String::new();
        std::io::stdin().read_line(&mut code)?;

        let code = code.trim();
        if code.is_empty() {
            return Err(UploadError::InvalidConfig(
                "Baidu Pan authorization code cannot be empty".to_string(),
            ));
        }
        self.get_access_token(code)?;

        let user_info = self.get_user_info()?;
        if user_info.errno == 0 {
            info!(
                "Authorization successful. Hello, {}!",
                user_info.baidu_name.unwrap_or_else(|| "User".to_string())
            );
            if let (Some(total), Some(used)) = (user_info.total, user_info.used) {
                info!("Total storage: {:.2} GB", total as f64 / (1024_f64.powi(3)));
                info!("Used storage: {:.2} GB", used as f64 / (1024_f64.powi(3)));
            }
            Ok(())
        } else {
            Err(classify_baidu_errno(
                user_info.errno,
                "failed to verify Baidu user info",
            ))
        }
    }

    fn get_user_info(&mut self) -> Result<UserInfo> {
        let access_token = self.get_valid_access_token()?;
        let url = build_url(
            &format!("{}/nas", self.endpoints.api_base),
            &[("method", "uinfo"), ("access_token", access_token.as_str())],
        )?;
        let resp = self
            .api_client
            .get(url)
            .send()
            .map_err(|e| UploadError::from_reqwest("Baidu user info request failed", e))?;

        let status = resp.status();
        let body = resp
            .text()
            .map_err(|e| UploadError::from_reqwest("read Baidu user info response", e))?;
        if !status.is_success() {
            return Err(UploadError::from_http_status(status.as_u16(), body));
        }

        serde_json::from_str(&body).map_err(|e| {
            UploadError::Protocol(format!(
                "decode Baidu user info: {e}; body: {}",
                redact_secret_params(&snippet(&body))
            ))
        })
    }

    fn get_upload_server(
        &self,
        access_token: &str,
        remote_path: &str,
        upload_id: &str,
    ) -> Result<String> {
        let url = build_url(
            &format!("{}/file", self.endpoints.pcs_base),
            &[
                ("method", "locateupload"),
                ("appid", "250528"),
                ("access_token", access_token),
                ("path", remote_path),
                ("uploadid", upload_id),
                ("upload_version", "2.0"),
            ],
        )?;
        let locate_result: LocateUploadResponse =
            self.call_api_with_retry("locateupload", || {
                let resp = self
                    .api_client
                    .get(url.clone())
                    .send()
                    .map_err(|e| UploadError::from_reqwest("Baidu locateupload failed", e))?;
                parse_json_status(resp, "Baidu locateupload")
            })?;

        if locate_result.error_code != 0 {
            let msg = locate_result
                .error_msg
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(classify_baidu_errno(locate_result.error_code, &msg));
        }

        let servers = locate_result.servers.ok_or_else(|| {
            UploadError::Protocol("no servers returned by Baidu locateupload".to_string())
        })?;

        let allow_http = self.endpoints.allow_loopback_http;
        let server = servers
            .into_iter()
            .find(|s| {
                s.server.starts_with("https://")
                    || (allow_http
                        && (s.server.starts_with("http://127.0.0.1")
                            || s.server.starts_with("http://localhost")))
            })
            .ok_or_else(|| {
                UploadError::Protocol(
                    "no HTTPS upload server found in Baidu locateupload response".to_string(),
                )
            })?;

        Ok(server.server)
    }

    /// Upload a file using caller-supplied path (computing only Baidu digests).
    pub fn upload(&mut self, file_path: impl AsRef<Path>, dest_path: &str) -> Result<()> {
        let local_path = file_path.as_ref();
        if self.dry_run {
            let file_name = local_path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| UploadError::InvalidConfig("invalid local file name".into()))?;
            let remote_full_path = join_remote_path(dest_path, file_name);
            info!(
                "[dry-run] Would upload {} to BaiduPan {}",
                local_path.display(),
                remote_full_path
            );
            return Ok(());
        }
        let digests = compute_file_digests(local_path, true, false)?;
        self.upload_with_digests(local_path, dest_path, &digests)
    }

    /// Upload a file using precomputed [`FileDigests`].
    pub fn upload_with_digests(
        &mut self,
        local_path: &Path,
        dest_path: &str,
        digests: &FileDigests,
    ) -> Result<()> {
        let file_name = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| UploadError::InvalidConfig("invalid local file name".to_string()))?;
        let remote_full_path = join_remote_path(dest_path, file_name);

        if self.dry_run {
            info!(
                "[dry-run] Would upload {} ({} bytes) to BaiduPan {}",
                local_path.display(),
                digests.file_size,
                remote_full_path
            );
            return Ok(());
        }

        if !local_path.is_file() {
            return Err(UploadError::InvalidConfig(format!(
                "local file not found or not a regular file: {}",
                local_path.display()
            )));
        }
        if digests.file_size > 0 && digests.baidu_block_md5s.is_empty() {
            return Err(UploadError::InvalidConfig(
                "FileDigests missing baidu_block_md5s for non-empty file".to_string(),
            ));
        }

        match self.upload_once(local_path, &remote_full_path, digests) {
            Ok(()) => Ok(()),
            Err(err) if err.is_auth_error() => {
                warn!(
                    "Baidu Pan API indicated auth expired ({err}); refreshing token and retrying once"
                );
                self.refresh_access_token()?;
                self.upload_once(local_path, &remote_full_path, digests)
            }
            Err(err) => Err(err),
        }
    }

    fn upload_once(
        &mut self,
        local_path: &Path,
        remote_full_path: &str,
        digests: &FileDigests,
    ) -> Result<()> {
        let access_token = self.get_valid_access_token()?;

        info!(
            "Starting Baidu Pan upload: {} ({} bytes) -> {}",
            local_path.display(),
            digests.file_size,
            remote_full_path
        );

        let block_list_json = serde_json::to_string(&digests.baidu_block_md5s).map_err(|e| {
            UploadError::Protocol(format!("failed to serialize Baidu block_list: {e}"))
        })?;
        let file_size_str = digests.file_size.to_string();

        // 1. Precreate
        debug!("Sending Baidu Pan precreate request...");
        let precreate_url = build_url(
            &format!("{}/file", self.endpoints.api_base),
            &[
                ("method", "precreate"),
                ("access_token", access_token.as_str()),
            ],
        )?;
        let precreate_result: PrecreateResponse = self.call_api_with_retry("precreate", || {
            let resp = self
                .api_client
                .post(precreate_url.clone())
                .form(&[
                    ("path", remote_full_path),
                    ("size", file_size_str.as_str()),
                    ("isdir", "0"),
                    ("autoinit", "1"),
                    ("block_list", block_list_json.as_str()),
                ])
                .send()
                .map_err(|e| UploadError::from_reqwest("Baidu precreate request failed", e))?;
            parse_json_status(resp, "Baidu precreate")
        })?;

        if precreate_result.errno != 0 {
            return Err(classify_baidu_errno(
                precreate_result.errno,
                "Baidu precreate failed",
            ));
        }

        // Rapid upload (return_type == 2): file already exists on Baidu Pan.
        if precreate_result.return_type == Some(2) {
            info!(
                "Baidu Pan rapid upload succeeded for {}; skipping chunk transfer and create",
                remote_full_path
            );
            return Ok(());
        }

        let upload_id = precreate_result.uploadid.ok_or_else(|| {
            UploadError::Protocol("Baidu precreate response missing uploadid".to_string())
        })?;
        info!("Baidu Pan precreate succeeded. Upload ID: {}", upload_id);

        let total_chunks = digests.baidu_block_md5s.len();
        let chunks_to_upload: Vec<usize> = match precreate_result.block_list {
            Some(list) if !list.is_empty() => {
                for &idx in &list {
                    if idx >= total_chunks {
                        return Err(UploadError::Protocol(format!(
                            "Baidu precreate returned out-of-range chunk index {idx} (total {total_chunks})"
                        )));
                    }
                }
                list
            }
            _ => (0..total_chunks).collect(),
        };

        // 2. Locate server & upload missing chunks concurrently
        if !chunks_to_upload.is_empty() {
            debug!("Locating Baidu Pan upload server...");
            let upload_server =
                self.get_upload_server(&access_token, remote_full_path, &upload_id)?;
            info!(
                "Uploading {} Baidu Pan chunk(s) via {} (concurrency={})",
                chunks_to_upload.len(),
                upload_server,
                self.max_concurrent_chunks
            );

            self.upload_chunks_parallel(
                local_path,
                digests.file_size,
                &chunks_to_upload,
                &upload_server,
                &access_token,
                remote_full_path,
                &upload_id,
            )?;
            info!("All requested Baidu Pan chunks uploaded successfully");
        }

        // 3. Create file
        debug!("Sending Baidu Pan create request...");
        let create_url = build_url(
            &format!("{}/file", self.endpoints.api_base),
            &[
                ("method", "create"),
                ("access_token", access_token.as_str()),
            ],
        )?;
        let create_result: CreateResponse = self.call_api_with_retry("create", || {
            let resp = self
                .api_client
                .post(create_url.clone())
                .form(&[
                    ("path", remote_full_path),
                    ("size", file_size_str.as_str()),
                    ("isdir", "0"),
                    ("uploadid", upload_id.as_str()),
                    ("block_list", block_list_json.as_str()),
                ])
                .send()
                .map_err(|e| UploadError::from_reqwest("Baidu create request failed", e))?;
            parse_json_status(resp, "Baidu create")
        })?;

        if create_result.errno == 0 {
            info!("Baidu Pan file uploaded successfully: {}", remote_full_path);
            Ok(())
        } else {
            Err(classify_baidu_errno(
                create_result.errno,
                "Baidu create file failed",
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_chunks_parallel(
        &self,
        local_path: &Path,
        file_size: u64,
        chunks_to_upload: &[usize],
        upload_server: &str,
        access_token: &str,
        remote_full_path: &str,
        upload_id: &str,
    ) -> Result<()> {
        let worker_count = self
            .max_concurrent_chunks
            .min(chunks_to_upload.len())
            .max(1);
        let next_pos = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let pcs_upload_url = format!(
            "{}/rest/2.0/pcs/superfile2",
            upload_server.trim_end_matches('/')
        );

        let worker_results: Vec<Result<()>> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let next_pos = &next_pos;
                let cancelled = &cancelled;
                let pcs_upload_url = &pcs_upload_url;
                let transfer_client = &self.transfer_client;

                handles.push(scope.spawn(move || -> Result<()> {
                    let mut file = File::open(local_path)?;

                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        let pos = next_pos.fetch_add(1, Ordering::Relaxed);
                        if pos >= chunks_to_upload.len() {
                            return Ok(());
                        }
                        let chunk_idx = chunks_to_upload[pos];
                        if let Err(err) = upload_single_chunk(
                            transfer_client,
                            &mut file,
                            file_size,
                            chunk_idx,
                            pcs_upload_url,
                            access_token,
                            remote_full_path,
                            upload_id,
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
                            "Baidu chunk upload worker panicked".to_string(),
                        ))
                    })
                })
                .collect()
        });

        for res in worker_results {
            res?;
        }
        Ok(())
    }

    fn call_api_with_retry<T>(&self, label: &str, mut op: impl FnMut() -> Result<T>) -> Result<T> {
        let mut attempt = 0u32;
        loop {
            match op() {
                Ok(val) => return Ok(val),
                Err(err) => {
                    if !err.is_retryable() || attempt >= API_MAX_RETRIES {
                        return Err(err);
                    }
                    let delay = backoff_with_jitter(attempt, API_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
                    warn!(
                        "Baidu Pan {label} hit retryable error (attempt {}/{}), retrying in {:?}: {}",
                        attempt + 1,
                        API_MAX_RETRIES,
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

#[allow(clippy::too_many_arguments)]
fn upload_single_chunk(
    transfer_client: &Client,
    file: &mut File,
    file_size: u64,
    chunk_idx: usize,
    pcs_upload_url: &str,
    access_token: &str,
    remote_full_path: &str,
    upload_id: &str,
    cancelled: &AtomicBool,
) -> Result<()> {
    let offset = (chunk_idx as u64) * (BAIDU_CHUNK_SIZE as u64);
    let len = (file_size - offset).min(BAIDU_CHUNK_SIZE as u64);
    let partseq_str = chunk_idx.to_string();
    let upload_url = build_url(
        pcs_upload_url,
        &[
            ("method", "upload"),
            ("access_token", access_token),
            ("type", "tmpfile"),
            ("path", remote_full_path),
            ("uploadid", upload_id),
            ("partseq", partseq_str.as_str()),
        ],
    )?;

    let mut attempt = 0u32;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(UploadError::Protocol(
                "Baidu chunk upload aborted due to sibling chunk failure".to_string(),
            ));
        }

        file.seek(SeekFrom::Start(offset))?;
        let chunk_reader = file.try_clone()?.take(len);
        let part = multipart::Part::reader_with_length(chunk_reader, len).file_name("file");
        let form = multipart::Form::new().part("file", part);

        let result = transfer_client
            .post(upload_url.clone())
            .multipart(form)
            .send();

        let err = match result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    info!("Baidu Pan chunk {} uploaded", chunk_idx + 1);
                    return Ok(());
                }
                let body = resp.text().unwrap_or_default();
                UploadError::from_http_status(status.as_u16(), body)
            }
            Err(e) => UploadError::from_reqwest(&format!("Baidu chunk {chunk_idx} upload"), e),
        };

        if err.is_retryable() && attempt < CHUNK_UPLOAD_MAX_RETRIES {
            let delay = backoff_with_jitter(attempt, CHUNK_UPLOAD_BACKOFF_BASE_MS, BACKOFF_MAX_MS);
            warn!(
                "Baidu Pan chunk {} failed (attempt {}/{}), retrying in {:?}: {}",
                chunk_idx + 1,
                attempt + 1,
                CHUNK_UPLOAD_MAX_RETRIES,
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

impl Uploader for BaiduPanUploader {
    fn name(&self) -> &str {
        "BaiduPan"
    }

    fn needs_baidu_digests(&self) -> bool {
        true
    }

    fn upload_with_digests(
        &mut self,
        file_path: &Path,
        dest_path: &str,
        digests: &FileDigests,
    ) -> Result<()> {
        BaiduPanUploader::upload_with_digests(self, file_path, dest_path, digests)
    }
}

fn build_url(base: &str, query: &[(&str, &str)]) -> Result<Url> {
    let mut url = Url::parse(base)?;
    {
        let mut pairs = url.query_pairs_mut();
        for (k, v) in query {
            pairs.append_pair(k, v);
        }
    }
    Ok(url)
}

fn join_remote_path(dest_path: &str, file_name: &str) -> String {
    let dir = dest_path.trim().trim_end_matches('/');
    if dir.is_empty() {
        format!("/{file_name}")
    } else {
        format!("{dir}/{file_name}")
    }
}

fn parse_json_status<T: serde::de::DeserializeOwned>(
    resp: reqwest::blocking::Response,
    label: &str,
) -> Result<T> {
    let status = resp.status();
    let body = resp
        .text()
        .map_err(|e| UploadError::from_reqwest(&format!("read {label} response"), e))?;
    if !status.is_success() {
        return Err(UploadError::from_http_status(status.as_u16(), body));
    }
    serde_json::from_str(&body).map_err(|e| {
        UploadError::Protocol(format!(
            "decode {label} response: {e}; body: {}",
            redact_secret_params(&snippet(&body))
        ))
    })
}

fn classify_oauth_error(error_code: &str, description: &str) -> UploadError {
    match error_code.trim() {
        "invalid_grant"
        | "expired_token"
        | "invalid_token"
        | "unauthorized_client"
        | "invalid_client" => UploadError::AuthExpired(format!("{error_code}: {description}")),
        "slow_down" | "rate_limit_exceeded" => {
            UploadError::RateLimited(format!("{error_code}: {description}"))
        }
        _ => UploadError::Api {
            code: error_code.to_string(),
            message: description.to_string(),
        },
    }
}

fn classify_baidu_errno(errno: i32, context: &str) -> UploadError {
    match errno {
        -6 | 110 | 111 => UploadError::AuthExpired(format!("{context}: errno {errno}")),
        31034 => UploadError::RateLimited(format!("{context}: errno {errno}")),
        _ => UploadError::Api {
            code: errno.to_string(),
            message: context.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(prefix: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("{prefix}_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        target: String,
        body: String,
    }

    fn spawn_mock_server(
        handler: impl Fn(&RecordedRequest) -> (u16, String) + Send + Sync + 'static,
    ) -> (
        String,
        Arc<Mutex<Vec<RecordedRequest>>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let handler = Arc::new(handler);

        // Shut down cleanly after handling a bounded number of connections or timeout.
        listener.set_nonblocking(false).unwrap();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    break;
                };
                let recorded = recorded_clone.clone();
                let handler = handler.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(&mut stream);
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let target = parts.next().unwrap_or("").to_string();
                    if target == "/__shutdown" {
                        return;
                    }

                    let mut content_length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        if line == "\r\n" || line == "\n" {
                            break;
                        }
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }

                    let mut body_bytes = vec![0u8; content_length];
                    if content_length > 0 {
                        let _ = reader.read_exact(&mut body_bytes);
                    }
                    let body = String::from_utf8_lossy(&body_bytes).to_string();
                    let req = RecordedRequest {
                        method,
                        target,
                        body,
                    };
                    let (status, resp_body) = handler(&req);
                    recorded.lock().unwrap().push(req);

                    let status_text = match status {
                        200 => "OK",
                        400 => "Bad Request",
                        401 => "Unauthorized",
                        403 => "Forbidden",
                        404 => "Not Found",
                        429 => "Too Many Requests",
                        503 => "Service Unavailable",
                        _ => "Status",
                    };
                    let response = format!(
                        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });

        (base_url, recorded, handle)
    }

    fn write_token_file(path: &Path, access_token: &str, refresh_token: &str) {
        let data = serde_json::json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "expires_at": (Utc::now() + Duration::hours(1)).to_rfc3339(),
        });
        write_private_atomic(path, data.to_string().as_bytes()).unwrap();
    }

    #[test]
    fn oauth_uses_post_form_and_redacts_secrets() {
        let dir = TestDir::new("baidu_oauth");
        let cfg_path = dir.0.join("baidu.json");

        let (base_url, recorded, _handle) = spawn_mock_server(|req| {
            if req.target.starts_with("/oauth/token") {
                return (
                    401,
                    r#"{"error":"invalid_grant","error_description":"bad code"}"#.to_string(),
                );
            }
            (404, "{}".to_string())
        });

        let endpoints = BaiduEndpoints {
            api_base: format!("{base_url}/xpan"),
            oauth_base: format!("{base_url}/oauth"),
            pcs_base: format!("{base_url}/pcs"),
            allow_loopback_http: true,
        };
        let mut uploader = BaiduPanUploader::build_client(
            "secret-app-key".into(),
            "super-secret-value".into(),
            Some(cfg_path),
            4,
            false,
            endpoints,
        )
        .unwrap();

        let debug_out = format!("{uploader:?}");
        assert!(!debug_out.contains("secret-app-key"));
        assert!(!debug_out.contains("super-secret-value"));
        assert!(debug_out.contains("[REDACTED]"));

        let err = uploader.get_access_token("auth-code-999").unwrap_err();
        assert!(err.is_auth_error());
        assert!(!err.to_string().contains("super-secret-value"));
        assert!(!err.to_string().contains("auth-code-999"));

        let reqs = recorded.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].target, "/oauth/token");
        assert!(reqs[0].body.contains("client_secret=super-secret-value"));
        assert!(reqs[0].body.contains("code=auth-code-999"));
    }

    #[test]
    fn rapid_upload_return_type_2_skips_chunks_and_create() {
        let dir = TestDir::new("baidu_rapid");
        let cfg_path = dir.0.join("baidu.json");
        write_token_file(&cfg_path, "tok-rapid", "ref-rapid");

        let file_path = dir.0.join("sample.bin");
        std::fs::write(&file_path, b"hello rapid upload").unwrap();

        let (base_url, recorded, _handle) = spawn_mock_server(|req| {
            if req.target.contains("method=precreate") {
                return (
                    200,
                    r#"{"errno":0,"return_type":2,"uploadid":"ignored"}"#.to_string(),
                );
            }
            panic!("unexpected request during rapid upload: {}", req.target);
        });

        let endpoints = BaiduEndpoints {
            api_base: format!("{base_url}/xpan"),
            oauth_base: format!("{base_url}/oauth"),
            pcs_base: format!("{base_url}/pcs"),
            allow_loopback_http: true,
        };
        let mut uploader = BaiduPanUploader::build_client(
            "k".into(),
            "s".into(),
            Some(cfg_path),
            4,
            false,
            endpoints,
        )
        .unwrap();
        uploader.load_tokens().unwrap();

        uploader.upload(&file_path, "/apps/backups").unwrap();

        let reqs = recorded.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].target.contains("method=precreate"));
    }

    #[test]
    fn partial_block_list_uploads_only_requested_chunks_in_parallel() {
        let dir = TestDir::new("baidu_partial");
        let cfg_path = dir.0.join("baidu.json");
        write_token_file(&cfg_path, "tok-part", "ref-part");

        // 3 chunks: 4 MiB + 4 MiB + 100 bytes
        let file_path = dir.0.join("multi.bin");
        let total_size = 2 * BAIDU_CHUNK_SIZE + 100;
        let payload = vec![0x5Au8; total_size];
        std::fs::write(&file_path, &payload).unwrap();

        let uploaded_parts2 = Arc::new(Mutex::new(Vec::<String>::new()));
        let uploaded_parts_srv2 = uploaded_parts2.clone();
        let retried_once = Arc::new(AtomicBool::new(false));
        let retried_once_srv = retried_once.clone();
        let base_for_locate = Arc::new(Mutex::new(String::new()));
        let base_for_locate_srv = base_for_locate.clone();

        let (base_url2, recorded2, _handle2) = spawn_mock_server(move |req| {
            if req.target.contains("method=precreate") {
                return (
                    200,
                    r#"{"errno":0,"return_type":1,"uploadid":"up-123","block_list":[0,2]}"#
                        .to_string(),
                );
            }
            if req.target.contains("method=locateupload") {
                let srv = base_for_locate_srv.lock().unwrap().clone();
                return (
                    200,
                    format!(r#"{{"error_code":0,"servers":[{{"server":"{srv}"}}]}}"#),
                );
            }
            if req.target.contains("method=upload") {
                if req.target.contains("partseq=2")
                    && !retried_once_srv.swap(true, Ordering::SeqCst)
                {
                    return (429, "rate limited".to_string());
                }
                uploaded_parts_srv2.lock().unwrap().push(req.target.clone());
                return (200, r#"{"md5":"ok"}"#.to_string());
            }
            if req.target.contains("method=create") {
                return (200, r#"{"errno":0,"fs_id":42}"#.to_string());
            }
            (404, "{}".to_string())
        });
        *base_for_locate.lock().unwrap() = base_url2.clone();

        let endpoints = BaiduEndpoints {
            api_base: format!("{base_url2}/xpan"),
            oauth_base: format!("{base_url2}/oauth"),
            pcs_base: format!("{base_url2}/pcs"),
            allow_loopback_http: true,
        };
        let mut uploader = BaiduPanUploader::build_client(
            "k".into(),
            "s".into(),
            Some(cfg_path),
            2,
            false,
            endpoints,
        )
        .unwrap();
        uploader.load_tokens().unwrap();

        uploader.upload(&file_path, "/apps/backups").unwrap();

        let parts = uploaded_parts2.lock().unwrap();
        let seqs: HashSet<&str> = parts
            .iter()
            .map(|t| {
                if t.contains("partseq=0") {
                    "0"
                } else if t.contains("partseq=1") {
                    "1"
                } else if t.contains("partseq=2") {
                    "2"
                } else {
                    "other"
                }
            })
            .collect();
        assert_eq!(seqs, HashSet::from(["0", "2"]));
        assert!(retried_once.load(Ordering::SeqCst));

        let all_reqs = recorded2.lock().unwrap();
        assert!(all_reqs.iter().any(|r| r.target.contains("method=create")));
    }

    #[test]
    fn non_retryable_chunk_status_403_fails_without_retry() {
        let dir = TestDir::new("baidu_403");
        let cfg_path = dir.0.join("baidu.json");
        write_token_file(&cfg_path, "tok-403", "ref-403");

        let file_path = dir.0.join("small.bin");
        std::fs::write(&file_path, b"payload").unwrap();

        let base_for_locate = Arc::new(Mutex::new(String::new()));
        let base_for_locate_srv = base_for_locate.clone();
        let upload_calls = Arc::new(AtomicUsize::new(0));
        let upload_calls_srv = upload_calls.clone();

        let (base_url, _recorded, _handle) = spawn_mock_server(move |req| {
            if req.target.contains("method=precreate") {
                return (
                    200,
                    r#"{"errno":0,"return_type":1,"uploadid":"up-403"}"#.to_string(),
                );
            }
            if req.target.contains("method=locateupload") {
                let srv = base_for_locate_srv.lock().unwrap().clone();
                return (
                    200,
                    format!(r#"{{"error_code":0,"servers":[{{"server":"{srv}"}}]}}"#),
                );
            }
            if req.target.contains("method=upload") {
                upload_calls_srv.fetch_add(1, Ordering::SeqCst);
                return (403, "forbidden".to_string());
            }
            (404, "{}".to_string())
        });
        *base_for_locate.lock().unwrap() = base_url.clone();

        let endpoints = BaiduEndpoints {
            api_base: format!("{base_url}/xpan"),
            oauth_base: format!("{base_url}/oauth"),
            pcs_base: format!("{base_url}/pcs"),
            allow_loopback_http: true,
        };
        let mut uploader = BaiduPanUploader::build_client(
            "k".into(),
            "s".into(),
            Some(cfg_path),
            4,
            false,
            endpoints,
        )
        .unwrap();
        uploader.load_tokens().unwrap();

        let err = uploader.upload(&file_path, "/apps/backups").unwrap_err();
        assert!(!err.is_retryable());
        assert_eq!(upload_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn baidu_auth_expired_refreshes_token_and_retries_once() {
        let dir = TestDir::new("baidu_refresh_retry");
        let cfg_path = dir.0.join("baidu.json");
        write_token_file(&cfg_path, "expired-tok", "valid-ref");

        let file_path = dir.0.join("sample.bin");
        std::fs::write(&file_path, b"refresh retry payload").unwrap();

        let (base_url, recorded, _handle) = spawn_mock_server(|req| {
            if req.target.starts_with("/oauth/token") {
                return (
                    200,
                    r#"{"access_token":"fresh-tok","refresh_token":"fresh-ref","expires_in":3600}"#
                        .to_string(),
                );
            }
            if req.target.contains("method=precreate") {
                if req.target.contains("access_token=expired-tok") {
                    return (200, r#"{"errno":-6}"#.to_string());
                }
                if req.target.contains("access_token=fresh-tok") {
                    return (200, r#"{"errno":0,"return_type":2}"#.to_string());
                }
            }
            (404, "{}".to_string())
        });

        let endpoints = BaiduEndpoints {
            api_base: format!("{base_url}/xpan"),
            oauth_base: format!("{base_url}/oauth"),
            pcs_base: format!("{base_url}/pcs"),
            allow_loopback_http: true,
        };
        let mut uploader = BaiduPanUploader::build_client(
            "k".into(),
            "s".into(),
            Some(cfg_path.clone()),
            4,
            false,
            endpoints,
        )
        .unwrap();
        uploader.load_tokens().unwrap();

        uploader.upload(&file_path, "/apps/backups").unwrap();

        let reqs = recorded.lock().unwrap();
        assert!(reqs.iter().any(|r| r.target == "/oauth/token"));
        let saved = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(saved.contains("fresh-tok"));
    }
}
