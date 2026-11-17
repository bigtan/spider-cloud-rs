use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Instant;

use chrono::Local;
use reqwest::blocking::Response;
use serde::de::DeserializeOwned;
use spider_common::fs::write_private_atomic;
use tracing::{info, warn};
use url::Url;
use urlencoding::decode as url_decode;

use super::Cloud189Client;
use super::crypto::{
    chrono_millis, encode_form_urlencoded, encrypt_credentials, with_client_params, with_query,
};
use super::types::{
    APP_ID, ApiError, AppConfData, AppConfResp, Cloud189Config, EncryptConfData, EncryptConfResp,
    LoginSubmitResp, QR_LOGIN_TIMEOUT, QR_POLL_INTERVAL, QrCodeResp, QrCodeState, RefreshTokenResp,
    SESSION_COOLDOWN, Session, SessionResp, User, classify_api_error,
};
use crate::error::{Result, UploadError, redact_secret_params, snippet};

pub(super) fn default_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| UploadError::InvalidConfig("cannot determine home directory".to_string()))?;
    Ok(home
        .join(".config")
        .join("spider-cloud")
        .join("cloud189")
        .join("config.json"))
}

pub(super) fn load_config(path: &Path) -> Result<Cloud189Config> {
    if !path.exists() {
        let mut cfg = Cloud189Config::default();
        cfg.path = path.to_path_buf();
        return Ok(cfg);
    }
    let data = fs::read(path)?;
    let mut cfg: Cloud189Config = match serde_json::from_slice(&data) {
        Ok(cfg) => cfg,
        Err(err) => {
            warn!(
                "Cloud189 config file {} is invalid, starting with an empty config: {err}",
                path.display()
            );
            Cloud189Config::default()
        }
    };
    cfg.path = path.to_path_buf();
    Ok(cfg)
}

pub(super) fn save_config(config: &Cloud189Config) -> Result<()> {
    let data = serde_json::to_vec_pretty(config)
        .map_err(|e| UploadError::Protocol(format!("serialize Cloud189 config: {e}")))?;
    write_private_atomic(&config.path, &data)?;
    Ok(())
}

impl Cloud189Client {
    pub(super) fn init_cookies_from_config(&self) {
        if let Some(sson) = self.config.sson.as_ref()
            && let Ok(url) = Url::parse("https://e.189.cn")
        {
            let cookie = format!("SSON={sson}");
            self.jar.add_cookie_str(&cookie, &url);
        }
        if let Some(auth) = self.config.auth.as_ref() {
            let cookie = format!("COOKIE_LOGIN_USER={auth}");
            if let Ok(url_cloud) = Url::parse("https://cloud.189.cn") {
                self.jar.add_cookie_str(&cookie, &url_cloud);
            }
            if let Ok(url_mobile) = Url::parse("https://m.cloud.189.cn") {
                self.jar.add_cookie_str(&cookie, &url_mobile);
            }
        }
    }

    pub(super) fn has_valid_session(&self) -> bool {
        self.config.session.as_ref().is_some_and(Session::is_valid)
    }

