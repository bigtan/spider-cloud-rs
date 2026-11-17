use std::fmt;

pub type Result<T> = std::result::Result<T, UploadError>;

/// Typed error for cloud upload operations, replacing ad-hoc string matching.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("authentication expired or invalid: {0}")]
    AuthExpired(String),

    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("transient network error: {0}")]
    TransientNetwork(String),

    #[error("transient server error (HTTP {status}): {body}")]
    TransientServer { status: u16, body: String },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("API error ({code}): {message}")]
    Api { code: String, message: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl UploadError {
    /// Returns `true` when the failure indicates expired/invalid credentials or session state.
    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::AuthExpired(_))
    }

    /// Returns `true` for transient errors where retrying with backoff is appropriate.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited(_) | Self::TransientNetwork(_) | Self::TransientServer { .. }
        )
    }

    /// Map a non-success HTTP status code and response snippet into a typed [`UploadError`].
    ///
    /// Retries are strictly limited to `408`, `429`, and `5xx`; `401` maps to
    /// [`UploadError::AuthExpired`] (not retried blindly) and other `4xx` codes
    /// (such as `400`, `403`, `404`) map to non-retryable [`UploadError::Protocol`].
    pub fn from_http_status(status: u16, body: impl Into<String>) -> Self {
        let body = redact_secret_params(&snippet(&body.into()));
        match status {
            401 => Self::AuthExpired(format!("HTTP 401: {body}")),
            429 => Self::RateLimited(if body.is_empty() {
                "HTTP 429 Too Many Requests".to_string()
            } else {
                format!("HTTP 429: {body}")
            }),
            _ if spider_common::retry::is_retryable_http_status(status) => {
                Self::TransientServer { status, body }
            }
            _ => Self::Protocol(if body.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {body}")
            }),
        }
    }

    /// Convert a [`reqwest::Error`] into a typed [`UploadError`], stripping URLs and secret params.
    pub fn from_reqwest(context: &str, err: reqwest::Error) -> Self {
        let is_transient =
            err.is_timeout() || err.is_connect() || err.is_request() || err.is_body();
        let clean = redact_secret_params(&err.without_url().to_string());
        let msg = format!("{context}: {clean}");
        if is_transient {
            Self::TransientNetwork(msg)
        } else {
            Self::Protocol(msg)
        }
    }
}

impl From<url::ParseError> for UploadError {
    fn from(err: url::ParseError) -> Self {
        Self::Protocol(format!("invalid URL: {err}"))
    }
}

/// Truncate an arbitrary UTF-8 string cleanly at character boundaries for log/error snippets.
pub(crate) fn snippet(text: &str) -> String {
    let trimmed = text.trim();
    const MAX_BYTES: usize = 200;
    if trimmed.len() <= MAX_BYTES {
        trimmed.to_string()
    } else {
        let mut end = MAX_BYTES;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
    }
}

/// Scrub sensitive query/form parameter values (e.g. `access_token`, `client_secret`) from text.
pub(crate) fn redact_secret_params(input: &str) -> String {
    const SECRET_KEYS: &[&str] = &[
        "access_token=",
        "accesstoken=",
        "refresh_token=",
        "refreshtoken=",
        "client_secret=",
        "client_id=",
        "app_secret=",
        "password=",
        "epd=",
        "code=",
        "sessionkey=",
        "sessionsecret=",
        "sson=",
    ];

    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;

    while idx < input.len() {
        let remaining_lower = &lower[idx..];
        let matched = SECRET_KEYS
            .iter()
            .find(|&&k| remaining_lower.starts_with(k))
            .copied();

        if let Some(key) = matched {
            let boundary_ok = idx == 0
                || matches!(
                    input.as_bytes()[idx - 1],
                    b'?' | b'&' | b';' | b' ' | b'"' | b'\'' | b',' | b'('
                );
            if boundary_ok {
                let key_end = idx + key.len();
                out.push_str(&input[idx..key_end]);
                out.push_str("[REDACTED]");
                let mut val_end = key_end;
                while val_end < input.len() {
                    let b = input.as_bytes()[val_end];
                    if matches!(
                        b,
                        b'&' | b' ' | b'\r' | b'\n' | b'\t' | b'"' | b'\'' | b')' | b'}' | b','
                    ) {
                        break;
                    }
                    val_end += 1;
                }
                idx = val_end;
                continue;
            }
        }

        if let Some(ch) = input[idx..].chars().next() {
            out.push(ch);
            idx += ch.len_utf8();
        } else {
            break;
        }
    }

    out
}

