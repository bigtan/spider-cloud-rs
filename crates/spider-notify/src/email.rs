use crate::{Notifier, Result};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use std::fmt;
use std::time::Duration;
use tracing::{debug, error, info};
use zeroize::Zeroizing;

/// SMTP email notifier using TLS relay.
pub struct EmailNotifier {
    sender: String,
    password: Zeroizing<String>,
    recipient: String,
    smtp_host: String,
    smtp_port: u16,
}

impl fmt::Debug for EmailNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailNotifier")
            .field("sender", &self.sender)
            .field("password", &"<redacted>")
            .field("recipient", &self.recipient)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .finish()
    }
}

impl EmailNotifier {
    /// Construct an [`EmailNotifier`] using the default `smtp.qq.com:465` relay.
    pub fn new(sender: String, password: String, recipient: String) -> Self {
        Self::with_smtp(sender, password, recipient, "smtp.qq.com".to_string(), 465)
    }

    /// Construct an [`EmailNotifier`] with explicit SMTP host and port.
    pub fn with_smtp(
        sender: String,
        password: String,
        recipient: String,
        smtp_host: String,
        smtp_port: u16,
    ) -> Self {
        Self {
            sender,
            password: Zeroizing::new(password),
            recipient,
            smtp_host,
            smtp_port,
        }
    }

    /// Send an HTML-formatted email body.
    pub fn send_html(&self, subject: &str, html_body: &str) -> Result<()> {
        self.send_with_content_type(subject, html_body, ContentType::TEXT_HTML)
    }

    fn send_with_content_type(
        &self,
        subject: &str,
        body: &str,
        content_type: ContentType,
    ) -> Result<()> {
        debug!("Building email message ({:?})", content_type);
        let from: Mailbox = self
            .sender
            .parse()
            .map_err(|err| anyhow::anyhow!("invalid EMAIL_SENDER '{}': {err}", self.sender))?;
        let to: Mailbox = self.recipient.parse().map_err(|err| {
            anyhow::anyhow!("invalid EMAIL_RECIPIENT '{}': {err}", self.recipient)
        })?;
        let email = Message::builder()
            .from(from)
            .to(to)
            .subject(subject)
            .header(content_type)
            .body(body.to_string())?;

        let creds = Credentials::new(self.sender.clone(), self.password.as_str().to_string());

        debug!("Connecting to SMTP server {}", self.smtp_host);
        let mailer = SmtpTransport::relay(&self.smtp_host)?
            .credentials(creds)
            .port(self.smtp_port)
            .timeout(Some(Duration::from_secs(20)))
            .build();

        match mailer.send(&email) {
            Ok(_) => {
                info!("Email notification sent: {}", subject);
                Ok(())
            }
            Err(e) => {
                error!("Failed to send email notification: {:?}", e);
                Err(e.into())
            }
        }
    }
}

impl Notifier for EmailNotifier {
    fn name(&self) -> &str {
        "Email"
    }

    fn send(&self, subject: &str, message: &str) -> Result<()> {
        self.send_with_content_type(subject, message, ContentType::TEXT_PLAIN)
    }
}

#[cfg(test)]
mod tests {
    use super::EmailNotifier;
    use crate::Notifier;
    use lettre::message::header::ContentType;

    #[test]
    fn send_builds_plain_text_body() {
        let body = "Line1\nLine2";
        let email = lettre::Message::builder()
            .from("sender@example.com".parse().unwrap())
            .to("to@example.com".parse().unwrap())
            .subject("Subject")
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .unwrap();
        let formatted = String::from_utf8(email.formatted().to_vec()).unwrap();
        let normalized = formatted.replace("\r\n", "\n");
        assert!(normalized.contains("Line1\nLine2"));
    }

    #[test]
    fn invalid_sender_returns_error() {
        let notifier = EmailNotifier::new(
            "invalid".to_string(),
            "secret".to_string(),
            "to@example.com".to_string(),
        );
        let result = notifier.send("Subject", "Body");
        assert!(result.is_err());
    }

    #[test]
    fn debug_redacts_password() {
        let notifier = EmailNotifier::new(
            "sender@example.com".to_string(),
            "super-secret-smtp-pw".to_string(),
            "to@example.com".to_string(),
        );
        let dbg = format!("{notifier:?}");
        assert!(!dbg.contains("super-secret-smtp-pw"));
        assert!(dbg.contains("<redacted>"));
    }
}
