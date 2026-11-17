mod auth;
mod crypto;
mod folder;
mod transfer;
mod types;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;
use reqwest::cookie::Jar;
use tracing::{info, warn};

use self::auth::{default_config_path, load_config};
use self::types::{
    API_TIMEOUT, CONNECT_TIMEOUT, Cloud189Config, Cloud189Endpoints, TRANSFER_TIMEOUT,
    is_folder_missing_error,
};
use crate::config::{DEFAULT_MAX_CONCURRENT_CHUNKS, MAX_CONCURRENT_CHUNKS, MIN_CONCURRENT_CHUNKS};
use crate::coordinator::Uploader;
use crate::digest::{FileDigests, compute_file_digests};
use crate::error::{Result, UploadError};

pub struct Cloud189Uploader {
    client: Cloud189Client,
}

impl fmt::Debug for Cloud189Uploader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cloud189Uploader")
            .field("client", &self.client)
            .finish()
    }
}

struct Cloud189Client {
    api_client: Client,
    transfer_client: Client,
    jar: Arc<Jar>,
    config: Cloud189Config,
    folder_cache: HashMap<String, String>,
    session_verified_at: Option<Instant>,
    max_concurrent_chunks: usize,
    dry_run: bool,
    endpoints: Cloud189Endpoints,
}

impl fmt::Debug for Cloud189Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cloud189Client")
            .field("config", &self.config)
            .field("cached_folders", &self.folder_cache.len())
            .field("session_verified", &self.session_verified_at.is_some())
            .field("max_concurrent_chunks", &self.max_concurrent_chunks)
            .field("dry_run", &self.dry_run)
            .finish()
    }
}

impl Cloud189Uploader {
    /// Create a new Cloud189 uploader with default chunk concurrency (`4`).
    pub fn new(
        config_path: Option<PathBuf>,
        username: Option<String>,
        password: Option<String>,
        use_qr: bool,
    ) -> Result<Self> {
        Self::with_concurrency(
            config_path,
            username,
            password,
            use_qr,
            DEFAULT_MAX_CONCURRENT_CHUNKS,
        )
    }

    /// Create a new Cloud189 uploader with explicit bounded chunk concurrency (`1..=16`).
    pub fn with_concurrency(
        config_path: Option<PathBuf>,
        username: Option<String>,
        password: Option<String>,
        use_qr: bool,
        max_concurrent_chunks: usize,
    ) -> Result<Self> {
        let username = username
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let password = password.filter(|s| !s.trim().is_empty());
        if !use_qr && (username.is_some() ^ password.is_some()) {
            return Err(UploadError::InvalidConfig(
                "Cloud189 uploader enabled with password login, but username/password are incomplete"
                    .to_string(),
            ));
        }

        let config_path = match config_path.filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => path,
            None => default_config_path()?,
        };
        let mut config = load_config(&config_path)?;
        config.path = config_path;

        let mut api = Self::build_client(
            config,
            max_concurrent_chunks,
            false,
            Cloud189Endpoints::default(),
        )?;
        api.init_cookies_from_config();

        if !api.has_valid_session() {
            let refreshed = match api.try_refresh_session() {
                Ok(ok) => ok,
                Err(err) => {
                    warn!("Cloud189 refresh session failed, falling back to login: {err}");
                    false
                }
            };
            if !refreshed {
                if use_qr {
                    api.login_qr()?;
                } else if let (Some(user), Some(pass)) = (username.as_deref(), password.as_deref())
                {
                    api.login(user, pass)?;
                } else {
                    return Err(UploadError::AuthExpired(
                        "Cloud189 has no valid session; configure both username/password or enable QR login"
                            .to_string(),
                    ));
                }
            }
        }

