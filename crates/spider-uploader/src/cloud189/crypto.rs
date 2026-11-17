use std::time::SystemTime;

use aes::Aes128;
use aes::cipher::{Block, BlockCipherEncrypt, KeyInit as AesKeyInit};
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use httpdate::fmt_http_date;
use reqwest::blocking::RequestBuilder;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rsa::PublicKey;
use rsa::pkcs8::DecodePublicKey;
use sha1::Sha1;
use url::Url;
use uuid::Uuid;

use super::types::Session;
use crate::error::{Result, UploadError};

pub(super) fn encrypt_credentials(
    pub_key: &str,
    prefix: &str,
    user: &str,
    pass: &str,
) -> Result<(String, String)> {
    let pem = normalize_public_key(pub_key);
    let key = PublicKey::from_public_key_pem(&pem)
        .map_err(|e| UploadError::Protocol(format!("parse Cloud189 RSA public key: {e}")))?;
    let enc_user = key
        .pkcs1v15_encrypt(user.as_bytes())
        .map_err(|e| UploadError::Protocol(format!("encrypt username: {e}")))?;
    let enc_pass = key
        .pkcs1v15_encrypt(pass.as_bytes())
        .map_err(|e| UploadError::Protocol(format!("encrypt password: {e}")))?;
    Ok((
        format!("{}{}", prefix, hex::encode(enc_user)),
        format!("{}{}", prefix, hex::encode(enc_pass)),
    ))
}

pub(super) fn normalize_public_key(raw: &str) -> String {
    let mut base64 = String::new();
    for line in raw.lines() {
        if line.contains("BEGIN PUBLIC KEY") || line.contains("END PUBLIC KEY") {
            continue;
        }
        for ch in line.chars() {
            if ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' || ch == '=' {
                base64.push(ch);
            }
        }
    }
    if base64.is_empty() {
        base64 = raw
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '+' || *ch == '/' || *ch == '=')
            .collect();
    }
    let mut wrapped = String::new();
    let mut i = 0;
    while i < base64.len() {
        let end = usize::min(i + 64, base64.len());
        wrapped.push_str(&base64[i..end]);
        wrapped.push('\n');
        i = end;
    }
    format!("-----BEGIN PUBLIC KEY-----\n{wrapped}-----END PUBLIC KEY-----")
}

pub(super) fn aes_ecb_hex(data: &[u8], secret: &str) -> Result<String> {
    if secret.len() < 16 {
        return Err(UploadError::AuthExpired(
            "Cloud189 session secret too short".to_string(),
        ));
    }
    let key = &secret.as_bytes()[0..16];
    let cipher = Aes128::new_from_slice(key)
        .map_err(|e| UploadError::Protocol(format!("init AES-128 cipher: {e}")))?;
    let mut buf = data.to_vec();
    let pad = 16 - (buf.len() % 16);
    buf.extend(std::iter::repeat_n(pad as u8, pad));
    for chunk in buf.chunks_mut(16) {
        let block = <&mut Block<Aes128>>::try_from(chunk)
            .map_err(|_| UploadError::Protocol("invalid AES block size".to_string()))?;
        cipher.encrypt_block(block);
    }
    Ok(hex::encode(buf))
}

pub(super) fn encode_form_urlencoded(params: &[(String, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in params {
        serializer.append_pair(k, v);
    }
    serializer.finish()
}

/// Encode `k=v&...` plaintext for Cloud189 AES-ECB `params`, rejecting raw `'&'`
/// in any parameter key or value (and `'='` in keys) so callers cannot inject
/// or corrupt parameters. Note `partInfo` must NOT be URL-encoded because it
/// is omitted from `decodefields`.
pub(super) fn encode_param_plain(params: &[(String, String)]) -> Result<String> {
    let mut out = String::new();
    for (k, v) in params {
        if k.contains('&') || k.contains('=') {
            return Err(UploadError::Protocol(format!(
                "Cloud189 parameter key contains forbidden delimiter: {k}"
            )));
        }
        if v.contains('&') {
            return Err(UploadError::Protocol(format!(
                "Cloud189 parameter '{k}' value contains forbidden '&'"
            )));
        }
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    Ok(out)
}

pub(super) fn with_query(base: &str, params: &[(String, String)]) -> Result<Url> {
    let mut url = Url::parse(base)?;
    for (k, v) in params {
        url.query_pairs_mut().append_pair(k, v);
    }
    Ok(url)
}

pub(super) fn with_client_params(mut url: Url) -> Url {
    url.query_pairs_mut()
        .append_pair("rand", &chrono_millis().to_string())
        .append_pair("clientType", "TELEPC")
        .append_pair("version", "7.1.8.0")
        .append_pair("channelId", "web_cloud.189.cn");
    url
}

pub(super) fn parse_request_headers(raw: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    if raw.trim().is_empty() {
        return Ok(headers);
    }
    for item in raw.split('&') {
        if item.is_empty() {
            continue;
        }
        let mut iter = item.splitn(2, '=');
        let key = iter
            .next()
            .ok_or_else(|| UploadError::Protocol("invalid requestHeader pair".to_string()))?;
        let value = iter.next().unwrap_or("");
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|e| UploadError::Protocol(format!("invalid header name '{key}': {e}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| UploadError::Protocol(format!("invalid header value for '{key}': {e}")))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

pub(super) fn hmac_sha1_hex(data: &str, key: &[u8]) -> String {
    let mut mac = <Hmac<Sha1> as HmacKeyInit>::new_from_slice(key)
        .expect("HMAC-SHA1 accepts keys of any length");
    mac.update(data.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub(super) fn chrono_millis() -> i64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

pub(super) fn apply_signature(
    mut req: RequestBuilder,
    session: Option<&Session>,
    method: &str,
    path: &str,
    params: Option<&str>,
) -> RequestBuilder {
    let Some(session) = session.filter(|s| s.is_valid()) else {
        return req;
    };
    let date = fmt_http_date(SystemTime::now());
    let mut data = format!(
        "SessionKey={}&Operate={}&RequestURI={}&Date={}",
        session.key, method, path, date
    );
    if let Some(p) = params {
        data.push_str("&params=");
        data.push_str(p);
    }
    let signature = hmac_sha1_hex(&data, session.secret.as_bytes());
    req = req
        .header("Date", date)
        .header("user-agent", "desktop")
        .header("SessionKey", session.key.as_str())
        .header("Signature", signature)
        .header("X-Request-ID", Uuid::new_v4().to_string());
    req
}
