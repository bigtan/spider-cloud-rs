use crate::chanify::ChanifyNotifier;
use crate::config::NotifierConfig;
use crate::email::EmailNotifier;
use crate::pushgo::PushgoNotifier;
use crate::{Notifier, Result};
use anyhow::Context;
use tracing::warn;

/// Multi-format notification message delivered across all configured channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPayload {
    pub subject: String,
    pub plain_text: String,
    pub html_body: Option<String>,
    pub markdown_body: Option<String>,
}

/// Outcome for a single notification channel delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyAttempt {
    pub channel: String,
    pub success: bool,
    pub error: Option<String>,
}

impl NotifyAttempt {
    pub fn success(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            success: true,
            error: None,
        }
    }

    pub fn failure(channel: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            success: false,
            error: Some(error.into()),
        }
    }
}

/// Concurrent dispatcher over configured Chanify, Email, and Pushgo notifiers.
#[derive(Debug)]
pub struct NotificationDispatcher {
    chanify: Option<ChanifyNotifier>,
    email: Option<EmailNotifier>,
    pushgo: Option<PushgoNotifier>,
}

impl NotificationDispatcher {
    /// Construct a [`NotificationDispatcher`] from validated [`NotifierConfig`].
    pub fn from_config(cfg: &NotifierConfig) -> Result<Self> {
        cfg.validate()?;

        let chanify = if cfg.chanify.enabled {
            let url = cfg
                .chanify
                .url
                .clone()
                .context("Chanify enabled without url")?;
            let token = cfg
                .chanify
                .token
                .clone()
                .context("Chanify enabled without token")?;
            Some(ChanifyNotifier::new(url, token)?)
        } else {
            None
        };

        let email = if cfg.email.enabled {
            let sender = cfg
                .email
                .sender
                .clone()
                .context("Email enabled without sender")?;
            let password = cfg
                .email
                .password
                .clone()
                .context("Email enabled without password")?;
            let recipient = cfg
                .email
                .recipient
                .clone()
                .context("Email enabled without recipient")?;
            let smtp_host = cfg
                .email
                .smtp_host
                .clone()
                .unwrap_or_else(|| "smtp.qq.com".to_string());
            let smtp_port = cfg.email.smtp_port.unwrap_or(465);
            Some(EmailNotifier::with_smtp(
                sender, password, recipient, smtp_host, smtp_port,
            ))
        } else {
            None
        };

        let pushgo = if cfg.pushgo.enabled {
            let url = cfg
                .pushgo
                .url
                .clone()
                .context("Pushgo enabled without url")?;
            let api_token = cfg
                .pushgo
                .api_token
                .clone()
                .context("Pushgo enabled without api_token")?;
            let hex_key = cfg
                .pushgo
                .hex_key
                .clone()
                .context("Pushgo enabled without hex_key")?;
            let channel_id = cfg
                .pushgo
                .channel_id
                .clone()
                .context("Pushgo enabled without channel_id")?;
            let password = cfg
                .pushgo
                .password
                .clone()
                .context("Pushgo enabled without password")?;
            Some(PushgoNotifier::new(
                url,
                api_token,
                hex_key,
                channel_id,
                password,
                cfg.pushgo.icon.clone(),
                cfg.pushgo.image.clone(),
            )?)
        } else {
            None
        };

        Ok(Self {
            chanify,
            email,
            pushgo,
        })
    }

    /// Return `true` if at least one notification channel is enabled.
    pub fn has_channels(&self) -> bool {
        self.chanify.is_some() || self.email.is_some() || self.pushgo.is_some()
    }

