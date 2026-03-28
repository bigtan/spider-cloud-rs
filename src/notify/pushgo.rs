use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as Base64Standard;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Map, Value, json};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::Result;
use crate::notify::Notifier;

pub struct PushgoNotifier {
    url: String,
    api_token: String,
    hex_key: String,
    channel_id: String,
    password: String,
    icon: Option<String>,
    image: Option<String>,
    client: Client,
}

impl PushgoNotifier {
    pub fn new(
        url: String,
        api_token: String,
        hex_key: String,
        channel_id: String,
        password: String,
        icon: Option<String>,
        image: Option<String>,
    ) -> Self {
        Self {
            url,
            api_token,
            hex_key,
            channel_id,
            password,
            icon,
            image,
            client: Client::new(),
        }
    }

    fn encrypt_payload(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let key_bytes = hex::decode(self.hex_key.trim())?;

        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext_with_tag = match key_bytes.len() {
            16 => {
                let cipher =
                    Aes128Gcm::new_from_slice(&key_bytes).map_err(|e| anyhow!("{:?}", e))?;
                cipher
                    .encrypt(nonce, plaintext)
                    .map_err(|e| anyhow!("{:?}", e))?
            }
            32 => {
                let cipher =
                    Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| anyhow!("{:?}", e))?;
                cipher
                    .encrypt(nonce, plaintext)
                    .map_err(|e| anyhow!("{:?}", e))?
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
        if let Some(icon) = &self.icon {
            if !icon.is_empty() {
                source_data.insert("icon".to_string(), Value::String(icon.clone()));
            }
        }
        if let Some(image) = &self.image
            && !image.is_empty()
        {
            source_data.insert(
                "images".to_string(),
                Value::Array(vec![Value::String(image.clone())]),
            );
        }

        let json_bytes = serde_json::to_vec(&source_data)?;
        let encrypted = self.encrypt_payload(&json_bytes)?;
        let base64_payload = Base64Standard.encode(encrypted);

        let post_body = json!({
            "title": pushgo_title,
            "channel_id": self.channel_id,
            "password": self.password,
            "op_id": Uuid::new_v4().to_string(),
            "ciphertext": base64_payload,
        });

        debug!("Sending Pushgo notification to: {}", self.url);

        match self
            .client
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&post_body)
            .send()
        {
            Ok(response) => {
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
            Err(e) => Err(e.into()),
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
        );
        let result = notifier.encrypt_payload(b"test");
        assert!(result.is_err());
    }
}