    pub(super) fn ensure_session(&mut self) -> Result<()> {
        if self.has_valid_session()
            && self
                .session_verified_at
                .is_some_and(|verified| verified.elapsed() < SESSION_COOLDOWN)
        {
            return Ok(());
        }

        match self.try_refresh_session() {
            Ok(true) => {
                self.session_verified_at = Some(Instant::now());
                Ok(())
            }
            Ok(false) => {
                if self.has_valid_session() {
                    self.session_verified_at = Some(Instant::now());
                    Ok(())
                } else {
                    Err(UploadError::AuthExpired(
                        "missing valid Cloud189 session, please login again".to_string(),
                    ))
                }
            }
            Err(err) => {
                if err.is_auth_error() {
                    self.session_verified_at = None;
                    Err(err)
                } else if self.has_valid_session() {
                    warn!(
                        "Cloud189 refresh session failed transiently, continuing with existing session: {err}"
                    );
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }

    pub(super) fn force_refresh_session(&mut self) -> Result<()> {
        self.session_verified_at = None;
        match self.try_refresh_session() {
            Ok(true) => {
                self.session_verified_at = Some(Instant::now());
                Ok(())
            }
            Ok(false) => Err(UploadError::AuthExpired(
                "Cloud189 session expired and cannot be refreshed".to_string(),
            )),
            Err(err) => Err(err),
        }
    }

    pub(super) fn try_refresh_session(&mut self) -> Result<bool> {
        let Some(session) = self.config.session.clone() else {
            return Ok(false);
        };

        if !session.access_token.is_empty() {
            match self.refresh_session(&session.access_token) {
                Ok(updated) => {
                    self.update_session(updated);
                    save_config(&self.config)?;
                    self.session_verified_at = Some(Instant::now());
                    return Ok(true);
                }
                Err(err) => {
                    if !err.is_auth_error() {
                        return Err(err);
                    }
                }
            }
        }

        if session.refresh_token.is_empty() {
            return Ok(false);
        }

        let refreshed = self.refresh_token(&session.refresh_token)?;
        self.update_session(session_resp_from_tokens(
            refreshed.access_token,
            refreshed.refresh_token,
        ));
        save_config(&self.config)?;

        let access = self
            .config
            .session
            .as_ref()
            .map(|s| s.access_token.clone())
            .unwrap_or_default();
        if access.is_empty() {
            return Ok(false);
        }
        let updated = self.refresh_session(&access)?;
        self.update_session(updated);
        save_config(&self.config)?;
        self.session_verified_at = Some(Instant::now());
        Ok(true)
    }

    pub(super) fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let params = vec![
            ("appId".to_string(), APP_ID.to_string()),
            ("clientType".to_string(), "10020".to_string()),
            ("timeStamp".to_string(), format!("{}", chrono_millis())),
            (
                "returnURL".to_string(),
                "https://m.cloud.189.cn/zhuanti/2020/loginErrorPc/index.html".to_string(),
            ),
        ];
        let login_url = with_query(&self.endpoints.login_url, &params)?;
        let resp = self
            .api_client
            .get(login_url)
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 request login url", e))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            return Err(UploadError::from_http_status(status, body));
        }

        let referer = resp.url().to_string();
        let url = resp.url().clone();
        let lt = query_param(&url, "lt");
        let req_id = query_param(&url, "reqId");
        let app_key = query_param(&url, "appId");

        let app_conf = self.fetch_app_conf(&referer, &req_id, &lt, &app_key)?;
        let encrypt_conf = self.fetch_encrypt_conf(&referer)?;
        let (enc_user, enc_pass) =
            encrypt_credentials(&encrypt_conf.pub_key, &encrypt_conf.pre, username, password)?;

        let submit_params = vec![
            ("version".to_string(), "v2.0".to_string()),
            ("appKey".to_string(), app_conf.app_key),
            ("accountType".to_string(), app_conf.account_type),
            ("userName".to_string(), enc_user),
            ("epd".to_string(), enc_pass),
            ("captchaType".to_string(), String::new()),
            ("validateCode".to_string(), String::new()),
            ("smsValidateCode".to_string(), String::new()),
            ("captchaToken".to_string(), String::new()),
            ("returnUrl".to_string(), referer.clone()),
            ("mailSuffix".to_string(), app_conf.mail_suffix),
            ("dynamicCheck".to_string(), "FALSE".to_string()),
            ("clientType".to_string(), app_conf.client_type.to_string()),
            ("cb_SaveName".to_string(), "0".to_string()),
            ("isOauth2".to_string(), app_conf.is_oauth2.to_string()),
            ("state".to_string(), String::new()),
            ("paramId".to_string(), app_conf.param_id),
        ];

        let resp = self
            .api_client
            .post(&self.endpoints.open_login_submit_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::REFERER, &referer)
            .header("Reqid", &req_id)
            .header("lt", &lt)
            .body(encode_form_urlencoded(&submit_params))
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 login submit", e))?;

        let sson = extract_cookie(&resp, "SSON");
        let submit: LoginSubmitResp = parse_json_response(resp, "login submit")?;
        if submit.result != 0 {
            return Err(UploadError::AuthExpired(format!(
                "Cloud189 login failed: {}",
                submit.msg
            )));
        }
        self.config.sson = sson;

        let session = self.fetch_session(&submit.to_url)?;
        self.config.session = Some(session.into());
        self.config.user = Some(User {
            name: username.to_string(),
            password: String::new(),
        });
        self.session_verified_at = Some(Instant::now());

        save_config(&self.config)?;
        Ok(())
    }

    pub(super) fn login_qr(&mut self) -> Result<()> {
        let params = vec![
            ("appId".to_string(), APP_ID.to_string()),
            ("clientType".to_string(), "10020".to_string()),
            ("timeStamp".to_string(), format!("{}", chrono_millis())),
            (
                "returnURL".to_string(),
                "https://m.cloud.189.cn/zhuanti/2020/loginErrorPc/index.html".to_string(),
            ),
        ];
        let login_url = with_query(&self.endpoints.login_url, &params)?;
        let resp = self
            .api_client
            .get(login_url)
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 request QR login url", e))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            return Err(UploadError::from_http_status(status, body));
        }

        let referer = resp.url().to_string();
        let url = resp.url().clone();
        let lt = query_param(&url, "lt");
        let req_id = query_param(&url, "reqId");
        let app_key = query_param(&url, "appId");

        let app_conf = self.fetch_app_conf(&referer, &req_id, &lt, &app_key)?;

        let uuid_url = with_query(
            &format!(
                "{}/api/logbox/oauth2/getUUID.do",
                self.endpoints.open_oauth_base
            ),
            &[("appId".to_string(), app_key)],
        )?;
        let resp = self
            .api_client
            .get(uuid_url)
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 get QR uuid", e))?;
        let qr: QrCodeResp = parse_json_response(resp, "get qr uuid")?;

        let decoded_uuid = url_decode(&qr.encodeuuid)
            .map_err(|e| UploadError::Protocol(format!("decode qr uuid: {e}")))?
            .into_owned();
        let mut qr_params = url::form_urlencoded::Serializer::new(String::new());
        qr_params.append_pair("REQID", &req_id);
        qr_params.append_pair("uuid", &decoded_uuid);
        let qr_url = format!(
            "{}/api/logbox/oauth2/image.do?{}",
            self.endpoints.open_oauth_base,
            qr_params.finish()
        );

        info!(
            "Please open the QR code URL in a browser and scan to sign in:\n{}\n",
            qr_url
        );

        let deadline = Instant::now() + QR_LOGIN_TIMEOUT;
        loop {
            let state = self.query_qr_state(&qr, &app_conf, &referer)?;
            match state.status {
                -106 => {
                    info!("QR code not scanned yet, waiting...");
                }
                -11002 => {
                    info!("QR code scanned but not confirmed, waiting...");
                }
                0 => {
                    if let Some(sson) = state.sson.clone() {
                        self.config.sson = Some(sson);
                    }
                    let session = self.fetch_session(&state.redirect_url)?;
                    self.config.session = Some(session.into());
                    self.session_verified_at = Some(Instant::now());
                    save_config(&self.config)?;
                    info!("Cloud189 QR code login succeeded");
                    return Ok(());
                }
                _ => {
                    warn!("QR code login status unexpected: {}", state.status);
                    return Err(UploadError::AuthExpired(format!(
                        "unexpected Cloud189 QR login status: {}",
                        state.status
                    )));
                }
            }
            if Instant::now() >= deadline {
                return Err(UploadError::AuthExpired(format!(
                    "Cloud189 QR code login timed out after {}s without confirmation",
                    QR_LOGIN_TIMEOUT.as_secs()
                )));
            }
            sleep(QR_POLL_INTERVAL);
        }
    }

