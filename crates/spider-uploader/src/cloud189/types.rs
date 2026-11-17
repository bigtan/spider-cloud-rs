use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroize;

use crate::error::{RedactedStr, UploadError, redacted_opt};

pub(super) const API_BASE: &str = "https://api.cloud.189.cn";
pub(super) const UPLOAD_BASE: &str = "https://upload.cloud.189.cn";
pub(super) const LOGIN_URL: &str = "https://cloud.189.cn/unifyLoginForPC.action";
pub(super) const OPEN_APP_CONF_URL: &str = "https://open.e.189.cn/api/logbox/oauth2/appConf.do";
pub(super) const OPEN_ENCRYPT_CONF_URL: &str =
    "https://open.e.189.cn/api/logbox/config/encryptConf.do";
pub(super) const OPEN_LOGIN_SUBMIT_URL: &str =
    "https://open.e.189.cn/api/logbox/oauth2/loginSubmit.do";
pub(super) const OPEN_OAUTH_BASE: &str = "https://open.e.189.cn";
pub(super) const APP_ID: &str = "9317140619";
pub(super) const ROOT_FOLDER_ID: &str = "-11";

pub(super) const API_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const SESSION_COOLDOWN: Duration = Duration::from_secs(600);

pub(super) const UPLOAD_PART_MAX_RETRIES: u32 = 3;
pub(super) const UPLOAD_PART_BACKOFF_BASE_MS: u64 = 800;
pub(super) const UPLOAD_REQ_MAX_RETRIES: u32 = 3;
pub(super) const UPLOAD_REQ_BACKOFF_BASE_MS: u64 = 800;
pub(super) const COMMIT_UPLOAD_MAX_RETRIES: u32 = 5;
pub(super) const COMMIT_UPLOAD_BACKOFF_BASE_MS: u64 = 1_000;
pub(super) const DIR_OP_MAX_RETRIES: u32 = 3;
pub(super) const DIR_OP_BACKOFF_BASE_MS: u64 = 1_000;
pub(super) const BACKOFF_MAX_MS: u64 = 30_000;
pub(super) const QR_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const QR_POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub(super) struct Cloud189Endpoints {
    pub(super) api_base: String,
    pub(super) upload_base: String,
    pub(super) login_url: String,
    pub(super) open_app_conf_url: String,
    pub(super) open_encrypt_conf_url: String,
    pub(super) open_login_submit_url: String,
    pub(super) open_oauth_base: String,
    pub(super) allow_loopback_http: bool,
}

