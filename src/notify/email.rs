use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use tracing::{debug, error, info};

use crate::Result;
use crate::notify::Notifier;

pub struct EmailNotifier {
    sender: String,
    password: String,
    recipient: String,
    smtp_host: String,
    smtp_port: u16,
}

impl EmailNotifier {
    pub fn new(sender: String, password: String, recipient: String) -> Self {
        Self {
            sender,
            password,
            recipient,
            smtp_host: "smtp.qq.com".to_string(),
            smtp_port: 465,
        }
    }

    pub fn with_smtp(
        sender: String,
        password: String,
        recipient: String,
        smtp_host: String,
        smtp_port: u16,
    ) -> Self {
        Self {
            sender,
            password,
            recipient,
            smtp_host,
            smtp_port,
        }
    }

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

        let creds = Credentials::new(self.sender.clone(), self.password.clone());

        debug!("Connecting to SMTP server");
        let mailer = match SmtpTransport::relay(&self.smtp_host) {
            Ok(builder) => builder.credentials(creds).port(self.smtp_port).build(),
            Err(e) => return Err(e.into()),
        };

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
    use crate::notify::Notifier;
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
        let notifier = super::EmailNotifier::new(
            "invalid".to_string(),
            "secret".to_string(),
            "to@example.com".to_string(),
        );
        let result = notifier.send("Subject", "Body");
        assert!(result.is_err());
    }
}