    fn fetch_app_conf(
        &self,
        referer: &str,
        req_id: &str,
        lt: &str,
        app_key: &str,
    ) -> Result<AppConfData> {
        let params = vec![
            ("version".to_string(), "2.0".to_string()),
            ("appKey".to_string(), app_key.to_string()),
        ];
        let resp = self
            .api_client
            .post(&self.endpoints.open_app_conf_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ORIGIN, &self.endpoints.open_oauth_base)
            .header(reqwest::header::REFERER, referer)
            .header("Reqid", req_id)
            .header("lt", lt)
            .body(encode_form_urlencoded(&params))
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 get app conf", e))?;
        let data: AppConfResp = parse_json_response(resp, "app conf")?;
        Ok(data.data)
    }

    fn fetch_encrypt_conf(&self, referer: &str) -> Result<EncryptConfData> {
        let params = vec![("appId".to_string(), "cloud".to_string())];
        let resp = self
            .api_client
            .post(&self.endpoints.open_encrypt_conf_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::REFERER, referer)
            .body(encode_form_urlencoded(&params))
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 get encrypt conf", e))?;
        let data: EncryptConfResp = parse_json_response(resp, "encrypt conf")?;
        Ok(data.data)
    }

    fn query_qr_state(
        &self,
        qr: &QrCodeResp,
        app_conf: &AppConfData,
        referer: &str,
    ) -> Result<QrCodeState> {
        let mut url = Url::parse(&format!(
            "{}/api/logbox/oauth2/qrcodeLoginState.do",
            self.endpoints.open_oauth_base
        ))?;
        let timestamp = chrono_millis();
        let date = Local::now().format("%Y-%m-%d%H:%M:%S9").to_string();
        url.query_pairs_mut()
            .append_pair("appId", &app_conf.app_key)
            .append_pair("encryuuid", &qr.encryuuid)
            .append_pair("date", &date)
            .append_pair("uuid", &qr.uuid)
            .append_pair("returnUrl", &app_conf.return_url)
            .append_pair("clientType", &app_conf.client_type.to_string())
            .append_pair("timeStamp", &timestamp.to_string())
            .append_pair("cb_SaveName", "0")
            .append_pair("isOauth2", &app_conf.is_oauth2.to_string())
            .append_pair("state", "")
            .append_pair("paramId", &app_conf.param_id);

        let resp = self
            .api_client
            .get(url)
            .header(reqwest::header::REFERER, referer)
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 query QR state", e))?;
        let sson = extract_cookie(&resp, "SSON");
        let mut state: QrCodeState = parse_json_response(resp, "query qr state")?;
        if state.status == 0 {
            state.sson = sson;
        }
        Ok(state)
    }