impl Default for Cloud189Endpoints {
    fn default() -> Self {
        Self {
            api_base: API_BASE.to_string(),
            upload_base: UPLOAD_BASE.to_string(),
            login_url: LOGIN_URL.to_string(),
            open_app_conf_url: OPEN_APP_CONF_URL.to_string(),
            open_encrypt_conf_url: OPEN_ENCRYPT_CONF_URL.to_string(),
            open_login_submit_url: OPEN_LOGIN_SUBMIT_URL.to_string(),
            open_oauth_base: OPEN_OAUTH_BASE.to_string(),
            allow_loopback_http: false,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub(super) struct Cloud189Config {
    #[serde(default)]
    pub(super) user: Option<User>,
    #[serde(default)]
    pub(super) session: Option<Session>,
    #[serde(default)]
    pub(super) sson: Option<String>,
    #[serde(default)]
    pub(super) auth: Option<String>,
    #[serde(skip)]
    pub(super) path: PathBuf,
}

impl fmt::Debug for Cloud189Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cloud189Config")
            .field("user", &self.user)
            .field("session", &self.session)
            .field("sson", &redacted_opt(&self.sson))
            .field("auth", &redacted_opt(&self.auth))
            .field("path", &self.path)
            .finish()
    }
}

impl Drop for Cloud189Config {
    fn drop(&mut self) {
        if let Some(s) = self.sson.as_mut() {
            s.zeroize();
        }
        if let Some(a) = self.auth.as_mut() {
            a.zeroize();
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub(super) struct User {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) password: String,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("name", &self.name)
            .field("password", &RedactedStr(&self.password))
            .finish()
    }
}

impl Drop for User {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(super) struct Session {
    #[serde(default, rename = "loginName")]
    pub(super) login_name: String,
    #[serde(default, rename = "sessionKey")]
    pub(super) key: String,
    #[serde(default, rename = "sessionSecret")]
    pub(super) secret: String,
    #[serde(default, rename = "keepAlive")]
    pub(super) keep_alive: i64,
    #[serde(default, rename = "getFileDiffSpan")]
    pub(super) file_diff_span: i64,
    #[serde(default, rename = "getUserInfoSpan")]
    pub(super) user_info_span: i64,
    #[serde(default, rename = "familySessionKey")]
    pub(super) family_key: String,
    #[serde(default, rename = "familySessionSecret")]
    pub(super) family_secret: String,
    #[serde(default, rename = "accessToken")]
    pub(super) access_token: String,
    #[serde(default, rename = "refreshToken")]
    pub(super) refresh_token: String,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("login_name", &self.login_name)
            .field("key", &RedactedStr(&self.key))
            .field("secret", &RedactedStr(&self.secret))
            .field("keep_alive", &self.keep_alive)
            .field("file_diff_span", &self.file_diff_span)
            .field("user_info_span", &self.user_info_span)
            .field("family_key", &RedactedStr(&self.family_key))
            .field("family_secret", &RedactedStr(&self.family_secret))
            .field("access_token", &RedactedStr(&self.access_token))
            .field("refresh_token", &RedactedStr(&self.refresh_token))
            .finish()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.key.zeroize();
        self.secret.zeroize();
        self.family_key.zeroize();
        self.family_secret.zeroize();
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

impl Session {
    pub(super) fn is_valid(&self) -> bool {
        !self.key.is_empty() && !self.secret.is_empty()
    }
}

impl From<SessionResp> for Session {
    fn from(resp: SessionResp) -> Self {
        Self {
            login_name: resp.login_name,
            key: resp.key,
            secret: resp.secret,
            keep_alive: resp.keep_alive,
            file_diff_span: resp.file_diff_span,
            user_info_span: resp.user_info_span,
            family_key: resp.family_key,
            family_secret: resp.family_secret,
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct LoginSubmitResp {
    #[serde(rename = "result")]
    pub(super) result: i32,
    #[serde(rename = "msg")]
    pub(super) msg: String,
    #[serde(rename = "toUrl")]
    pub(super) to_url: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct AppConfResp {
    #[serde(rename = "data")]
    pub(super) data: AppConfData,
}

#[derive(Debug, Deserialize)]
pub(super) struct AppConfData {
    #[serde(rename = "accountType")]
    pub(super) account_type: String,
    #[serde(rename = "appKey")]
    pub(super) app_key: String,
    #[serde(rename = "clientType")]
    pub(super) client_type: i32,
    #[serde(rename = "isOauth2")]
    pub(super) is_oauth2: bool,
    #[serde(rename = "mailSuffix")]
    pub(super) mail_suffix: String,
    #[serde(rename = "paramId")]
    pub(super) param_id: String,
    #[serde(rename = "returnUrl")]
    pub(super) return_url: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct EncryptConfResp {
    #[serde(rename = "data")]
    pub(super) data: EncryptConfData,
}

#[derive(Debug, Deserialize)]
pub(super) struct EncryptConfData {
    #[serde(rename = "pre")]
    pub(super) pre: String,
    #[serde(rename = "pubKey")]
    pub(super) pub_key: String,
}

#[derive(Deserialize)]
pub(super) struct SessionResp {
    #[serde(rename = "loginName", default)]
    pub(super) login_name: String,
    #[serde(rename = "keepAlive", default)]
    pub(super) keep_alive: i64,
    #[serde(rename = "getFileDiffSpan", default)]
    pub(super) file_diff_span: i64,
    #[serde(rename = "getUserInfoSpan", default)]
    pub(super) user_info_span: i64,
    #[serde(rename = "sessionKey", default)]
    pub(super) key: String,
    #[serde(rename = "sessionSecret", default)]
    pub(super) secret: String,
    #[serde(rename = "familySessionKey", default)]
    pub(super) family_key: String,
    #[serde(rename = "familySessionSecret", default)]
    pub(super) family_secret: String,
    #[serde(rename = "accessToken", default)]
    pub(super) access_token: String,
    #[serde(rename = "refreshToken", default)]
    pub(super) refresh_token: String,
}

#[derive(Deserialize)]
pub(super) struct RefreshTokenResp {
    #[serde(rename = "accessToken", default)]
    pub(super) access_token: String,
    #[serde(rename = "refreshToken", default)]
    pub(super) refresh_token: String,
}

#[derive(Debug)]
pub(super) struct ApiError {
    pub(super) code: String,
    pub(super) message: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ListFilesResp {
    #[serde(rename = "res_code")]
    pub(super) res_code: i32,
    #[serde(rename = "res_message")]
    pub(super) res_message: String,
    #[serde(rename = "fileListAO")]
    pub(super) file_list: FileListAo,
}

#[derive(Debug, Deserialize)]
pub(super) struct FileListAo {
    #[serde(rename = "count")]
    pub(super) count: i32,
    #[serde(rename = "fileListSize")]
    pub(super) file_list_size: i32,
    #[serde(rename = "folderList", default)]
    pub(super) folders: Vec<Folder>,
}

#[derive(Debug, Deserialize, Clone)]
pub(super) struct Folder {
    #[serde(deserialize_with = "de_id")]
    pub(super) id: String,
    #[serde(rename = "name")]
    pub(super) name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct XmlListFiles {
    #[serde(rename = "fileList")]
    pub(super) file_list: XmlFileList,
}

#[derive(Debug, Deserialize)]
pub(super) struct XmlFileList {
    #[serde(rename = "count")]
    pub(super) count: Option<i32>,
    #[serde(rename = "fileListSize")]
    pub(super) file_list_size: Option<i32>,
    #[serde(rename = "folder", default)]
    pub(super) folders: Vec<XmlFolder>,
}

#[derive(Debug, Deserialize)]
pub(super) struct XmlFolder {
    #[serde(rename = "id")]
    pub(super) id: String,
    #[serde(rename = "name")]
    pub(super) name: String,
}

#[derive(Debug)]
pub(super) struct ListFilesResult {
    pub(super) folders: Vec<Folder>,
    pub(super) count: Option<i32>,
    pub(super) page_size: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MkdirResp {
    #[serde(rename = "res_code")]
    pub(super) res_code: i32,
    #[serde(rename = "res_message")]
    pub(super) res_message: String,
    #[serde(deserialize_with = "de_id")]
    pub(super) id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct XmlMkdirResp {
    #[serde(rename = "res_code")]
    pub(super) res_code: Option<i32>,
    #[serde(rename = "res_message")]
    pub(super) res_message: Option<String>,
    #[serde(rename = "id")]
    pub(super) id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct InitUploadResp {
    #[serde(rename = "code")]
    pub(super) code: String,
    #[serde(rename = "data")]
    pub(super) data: InitUploadData,
}

#[derive(Debug, Deserialize)]
pub(super) struct InitUploadData {
    #[serde(rename = "uploadFileId")]
    pub(super) upload_file_id: String,
    #[serde(rename = "fileDataExists")]
    pub(super) file_data_exists: i32,
}

#[derive(Debug, Deserialize)]
pub(super) struct UploadUrlsResp {
    #[serde(rename = "code")]
    pub(super) code: String,
    #[serde(rename = "uploadUrls")]
    pub(super) upload_urls: std::collections::HashMap<String, UploadUrl>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UploadUrl {
    #[serde(rename = "requestURL")]
    pub(super) request_url: String,
    #[serde(rename = "requestHeader")]
    pub(super) request_header: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct CommitUploadResp {
    #[serde(rename = "code")]
    pub(super) code: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct QrCodeResp {
    #[serde(rename = "uuid")]
    pub(super) uuid: String,
    #[serde(rename = "encryuuid")]
    pub(super) encryuuid: String,
    #[serde(rename = "encodeuuid")]
    pub(super) encodeuuid: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct QrCodeState {
    #[serde(rename = "redirectUrl", default)]
    pub(super) redirect_url: String,
    #[serde(rename = "status")]
    pub(super) status: i32,
    #[serde(skip)]
    pub(super) sson: Option<String>,
}

pub(super) fn de_id<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Err(Error::custom("invalid id")),
    }
}

pub(super) fn is_success_code(code: &str) -> bool {
    let c = code.trim();
    c == "0" || c.eq_ignore_ascii_case("success")
}

pub(super) fn is_retryable_commit_code(code: &str) -> bool {
    code.trim().eq_ignore_ascii_case("incomplete upload")
}

pub(super) fn classify_api_error(code: &str, message: &str) -> UploadError {
    let c = code.trim().to_ascii_lowercase();
    let m = message.trim().to_ascii_lowercase();

    if matches!(
        c.as_str(),
        "-6" | "401"
            | "userinvalidopentoken"
            | "invalidsessionkey"
            | "invalidtoken"
            | "invalid_token"
            | "invalidsignature"
            | "signatureerror"
            | "pleaselogin"
    ) || matches!(
        m.as_str(),
        "invalidsessionkey" | "userinvalidopentoken" | "invalid session key" | "please login again"
    ) {
        return UploadError::AuthExpired(format!("{code}: {message}"));
    }

    if matches!(c.as_str(), "429" | "toomanyrequests") || m.contains("too many requests") {
        return UploadError::RateLimited(format!("{code}: {message}"));
    }

    if matches!(
        c.as_str(),
        "500"
            | "502"
            | "503"
            | "504"
            | "408"
            | "system busy"
            | "server busy"
            | "service unavailable"
            | "internal server error"
            | "bad gateway"
            | "gateway timeout"
            | "incomplete upload"
    ) || m.contains("system busy")
        || m.contains("server busy")
        || m.contains("service unavailable")
    {
        return UploadError::TransientServer {
            status: 503,
            body: format!("{code}: {message}"),
        };
    }

    UploadError::Api {
        code: code.to_string(),
        message: message.to_string(),
    }
}

pub(super) fn classify_res_code(res_code: i32, message: &str) -> UploadError {
    classify_api_error(&res_code.to_string(), message)
}

pub(super) fn is_folder_missing_error(err: &UploadError) -> bool {
    match err {
        UploadError::Api { code, message } => {
            let c = code.trim().to_ascii_lowercase();
            let m = message.trim().to_ascii_lowercase();
            matches!(
                c.as_str(),
                "12" | "filenotfound"
                    | "foldernotfound"
                    | "parentfoldernotfound"
                    | "foldernotexist"
                    | "parentnotexist"
            ) || m.contains("folder not found")
                || m.contains("parent folder not found")
                || m.contains("does not exist")
                || m.contains("not exist")
        }
        _ => false,
    }
}
