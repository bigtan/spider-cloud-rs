use crate::config::validate_notify_url;
use crate::{Notifier, Result};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as Base64Standard;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Map, Value, json};
use std::fmt;
use std::time::Duration;
use tracing::{debug, error, info};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Pushgo AES-GCM encrypted push notifier.
pub struct PushgoNotifier {
    url: String,
    api_token: Zeroizing<String>,
    hex_key: Zeroizing<String>,
    channel_id: String,
    password: Zeroizing<String>,
    icon: Option<String>,
    image: Option<String>,
    client: Client,
}

impl fmt::Debug for PushgoNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushgoNotifier")
            .field("url", &self.url)
            .field("api_token", &"<redacted>")
            .field("hex_key", &"<redacted>")
            .field("channel_id", &self.channel_id)
            .field("password", &"<redacted>")
            .field("icon", &self.icon)
            .field("image", &self.image)
            .finish()
    }
}

impl PushgoNotifier {
    /// Construct a new [`PushgoNotifier`], validating the endpoint URL and building the HTTP client.
    pub fn new(
        url: String,
        api_token: String,
        hex_key: String,
        channel_id: String,
        password: String,
        icon: Option<String>,
        image: Option<String>,
    ) -> Result<Self> {
        validate_notify_url(&url, "pushgo.url")?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build Pushgo HTTP client")?;
        Ok(Self {
            url: url.trim().to_string(),
            api_token: Zeroizing::new(api_token),
            hex_key: Zeroizing::new(hex_key),
            channel_id,
            password: Zeroizing::new(password),
            icon,
            image,
            client,
        })
    }

    pub(crate) fn encrypt_payload(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let key_bytes = Zeroizing::new(hex::decode(self.hex_key.trim())?);

        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext_with_tag = match key_bytes.len() {
            16 => {
                let cipher = Aes128Gcm::new_from_slice(&key_bytes).map_err(|e| anyhow!("{e:?}"))?;
                cipher
                    .encrypt(&nonce, plaintext)
                    .map_err(|e| anyhow!("{e:?}"))?
            }
            32 => {
                let cipher = Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| anyhow!("{e:?}"))?;
                cipher
                    .encrypt(&nonce, plaintext)
                    .map_err(|e| anyhow!("{e:?}"))?
            }
            _ => {
                return Err(anyhow!("PUSHGO_HEX_KEY must be 16 or 32 bytes in hex"));
            }
        };

        let mut final_binary = ciphertext_with_tag;
        final_binary.extend_from_slice(&nonce_bytes);
        Ok(final_binary)
    }
}

impl Notifier for PushgoNotifier {
    fn name(&self) -> &str {
        "Pushgo"
    }

    fn send(&self, subject: &str, message: &str) -> Result<()> {
        let pushgo_title = subject.to_string();
        let markdown_body = message.to_string();

        let mut source_data = Map::new();
        source_data.insert("title".to_string(), Value::String(pushgo_title.clone()));
        source_data.insert("body".to_string(), Value::String(markdown_body));
        if let Some(icon) = &self.icon
            && !icon.is_empty()
        {
            source_data.insert("icon".to_string(), Value::String(icon.clone()));
        }
        if let Some(image) = &self.image
            && !image.is_empty()
        {
            source_data.insert(
                "images".to_string(),
                Value::Array(vec![Value::String(image.clone())]),
            );
        }

        let json_bytes = Zeroizing::new(serde_json::to_vec(&source_data)?);
        let encrypted = self.encrypt_payload(&json_bytes)?;
        let base64_payload = Base64Standard.encode(encrypted);

        let post_body = json!({
            "title": pushgo_title,
            "channel_id": self.channel_id,
            "password": self.password.as_str(),
            "op_id": Uuid::new_v4().to_string(),
            "ciphertext": base64_payload,
        });

        debug!("Sending Pushgo notification to: {}", self.url);

        let response = self
            .client
            .post(&self.url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_token.as_str()),
            )
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&post_body)
            .send()
            .context("Pushgo HTTP request failed")?;

        if response.status().is_success() {
            info!("Pushgo notification sent: {}", subject);
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            error!("Failed to send Pushgo notification: {}", status);
            anyhow::bail!("pushgo request failed: {} {}", status, body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PushgoNotifier;

    #[test]
    fn encrypt_payload_rejects_invalid_key_length() {
        let notifier = PushgoNotifier::new(
            "http://localhost".to_string(),
            "token".to_string(),
            "abcd".to_string(),
            "ch".to_string(),
            "pw".to_string(),
            None,
            None,
        )
        .unwrap();
        let result = notifier.encrypt_payload(b"test");
        assert!(result.is_err());
    }

    #[test]
    fn debug_redacts_secrets_and_rejects_external_http() {
        assert!(
            PushgoNotifier::new(
                "http://example.com/message".to_string(),
                "api-secret".to_string(),
                "00112233445566778899aabbccddeeff".to_string(),
                "ch".to_string(),
                "pw-secret".to_string(),
                None,
                None,
            )
            .is_err()
        );

        let notifier = PushgoNotifier::new(
            "https://pushgo.example.com/message".to_string(),
            "api-secret".to_string(),
            "00112233445566778899aabbccddeeff".to_string(),
            "ch".to_string(),
            "pw-secret".to_string(),
            None,
            None,
        )
        .unwrap();
        let dbg = format!("{notifier:?}");
        assert!(!dbg.contains("api-secret"));
        assert!(!dbg.contains("00112233445566778899aabbccddeeff"));
        assert!(!dbg.contains("pw-secret"));
        assert!(dbg.contains("<redacted>"));
    }
}