        Ok(Self { client: api })
    }

    /// Construct a dry-run Cloud189 uploader without performing network I/O or login.
    pub fn new_dry_run(max_concurrent_chunks: usize) -> Result<Self> {
        let client = Self::build_client(
            Cloud189Config::default(),
            max_concurrent_chunks,
            true,
            Cloud189Endpoints::default(),
        )?;
        Ok(Self { client })
    }

    fn build_client(
        config: Cloud189Config,
        max_concurrent_chunks: usize,
        dry_run: bool,
        endpoints: Cloud189Endpoints,
    ) -> Result<Cloud189Client> {
        if !(MIN_CONCURRENT_CHUNKS..=MAX_CONCURRENT_CHUNKS).contains(&max_concurrent_chunks) {
            return Err(UploadError::InvalidConfig(format!(
                "max_concurrent_chunks must be in {MIN_CONCURRENT_CHUNKS}..={MAX_CONCURRENT_CHUNKS}, got {max_concurrent_chunks}"
            )));
        }

        let jar = Arc::new(Jar::default());
        let api_client = Client::builder()
            .cookie_provider(jar.clone())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(API_TIMEOUT)
            .build()
            .map_err(|e| UploadError::from_reqwest("create Cloud189 API HTTP client", e))?;

        let transfer_client = Client::builder()
            .cookie_provider(jar.clone())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TRANSFER_TIMEOUT)
            .build()
            .map_err(|e| UploadError::from_reqwest("create Cloud189 transfer HTTP client", e))?;

        Ok(Cloud189Client {
            api_client,
            transfer_client,
            jar,
            config,
            folder_cache: HashMap::new(),
            session_verified_at: None,
            max_concurrent_chunks,
            dry_run,
            endpoints,
        })
    }

    /// Upload a file using caller-supplied path (computing only Cloud189 digests).
    pub fn upload(&mut self, file_path: impl AsRef<Path>, dest_path: &str) -> Result<()> {
        let local_path = file_path.as_ref();
        if self.client.dry_run {
            let remote_dir = normalize_remote_dir(dest_path)?;
            info!(
                "[dry-run] Would upload {} to Cloud189 {}",
                local_path.display(),
                remote_dir
            );
            return Ok(());
        }
        let digests = compute_file_digests(local_path, false, true)?;
        self.upload_with_digests(local_path, dest_path, &digests)
    }

    /// Upload a file using precomputed [`FileDigests`].
    pub fn upload_with_digests(
        &mut self,
        local_path: &Path,
        dest_path: &str,
        digests: &FileDigests,
    ) -> Result<()> {
        let remote_dir = normalize_remote_dir(dest_path)?;
        if self.client.dry_run {
            info!(
                "[dry-run] Would upload {} ({} bytes) to Cloud189 {}",
                local_path.display(),
                digests.file_size,
                remote_dir
            );
            return Ok(());
        }

        self.client.ensure_session()?;
        let folder_id = self.client.resolve_folder(remote_dir, true)?;

        match self
            .client
            .upload_file_with_digests(local_path, &folder_id, digests)
        {
            Ok(()) => Ok(()),
            Err(err) if err.is_auth_error() => {
                warn!(
                    "Cloud189 upload hit auth error ({err}); refreshing session and retrying once"
                );
                self.client.force_refresh_session()?;
                let refreshed_folder_id = self.client.resolve_folder(remote_dir, true)?;
                self.client
                    .upload_file_with_digests(local_path, &refreshed_folder_id, digests)
            }
            Err(err) if is_folder_missing_error(&err) => {
                warn!(
                    "Cloud189 upload failed with missing folder ({err}); invalidating cache and retrying once"
                );
                self.client.invalidate_folder_cache(remote_dir);
                let fresh_folder_id = self.client.resolve_folder(remote_dir, true)?;
                self.client
                    .upload_file_with_digests(local_path, &fresh_folder_id, digests)
            }
            Err(err) => Err(err),
        }
    }
}

impl Uploader for Cloud189Uploader {
    fn name(&self) -> &str {
        "Cloud189"
    }

    fn needs_cloud189_digests(&self) -> bool {
        true
    }

    fn upload_with_digests(
        &mut self,
        file_path: &Path,
        dest_path: &str,
        digests: &FileDigests,
    ) -> Result<()> {
        Cloud189Uploader::upload_with_digests(self, file_path, dest_path, digests)
    }
}