    fn fetch_session(&self, redirect_url: &str) -> Result<SessionResp> {
        let params = vec![("redirectURL".to_string(), redirect_url.to_string())];
        let url = with_client_params(Url::parse(&format!(
            "{}/getSessionForPC.action",
            self.endpoints.api_base
        ))?);
        let resp = self
            .api_client
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json;charset=UTF-8")
            .body(encode_form_urlencoded(&params))
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 get session", e))?;
        parse_json_with_error(resp, "get session")
    }

    fn refresh_session(&self, access_token: &str) -> Result<SessionResp> {
        let mut url = with_client_params(Url::parse(&format!(
            "{}/getSessionForPC.action",
            self.endpoints.api_base
        ))?);
        url.query_pairs_mut()
            .append_pair("appId", APP_ID)
            .append_pair("accessToken", access_token);
        let resp = self
            .api_client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json;charset=UTF-8")
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 refresh session", e))?;
        parse_json_with_error(resp, "refresh session")
    }

    fn refresh_token(&self, refresh_token: &str) -> Result<RefreshTokenResp> {
        let resp = self
            .api_client
            .post(format!(
                "{}/api/oauth2/refreshToken.do",
                self.endpoints.open_oauth_base
            ))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json;charset=UTF-8")
            .body(encode_form_urlencoded(&[
                ("clientId".to_string(), APP_ID.to_string()),
                ("refreshToken".to_string(), refresh_token.to_string()),
                ("grantType".to_string(), "refresh_token".to_string()),
                ("format".to_string(), "json".to_string()),
            ]))
            .send()
            .map_err(|e| UploadError::from_reqwest("Cloud189 refresh token", e))?;
        parse_json_with_error(resp, "refresh token")
    }