/// Redact a URL by removing query string and fragment before logging or returning in errors.
pub(crate) fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut parsed) => {
            if parsed.query().is_some() {
                parsed.set_query(Some("..."));
            }
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => redact_secret_params(raw),
    }
}

/// Format an optional secret consistently as `Some("[REDACTED]")` or `None`.
pub(crate) fn redacted_opt<T>(opt: &Option<T>) -> &'static str {
    if opt.is_some() {
        "Some([REDACTED])"
    } else {
        "None"
    }
}

pub(crate) struct RedactedStr<'a>(pub(crate) &'a str);

impl fmt::Debug for RedactedStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str("\"\"")
        } else {
            f.write_str("\"[REDACTED]\"")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_classification_methods_work_without_string_matching() {
        let auth = UploadError::AuthExpired("session expired".into());
        assert!(auth.is_auth_error());
        assert!(!auth.is_retryable());

        let rate = UploadError::RateLimited("429".into());
        assert!(!rate.is_auth_error());
        assert!(rate.is_retryable());

        let net = UploadError::TransientNetwork("timed out".into());
        assert!(!net.is_auth_error());
        assert!(net.is_retryable());

        let srv = UploadError::TransientServer {
            status: 503,
            body: "busy".into(),
        };
        assert!(!srv.is_auth_error());
        assert!(srv.is_retryable());

        let cfg = UploadError::InvalidConfig("missing key".into());
        assert!(!cfg.is_auth_error());
        assert!(!cfg.is_retryable());

        let proto = UploadError::Protocol("HTTP 400: bad".into());
        assert!(!proto.is_auth_error());
        assert!(!proto.is_retryable());

        // An Api error containing words like "login" or "token" in a filename must NOT
        // falsely trigger auth-error or retry logic.
        let api = UploadError::Api {
            code: "FileError".into(),
            message: "file login_token.txt rejected".into(),
        };
        assert!(!api.is_auth_error());
        assert!(!api.is_retryable());
    }

    #[test]
    fn http_status_mapping_retries_only_408_429_5xx() {
        assert!(UploadError::from_http_status(408, "timeout").is_retryable());
        assert!(UploadError::from_http_status(429, "slow down").is_retryable());
        assert!(UploadError::from_http_status(500, "err").is_retryable());
        assert!(UploadError::from_http_status(502, "bad gw").is_retryable());
        assert!(UploadError::from_http_status(503, "unavail").is_retryable());
        assert!(UploadError::from_http_status(504, "gw timeout").is_retryable());

        for code in [400, 403, 404, 413, 422] {
            let err = UploadError::from_http_status(code, "client error");
            assert!(!err.is_retryable(), "status {code} must not be retryable");
            assert!(!err.is_auth_error(), "status {code} must not be auth error");
        }

        let unauth = UploadError::from_http_status(401, "unauthorized");
        assert!(unauth.is_auth_error());
        assert!(!unauth.is_retryable());
    }

    #[test]
    fn redact_secret_params_scrubs_sensitive_values() {
        let raw = "https://pan.baidu.com/api?method=precreate&access_token=SECRET123&client_secret=TOPSEC&path=/a";
        let scrubbed = redact_secret_params(raw);
        assert!(!scrubbed.contains("SECRET123"));
        assert!(!scrubbed.contains("TOPSEC"));
        assert!(scrubbed.contains("access_token=[REDACTED]"));
        assert!(scrubbed.contains("client_secret=[REDACTED]"));
        assert!(scrubbed.contains("path=/a"));
    }

    #[test]
    fn snippet_respects_utf8_boundaries() {
        let s = "测".repeat(100);
        let out = snippet(&s);
        assert!(out.ends_with("..."));
        assert!(out.len() <= 203);
    }
}
