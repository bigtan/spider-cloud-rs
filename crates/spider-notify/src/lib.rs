#![forbid(unsafe_code)]

pub use spider_common::Result;

pub mod chanify;
pub mod config;
pub mod dispatcher;
pub mod email;
pub mod pushgo;

pub use chanify::ChanifyNotifier;
pub use config::{ChanifyConfig, EmailConfig, NotifierConfig, PushgoConfig};
pub use dispatcher::{NotificationDispatcher, NotificationPayload, NotifyAttempt};
pub use email::EmailNotifier;
pub use pushgo::PushgoNotifier;

/// Common synchronous notification channel interface.
pub trait Notifier: Send + Sync {
    fn name(&self) -> &str;
    fn send(&self, subject: &str, message: &str) -> Result<()>;
}

/// Escape HTML special characters in dynamic strings interpolated into HTML email templates.
pub fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Render a responsive HTML email wrapper around `body_html` while escaping `subject` and `status_icon`.
pub fn render_email_shell(
    subject: &str,
    theme_color: &str,
    status_icon: &str,
    body_html: &str,
) -> String {
    let escaped_subject = escape_html(subject);
    let escaped_icon = escape_html(status_icon);
    let safe_color = if !theme_color.trim().is_empty()
        && theme_color.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '#' | '(' | ')' | ',' | '.' | '%' | ' ' | '-')
        }) {
        theme_color.trim()
    } else {
        "#3498db"
    };

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <style>
        body {{
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            margin: 0; padding: 0; background-color: #f4f4f4;
        }}
        .container {{
            max-width: 600px; margin: 20px auto; background-color: #fff;
            border-radius: 8px; overflow: hidden; box-shadow: 0 0 10px rgba(0,0,0,0.1);
        }}
        .header {{
            background-color: {safe_color}; color: white;
            padding: 20px; text-align: center;
        }}
        .content {{ padding: 20px; }}
        .footer {{
            background-color: #f9f9f9; padding: 15px; text-align: center;
            font-size: 12px; color: #666;
        }}
        table {{
            width: 100%; border-collapse: collapse; margin: 15px 0;
        }}
        th, td {{
            padding: 12px; text-align: left; border-bottom: 1px solid #eee;
        }}
        th {{ background-color: #f2f2f2; }}
        .status-success {{ color: #4CAF50; font-weight: bold; }}
        .status-fail {{ color: #FF5252; font-weight: bold; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>{escaped_icon} {escaped_subject}</h1>
        </div>
        <div class="content">{body_html}
        </div>
        <div class="footer">
            <p>此邮件由系统自动生成，请勿回复</p>
        </div>
    </div>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::{escape_html, render_email_shell};

    #[test]
    fn escape_html_replaces_special_characters() {
        assert_eq!(
            escape_html("<html>502 & \"Bad\" 'Gateway'</html>"),
            "&lt;html&gt;502 &amp; &quot;Bad&quot; &#39;Gateway&#39;&lt;/html&gt;"
        );
    }

    #[test]
    fn render_email_shell_escapes_subject_and_sanitizes_color() {
        let html = render_email_shell(
            "<script>alert(1)</script>",
            "#4CAF50; } body{",
            "✅",
            "<p>ok</p>",
        );
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("background-color: #3498db;"));
        assert!(html.contains("<p>ok</p>"));
    }
}