    fn update_session(&mut self, session: SessionResp) {
        fn pick(new: String, old: String) -> String {
            if new.is_empty() { old } else { new }
        }
        fn pick_span(new: i64, old: i64) -> i64 {
            if new == 0 { old } else { new }
        }

        let mut current = self.config.session.take().unwrap_or_default();
        let merged = Session {
            login_name: pick(session.login_name, std::mem::take(&mut current.login_name)),
            key: pick(session.key, std::mem::take(&mut current.key)),
            secret: pick(session.secret, std::mem::take(&mut current.secret)),
            keep_alive: pick_span(session.keep_alive, current.keep_alive),
            file_diff_span: pick_span(session.file_diff_span, current.file_diff_span),
            user_info_span: pick_span(session.user_info_span, current.user_info_span),
            family_key: pick(session.family_key, std::mem::take(&mut current.family_key)),
            family_secret: pick(
                session.family_secret,
                std::mem::take(&mut current.family_secret),
            ),
            access_token: pick(
                session.access_token,
                std::mem::take(&mut current.access_token),
            ),
            refresh_token: pick(
                session.refresh_token,
                std::mem::take(&mut current.refresh_token),
            ),
        };
        self.config.session = Some(merged);
    }
}

fn query_param(url: &Url, key: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

fn extract_cookie(resp: &Response, name: &str) -> Option<String> {
    resp.cookies()
        .find(|c| c.name() == name)
        .map(|c| c.value().to_string())
}

fn session_resp_from_tokens(access_token: String, refresh_token: String) -> SessionResp {
    SessionResp {
        login_name: String::new(),
        keep_alive: 0,
        file_diff_span: 0,
        user_info_span: 0,
        key: String::new(),
        secret: String::new(),
        family_key: String::new(),
        family_secret: String::new(),
        access_token,
        refresh_token,
    }
}

fn parse_json_response<T: DeserializeOwned>(resp: Response, label: &str) -> Result<T> {
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| UploadError::from_reqwest(&format!("read {label} body"), e))?;
    if !status.is_success() {
        return Err(UploadError::from_http_status(status.as_u16(), text));
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        UploadError::Protocol(format!(
            "decode {label} json (status {status}): {e}; body: {}",
            redact_secret_params(&snippet(&text))
        ))
    })?;
    serde_json::from_value(value).map_err(|e| {
        UploadError::Protocol(format!(
            "decode {label} body (status {status}): {e}; body: {}",
            redact_secret_params(&snippet(&text))
        ))
    })
}

fn parse_json_with_error<T: DeserializeOwned>(resp: Response, label: &str) -> Result<T> {
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| UploadError::from_reqwest(&format!("read {label} body"), e))?;
    if !status.is_success() {
        return Err(UploadError::from_http_status(status.as_u16(), text));
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        UploadError::Protocol(format!(
            "decode {label} json (status {status}): {e}; body: {}",
            redact_secret_params(&snippet(&text))
        ))
    })?;
    if let Some(err) = api_error(&value) {
        return Err(classify_api_error(&err.code, &err.message));
    }
    serde_json::from_value(value).map_err(|e| {
        UploadError::Protocol(format!(
            "decode {label} body (status {status}): {e}; body: {}",
            redact_secret_params(&snippet(&text))
        ))
    })
}

pub(super) fn api_error(value: &serde_json::Value) -> Option<ApiError> {
    if let Some(code) = value.get("res_code") {
        let is_error = if let Some(n) = code.as_i64() {
            n != 0
        } else if let Some(s) = code.as_str() {
            !s.is_empty() && s != "0"
        } else {
            !code.is_null()
        };
        if is_error {
            let msg = value
                .get("res_message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            let code_str = code
                .as_str()
                .map(ToString::to_string)
                .unwrap_or_else(|| code.to_string());
            return Some(ApiError {
                code: code_str,
                message: msg.to_string(),
            });
        }
    }

    if let Some(code) = value.get("code").and_then(|v| v.as_str())
        && !code.is_empty()
        && !code.eq_ignore_ascii_case("SUCCESS")
        && code != "0"
    {
        let msg = value
            .get("msg")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("message").and_then(|v| v.as_str()))
            .unwrap_or("unknown error");
        return Some(ApiError {
            code: code.to_string(),
            message: msg.to_string(),
        });
    }

    if let Some(code) = value.get("errorCode").and_then(|v| v.as_str())
        && !code.is_empty()
    {
        let msg = value
            .get("errorMsg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Some(ApiError {
            code: code.to_string(),
            message: msg.to_string(),
        });
    }

    if let Some(err) = value.get("error").and_then(|v| v.as_str())
        && !err.is_empty()
    {
        return Some(ApiError {
            code: "error".to_string(),
            message: err.to_string(),
        });
    }

    None
}