    /// Deliver `payload` concurrently across all enabled channels via `std::thread::scope`.
    pub fn dispatch(&self, payload: &NotificationPayload) -> Vec<NotifyAttempt> {
        if !self.has_channels() {
            return Vec::new();
        }

        std::thread::scope(|scope| {
            let mut handles = Vec::new();

            if let Some(chanify) = &self.chanify {
                handles.push(scope.spawn(move || {
                    match chanify.send(&payload.subject, &payload.plain_text) {
                        Ok(()) => NotifyAttempt::success(chanify.name()),
                        Err(err) => {
                            warn!("Chanify notification failed: {err:#}");
                            NotifyAttempt::failure(chanify.name(), format!("{err:#}"))
                        }
                    }
                }));
            }

            if let Some(email) = &self.email {
                handles.push(scope.spawn(move || {
                    let res = match payload.html_body.as_deref() {
                        Some(html) => email.send_html(&payload.subject, html),
                        None => email.send(&payload.subject, &payload.plain_text),
                    };
                    match res {
                        Ok(()) => NotifyAttempt::success(email.name()),
                        Err(err) => {
                            warn!("Email notification failed: {err:#}");
                            NotifyAttempt::failure(email.name(), format!("{err:#}"))
                        }
                    }
                }));
            }

            if let Some(pushgo) = &self.pushgo {
                handles.push(scope.spawn(move || {
                    let body = payload
                        .markdown_body
                        .as_deref()
                        .unwrap_or(&payload.plain_text);
                    match pushgo.send(&payload.subject, body) {
                        Ok(()) => NotifyAttempt::success(pushgo.name()),
                        Err(err) => {
                            warn!("Pushgo notification failed: {err:#}");
                            NotifyAttempt::failure(pushgo.name(), format!("{err:#}"))
                        }
                    }
                }));
            }

            handles
                .into_iter()
                .map(|handle| match handle.join() {
                    Ok(attempt) => attempt,
                    Err(_) => NotifyAttempt::failure("Unknown", "notification thread panicked"),
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChanifyConfig, EmailConfig, PushgoConfig};
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes128Gcm, Nonce};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as Base64Standard;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn read_http_request(mut stream: std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut header_end = None;
        let mut content_length = 0usize;

        loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if header_end.is_none()
                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                for line in headers.lines() {
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if let Some(end) = header_end
                && buf.len() >= end + content_length
            {
                break;
            }
        }

        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn dispatcher_empty_config_reports_no_channels() {
        let cfg = NotifierConfig::default();
        let dispatcher = NotificationDispatcher::from_config(&cfg).unwrap();
        assert!(!dispatcher.has_channels());
        let attempts = dispatcher.dispatch(&NotificationPayload {
            subject: "S".to_string(),
            plain_text: "P".to_string(),
            html_body: None,
            markdown_body: None,
        });
        assert!(attempts.is_empty());
    }

    #[test]
    fn dispatcher_sends_chanify_and_pushgo_concurrently() {
        let chanify_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let chanify_addr = chanify_listener.local_addr().unwrap();

        let pushgo_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let pushgo_addr = pushgo_listener.local_addr().unwrap();

        let (tx, rx) = mpsc::channel();
        let tx_c = tx.clone();
        let h1 = std::thread::spawn(move || {
            let (stream, _) = chanify_listener.accept().unwrap();
            let req = read_http_request(stream);
            tx_c.send(("chanify", req)).unwrap();
        });
        let h2 = std::thread::spawn(move || {
            let (stream, _) = pushgo_listener.accept().unwrap();
            let req = read_http_request(stream);
            tx.send(("pushgo", req)).unwrap();
        });

        let hex_key = "00112233445566778899aabbccddeeff";
        let cfg = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some(format!("http://{chanify_addr}/v1/sender")),
                token: Some("tok-123".to_string()),
            },
            email: EmailConfig::default(),
            pushgo: PushgoConfig {
                enabled: true,
                url: Some(format!("http://{pushgo_addr}/message")),
                api_token: Some("api-456".to_string()),
                hex_key: Some(hex_key.to_string()),
                channel_id: Some("chan-1".to_string()),
                password: Some("pw-789".to_string()),
                icon: None,
                image: None,
            },
        };

        let dispatcher = NotificationDispatcher::from_config(&cfg).unwrap();
        assert!(dispatcher.has_channels());

        let attempts = dispatcher.dispatch(&NotificationPayload {
            subject: "Alert".to_string(),
            plain_text: "Plain body".to_string(),
            html_body: Some("<p>HTML</p>".to_string()),
            markdown_body: Some("**Markdown body**".to_string()),
        });

        h1.join().unwrap();
        h2.join().unwrap();

        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().all(|a| a.success));

        let mut received = std::collections::HashMap::new();
        for _ in 0..2 {
            let (kind, raw) = rx.recv().unwrap();
            let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            received.insert(kind, (raw, body));
        }

        let (_, chanify_body) = &received["chanify"];
        let chanify_json: serde_json::Value = serde_json::from_str(chanify_body).unwrap();
        assert_eq!(chanify_json["token"], "tok-123");
        assert_eq!(chanify_json["title"], "Alert");
        assert_eq!(chanify_json["text"], "Plain body");

        let (pushgo_raw, pushgo_body) = &received["pushgo"];
        assert!(pushgo_raw.contains("Bearer api-456"));
        let pushgo_json: serde_json::Value = serde_json::from_str(pushgo_body).unwrap();
        assert_eq!(pushgo_json["channel_id"], "chan-1");
        assert_eq!(pushgo_json["password"], "pw-789");

        // Verify AES-128-GCM ciphertext decrypts back to markdown_body
        let ciphertext_b64 = pushgo_json["ciphertext"].as_str().unwrap();
        let raw_cipher = Base64Standard.decode(ciphertext_b64).unwrap();
        let (ct, nonce_slice) = raw_cipher.split_at(raw_cipher.len() - 12);
        let key_bytes = hex::decode(hex_key).unwrap();
        let cipher = Aes128Gcm::new_from_slice(&key_bytes).unwrap();
        let nonce = Nonce::try_from(nonce_slice).unwrap();
        let decrypted = cipher.decrypt(&nonce, ct).unwrap();
        let inner: serde_json::Value = serde_json::from_slice(&decrypted).unwrap();
        assert_eq!(inner["title"], "Alert");
        assert_eq!(inner["body"], "**Markdown body**");
    }

    #[test]
    fn dispatcher_collects_partial_failures_without_panicking() {
        let ok_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ok_addr = ok_listener.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (stream, _) = ok_listener.accept().unwrap();
            let _ = read_http_request(stream);
        });