fn normalize_remote_dir(dest_path: &str) -> Result<&str> {
    let remote_dir = if dest_path.trim().is_empty() {
        "/"
    } else {
        dest_path.trim()
    };
    if !remote_dir.starts_with('/') {
        return Err(UploadError::InvalidConfig(format!(
            "Cloud189 dest path must start with '/': {dest_path}"
        )));
    }
    Ok(remote_dir)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read as _, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::crypto::encode_param_plain;
    use super::folder::parse_list_files_text;
    use super::transfer::validate_upload_url;
    use super::types::{
        Cloud189Config, Cloud189Endpoints, Session, User, classify_api_error,
        is_retryable_commit_code, is_success_code,
    };
    use super::*;
    use crate::digest::CLOUD189_SLICE_SIZE;
    use crate::error::snippet;

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

    struct RecordedHttp {
        method: String,
        target: String,
        body: String,
    }

    fn spawn_mock_server(
        handler: impl Fn(&RecordedHttp) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<RecordedHttp>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let handler = Arc::new(handler);

        std::thread::spawn(move || {
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
                    let req = RecordedHttp {
                        method,
                        target,
                        body: String::from_utf8_lossy(&body_bytes).to_string(),
                    };
                    let (status, resp_body) = handler(&req);
                    recorded.lock().unwrap().push(req);

                    let response = format!(
                        "HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });

        (base_url, recorded)
    }

    fn test_config(path: PathBuf) -> Cloud189Config {
        let mut session = Session::default();
        session.login_name = "user@example.com".into();
        session.key = "session-key-secret".into();
        session.secret = "0123456789abcdef0123456789abcdef".into();
        session.access_token = "access-token-secret".into();
        session.refresh_token = "refresh-token-secret".into();

        Cloud189Config {
            user: Some(User {
                name: "user@example.com".into(),
                password: "secret-password".into(),
            }),
            session: Some(session),
            sson: Some("sson-secret".into()),
            auth: Some("auth-secret".into()),
            path,
        }
    }

    #[test]
    fn encode_param_plain_preserves_part_info_and_rejects_raw_ampersand() {
        let encoded_name = urlencoding::encode("a&b=c.tar.zst").into_owned();
        let params = vec![
            ("fileName".to_string(), encoded_name),
            ("partInfo".to_string(), "1-abc==,2-def+/".to_string()),
        ];
        let plain = encode_param_plain(&params).unwrap();
        assert_eq!(plain, "fileName=a%26b%3Dc.tar.zst&partInfo=1-abc==,2-def+/");

        let bad = vec![("fileName".to_string(), "bad&name.txt".to_string())];
        let err = encode_param_plain(&bad).unwrap_err();
        assert!(matches!(err, UploadError::Protocol(_)));

        let bad_key = vec![("a&b".to_string(), "v".to_string())];
        assert!(encode_param_plain(&bad_key).is_err());
    }

    #[test]
    fn https_check_rejects_non_https_upload_urls_in_production() {
        assert!(validate_upload_url("https://upload.example.com/part1", false).is_ok());
        let err = validate_upload_url("http://upload.example.com/part1", false).unwrap_err();
        assert!(matches!(err, UploadError::Protocol(_)));
        assert!(validate_upload_url("http://127.0.0.1:8080/part1", false).is_err());
        assert!(validate_upload_url("ftp://upload.example.com/part1", false).is_err());
    }

    #[test]
    fn debug_redacts_session_and_user_secrets() {
        let cfg = test_config(PathBuf::from("/tmp/c189.json"));
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("secret-password"));
        assert!(!dbg.contains("session-key-secret"));
        assert!(!dbg.contains("0123456789abcdef"));
        assert!(!dbg.contains("access-token-secret"));
        assert!(!dbg.contains("refresh-token-secret"));
        assert!(!dbg.contains("sson-secret"));
        assert!(!dbg.contains("auth-secret"));
        assert!(dbg.contains("[REDACTED]"));
    }

    #[test]
    fn folder_cache_reuses_ids_and_invalidates_on_missing_folder() {
        let dir = TestDir::new("c189_folder_cache");
        let cfg = test_config(dir.0.join("cfg.json"));
        let list_calls = Arc::new(AtomicUsize::new(0));
        let list_calls_srv = list_calls.clone();

        let (base_url, _) = spawn_mock_server(move |req| {
            if req.target.contains("/listFiles.action") {
                list_calls_srv.fetch_add(1, Ordering::SeqCst);
                if req.target.contains("folderId=-11") {
                    return (
                        200,
                        r#"{"res_code":0,"res_message":"ok","fileListAO":{"count":1,"fileListSize":100,"folderList":[{"id":"100","name":"apps"}]}}"#
                            .to_string(),
                    );
                }
                if req.target.contains("folderId=100") {
                    return (
                        200,
                        r#"{"res_code":0,"res_message":"ok","fileListAO":{"count":1,"fileListSize":100,"folderList":[{"id":"200","name":"backups"}]}}"#
                            .to_string(),
                    );
                }
            }
            (404, "{}".to_string())
        });

        let endpoints = Cloud189Endpoints {
            api_base: base_url.clone(),
            upload_base: base_url,
            allow_loopback_http: true,
            ..Default::default()
        };
        let mut client = Cloud189Uploader::build_client(cfg, 4, false, endpoints).unwrap();

        assert_eq!(
            client.resolve_folder("/apps/backups", false).unwrap(),
            "200"
        );
        assert_eq!(list_calls.load(Ordering::SeqCst), 2);

        // Second lookup hits cache with zero extra API calls.
        assert_eq!(
            client.resolve_folder("/apps/backups", false).unwrap(),
            "200"
        );
        assert_eq!(client.resolve_folder("/apps", false).unwrap(), "100");
        assert_eq!(list_calls.load(Ordering::SeqCst), 2);

        // Invalidate `/apps/backups` and re-resolve.
        client.invalidate_folder_cache("/apps/backups");
        assert_eq!(
            client.resolve_folder("/apps/backups", false).unwrap(),
            "200"
        );
        assert_eq!(list_calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn session_ttl_cooldown_skips_redundant_refresh_and_auth_expired_refreshes_immediately() {
        let dir = TestDir::new("c189_session_ttl");
        let cfg = test_config(dir.0.join("cfg.json"));
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls_srv = refresh_calls.clone();

        let (base_url, _) = spawn_mock_server(move |req| {
            if req.target.contains("/getSessionForPC.action") {
                refresh_calls_srv.fetch_add(1, Ordering::SeqCst);
                return (
                    200,
                    r#"{"loginName":"user","sessionKey":"k2","sessionSecret":"0123456789abcdef0123456789abcdef","accessToken":"a2","refreshToken":"r2"}"#
                        .to_string(),
                );
            }
            (404, "{}".to_string())
        });

        let endpoints = Cloud189Endpoints {
            api_base: base_url.clone(),
            upload_base: base_url,
            allow_loopback_http: true,
            ..Default::default()
        };
        let mut client = Cloud189Uploader::build_client(cfg, 4, false, endpoints).unwrap();

        // First call refreshes session and sets `session_verified_at`.
        client.ensure_session().unwrap();
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);

        // Second call within 10-minute TTL makes 0 HTTP requests.
        client.ensure_session().unwrap();
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);

        // Expired TTL (> 10 minutes) triggers another refresh.
        client.session_verified_at = Some(Instant::now() - Duration::from_secs(601));
        client.ensure_session().unwrap();
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 2);

        // Explicit `force_refresh_session` (on AuthExpired) bypasses cooldown immediately.
        client.force_refresh_session().unwrap();
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn parallel_part_upload_retries_only_408_429_5xx_and_never_404() {
        let dir = TestDir::new("c189_parts");
        let cfg = test_config(dir.0.join("cfg.json"));
        let file_path = dir.0.join("two_parts&test=1.bin");
        let data = vec![0x42u8; CLOUD189_SLICE_SIZE + 128];
        std::fs::write(&file_path, &data).unwrap();
        let digests = compute_file_digests(&file_path, false, true).unwrap();

        let put_part1_attempts = Arc::new(AtomicUsize::new(0));
        let put_part1_srv = put_part1_attempts.clone();
        let base_holder = Arc::new(Mutex::new(String::new()));
        let base_holder_srv = base_holder.clone();

        let (base_url, recorded) = spawn_mock_server(move |req| {
            let srv = base_holder_srv.lock().unwrap().clone();
            if req.target.contains("/person/initMultiUpload") {
                return (
                    200,
                    r#"{"code":"SUCCESS","data":{"uploadFileId":"uf-1","fileDataExists":0}}"#
                        .to_string(),
                );
            }
            if req.target.contains("/person/getMultiUploadUrls") {
                return (
                    200,
                    format!(
                        r#"{{"code":"SUCCESS","uploadUrls":{{"partNumber_1":{{"requestURL":"{srv}/put/1","requestHeader":"x-test=1"}},"partNumber_2":{{"requestURL":"{srv}/put/2","requestHeader":"x-test=2"}}}}}}"#
                    ),
                );
            }
            if req.method == "PUT" && req.target == "/put/1" {
                let n = put_part1_srv.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    return (503, "busy".to_string());
                }
                return (200, String::new());
            }
            if req.method == "PUT" && req.target == "/put/2" {
                return (200, String::new());
            }
            if req.target.contains("/person/commitMultiUploadFile") {
                return (200, r#"{"code":"SUCCESS"}"#.to_string());
            }
            (404, "not found".to_string())
        });
        *base_holder.lock().unwrap() = base_url.clone();

        let endpoints = Cloud189Endpoints {
            api_base: base_url.clone(),
            upload_base: base_url.clone(),
            allow_loopback_http: true,
            ..Default::default()
        };
        let mut client = Cloud189Uploader::build_client(cfg, 2, false, endpoints).unwrap();
        client.session_verified_at = Some(Instant::now());

        let mut uploader = Cloud189Uploader { client };
        uploader
            .upload_with_digests(&file_path, "/", &digests)
            .unwrap();
        assert_eq!(put_part1_attempts.load(Ordering::SeqCst), 2);

        {
            let reqs = recorded.lock().unwrap();
            let put1_ok: Vec<_> = reqs
                .iter()
                .filter(|r| r.method == "PUT" && r.target == "/put/1")
                .collect();
            assert_eq!(put1_ok.len(), 2);
            assert_eq!(put1_ok[1].body.len(), CLOUD189_SLICE_SIZE);
            let put2_ok: Vec<_> = reqs
                .iter()
                .filter(|r| r.method == "PUT" && r.target == "/put/2")
                .collect();
            assert_eq!(put2_ok.len(), 1);
            assert_eq!(put2_ok[0].body.len(), 128);

            // Verify initMultiUpload URL-encoded fileName and getMultiUploadUrls did NOT URL-encode partInfo.
            let secret = "0123456789abcdef0123456789abcdef";
            let init_req = reqs
                .iter()
                .find(|r| r.target.contains("/person/initMultiUpload"))
                .unwrap();
            let expected_init_plain = super::crypto::encode_param_plain(&[
                ("parentFolderId".to_string(), "-11".to_string()),
                (
                    "fileName".to_string(),
                    "two_parts%26test%3D1.bin".to_string(),
                ),
                ("fileSize".to_string(), digests.file_size.to_string()),
                ("sliceSize".to_string(), CLOUD189_SLICE_SIZE.to_string()),
                ("fileMd5".to_string(), digests.full_md5_hex.clone()),
                ("sliceMd5".to_string(), digests.cloud189_slice_md5.clone()),
                (
                    "extend".to_string(),
                    r#"{"opScene":"1","relativepath":"","rootfolderid":""}"#.to_string(),
                ),
            ])
            .unwrap();
            let expected_init_enc =
                super::crypto::aes_ecb_hex(expected_init_plain.as_bytes(), secret).unwrap();
            assert!(
                init_req
                    .target
                    .contains(&format!("params={expected_init_enc}"))
            );

            let urls_req = reqs
                .iter()
                .find(|r| r.target.contains("/person/getMultiUploadUrls"))
                .unwrap();
            let part_info = format!(
                "1-{},2-{}",
                digests.cloud189_parts[0].name, digests.cloud189_parts[1].name
            );
            let expected_urls_plain = super::crypto::encode_param_plain(&[
                ("partInfo".to_string(), part_info),
                ("uploadFileId".to_string(), "uf-1".to_string()),
            ])
            .unwrap();
            let expected_urls_enc =
                super::crypto::aes_ecb_hex(expected_urls_plain.as_bytes(), secret).unwrap();
            assert!(
                urls_req
                    .target
                    .contains(&format!("params={expected_urls_enc}"))
            );
        }

        // Now verify 404 on PUT is NOT retried.
        let put_404_calls = Arc::new(AtomicUsize::new(0));
        let put_404_srv = put_404_calls.clone();
        let base_holder2 = Arc::new(Mutex::new(String::new()));
        let base_holder2_srv = base_holder2.clone();
        let (base_url2, _) = spawn_mock_server(move |req| {
            let srv = base_holder2_srv.lock().unwrap().clone();
            if req.target.contains("/person/initMultiUpload") {
                return (
                    200,
                    r#"{"code":"SUCCESS","data":{"uploadFileId":"uf-2","fileDataExists":0}}"#
                        .to_string(),
                );
            }
            if req.target.contains("/person/getMultiUploadUrls") {
                return (
                    200,
                    format!(
                        r#"{{"code":"SUCCESS","uploadUrls":{{"partNumber_1":{{"requestURL":"{srv}/put/1","requestHeader":""}},"partNumber_2":{{"requestURL":"{srv}/put/2","requestHeader":""}}}}}}"#
                    ),
                );
            }
            if req.method == "PUT" {
                put_404_srv.fetch_add(1, Ordering::SeqCst);
                return (404, "no such upload".to_string());
            }
            (404, "{}".to_string())
        });
        *base_holder2.lock().unwrap() = base_url2.clone();
        drop(recorded);

        let endpoints2 = Cloud189Endpoints {
            api_base: base_url2.clone(),
            upload_base: base_url2,
            allow_loopback_http: true,
            ..Default::default()
        };
        let mut client2 = Cloud189Uploader::build_client(
            test_config(dir.0.join("cfg2.json")),
            1,
            false,
            endpoints2,
        )
        .unwrap();
        client2.session_verified_at = Some(Instant::now());
        let mut uploader2 = Cloud189Uploader { client: client2 };
        let err = uploader2
            .upload_with_digests(&file_path, "/", &digests)
            .unwrap_err();
        assert!(!err.is_retryable());
        assert_eq!(put_404_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn typed_error_classification_avoids_false_positives() {
        let auth_err = parse_list_files_text(
            r#"{"res_code":-6,"res_message":"InvalidSessionKey","fileListAO":{"count":0,"fileListSize":0,"folderList":[]}}"#,
        )
        .unwrap_err();
        assert!(auth_err.is_auth_error());

        let folder_err = parse_list_files_text(
            r#"{"res_code":12,"res_message":"parent folder not found","fileListAO":{"count":0,"fileListSize":0,"folderList":[]}}"#,
        )
        .unwrap_err();
        assert!(!folder_err.is_auth_error());
        assert!(is_folder_missing_error(&folder_err));

        let file_err = classify_api_error("InvalidFile", "file login_token.txt rejected");
        assert!(!file_err.is_auth_error());
        assert!(!file_err.is_retryable());

        assert!(classify_api_error("system busy", "busy").is_retryable());
        assert!(is_success_code("0"));
        assert!(is_success_code(" success "));
        assert!(is_retryable_commit_code(" Incomplete Upload "));
        assert_eq!(snippet("  hello  "), "hello");
    }

    #[test]
    fn upload_with_digests_invalidates_folder_cache_and_retries_on_missing_folder() {
        let dir = TestDir::new("c189_folder_retry");
        let cfg = test_config(dir.0.join("cfg.json"));
        let file_path = dir.0.join("item.bin");
        std::fs::write(&file_path, b"folder retry test").unwrap();
        let digests = compute_file_digests(&file_path, false, true).unwrap();

        let list_calls = Arc::new(AtomicUsize::new(0));
        let list_calls_srv = list_calls.clone();
        let init_calls = Arc::new(AtomicUsize::new(0));
        let init_calls_srv = init_calls.clone();

        let (base_url, _) = spawn_mock_server(move |req| {
            if req.target.contains("/listFiles.action") {
                list_calls_srv.fetch_add(1, Ordering::SeqCst);
                return (
                    200,
                    r#"{"res_code":0,"res_message":"ok","fileListAO":{"count":1,"fileListSize":100,"folderList":[{"id":"fresh-id","name":"backups"}]}}"#
                        .to_string(),
                );
            }
            if req.target.contains("/person/initMultiUpload") {
                let n = init_calls_srv.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    return (
                        200,
                        r#"{"code":"FolderNotFound","message":"folder does not exist"}"#
                            .to_string(),
                    );
                }
                return (
                    200,
                    r#"{"code":"SUCCESS","data":{"uploadFileId":"uf-ok","fileDataExists":1}}"#
                        .to_string(),
                );
            }
            if req.target.contains("/person/commitMultiUploadFile") {
                return (200, r#"{"code":"SUCCESS"}"#.to_string());
            }
            (404, "{}".to_string())
        });

        let endpoints = Cloud189Endpoints {
            api_base: base_url.clone(),
            upload_base: base_url,
            allow_loopback_http: true,
            ..Default::default()
        };
        let mut client = Cloud189Uploader::build_client(cfg, 2, false, endpoints).unwrap();
        client.session_verified_at = Some(Instant::now());
        // Pre-populate stale folder cache
        client
            .folder_cache
            .insert("/backups".to_string(), "stale-id".to_string());

        let mut uploader = Cloud189Uploader { client };
        uploader
            .upload_with_digests(&file_path, "/backups", &digests)
            .unwrap();

        assert_eq!(init_calls.load(Ordering::SeqCst), 2);
        assert_eq!(list_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            uploader
                .client
                .folder_cache
                .get("/backups")
                .map(String::as_str),
            Some("fresh-id")
        );
    }

    #[test]
    fn upload_with_digests_refreshes_session_on_auth_expired_and_retries() {
        let dir = TestDir::new("c189_auth_retry");
        let cfg = test_config(dir.0.join("cfg.json"));
        let file_path = dir.0.join("item.bin");
        std::fs::write(&file_path, b"auth retry test").unwrap();
        let digests = compute_file_digests(&file_path, false, true).unwrap();

        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls_srv = refresh_calls.clone();
        let init_calls = Arc::new(AtomicUsize::new(0));
        let init_calls_srv = init_calls.clone();

        let (base_url, _) = spawn_mock_server(move |req| {
            if req.target.contains("/getSessionForPC.action") {
                refresh_calls_srv.fetch_add(1, Ordering::SeqCst);
                return (
                    200,
                    r#"{"loginName":"user","sessionKey":"k2","sessionSecret":"0123456789abcdef0123456789abcdef","accessToken":"a2","refreshToken":"r2"}"#
                        .to_string(),
                );
            }
            if req.target.contains("/person/initMultiUpload") {
                let n = init_calls_srv.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    return (
                        200,
                        r#"{"code":"InvalidSessionKey","message":"expired"}"#.to_string(),
                    );
                }
                return (
                    200,
                    r#"{"code":"SUCCESS","data":{"uploadFileId":"uf-ok","fileDataExists":1}}"#
                        .to_string(),
                );
            }
            if req.target.contains("/person/commitMultiUploadFile") {
                return (200, r#"{"code":"SUCCESS"}"#.to_string());
            }
            (404, "{}".to_string())
        });

        let endpoints = Cloud189Endpoints {
            api_base: base_url.clone(),
            upload_base: base_url,
            allow_loopback_http: true,
            ..Default::default()
        };
        let mut client = Cloud189Uploader::build_client(cfg, 2, false, endpoints).unwrap();
        client.session_verified_at = Some(Instant::now());
        client
            .folder_cache
            .insert("/backups".to_string(), "101".to_string());

        let mut uploader = Cloud189Uploader { client };
        uploader
            .upload_with_digests(&file_path, "/backups", &digests)
            .unwrap();

        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(init_calls.load(Ordering::SeqCst), 2);
    }
}