        // Closed port on localhost for fast failure
        let dead_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead_listener.local_addr().unwrap();
        drop(dead_listener);

        let cfg = NotifierConfig {
            chanify: ChanifyConfig {
                enabled: true,
                url: Some(format!("http://{ok_addr}/v1/sender")),
                token: Some("tok".to_string()),
            },
            email: EmailConfig::default(),
            pushgo: PushgoConfig {
                enabled: true,
                url: Some(format!("http://{dead_addr}/message")),
                api_token: Some("api".to_string()),
                hex_key: Some("00112233445566778899aabbccddeeff".to_string()),
                channel_id: Some("c".to_string()),
                password: Some("p".to_string()),
                icon: None,
                image: None,
            },
        };

        let dispatcher = NotificationDispatcher::from_config(&cfg).unwrap();
        let attempts = dispatcher.dispatch(&NotificationPayload {
            subject: "Test".to_string(),
            plain_text: "Msg".to_string(),
            html_body: None,
            markdown_body: None,
        });
        h.join().unwrap();

        assert_eq!(attempts.len(), 2);
        let chanify_res = attempts.iter().find(|a| a.channel == "Chanify").unwrap();
        let pushgo_res = attempts.iter().find(|a| a.channel == "Pushgo").unwrap();
        assert!(chanify_res.success);
        assert!(!pushgo_res.success);
        assert!(pushgo_res.error.is_some());
    }
}
