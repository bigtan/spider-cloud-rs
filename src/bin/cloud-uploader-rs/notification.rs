use tracing::{info, warn};

use super::config::Config;
use super::uploader::UploadResult;
use spider_cloud_rs::notify;
use spider_cloud_rs::notify::Notifier as EsNotifier;
use spider_cloud_rs::uploader::UploadAttempt;

/// Notification manager that coordinates multiple notification services
pub struct NotificationManager {
    chanify: Option<notify::chanify::ChanifyNotifier>,
    email: Option<notify::email::EmailNotifier>,
    pushgo: Option<notify::pushgo::PushgoNotifier>,
}

impl NotificationManager {
    /// Create a new notification manager from configuration
    pub fn from_config(config: &Config) -> Self {
        let mut chanify = None;
        let mut email = None;
        let mut pushgo = None;

        // Chanify
        if config.chanify_enabled {
            if let Some(token) = &config.chanify_token {
                info!("Creating Chanify notifier");
                chanify = Some(notify::chanify::ChanifyNotifier::new(
                    config.chanify_url.clone(),
                    token.clone(),
                ));
            } else {
                warn!("Chanify notifications enabled but missing CHANIFY_TOKEN");
            }
        }

        // Email
        if config.email_enabled {
            if let (Some(sender), Some(password), Some(recipient)) = (
                &config.email_sender,
                &config.email_password,
                &config.email_recipient,
            ) {
                info!("Creating Email notifier");
                email = Some(notify::email::EmailNotifier::new(
                    sender.clone(),
                    password.clone(),
                    recipient.clone(),
                ));
            } else {
                warn!("Email notifications enabled but missing required configuration");
            }
        }

        // Pushgo
        if config.pushgo_enabled {
            if let (Some(api_token), Some(hex_key), Some(channel_id), Some(password)) = (
                &config.pushgo_api_token,
                &config.pushgo_hex_key,
                &config.pushgo_channel_id,
                &config.pushgo_password,
            ) {
                info!("Creating Pushgo notifier");
                pushgo = Some(notify::pushgo::PushgoNotifier::new(
                    config.pushgo_url.clone(),
                    api_token.clone(),
                    hex_key.clone(),
                    channel_id.clone(),
                    password.clone(),
                    config.pushgo_icon.clone(),
                    config.pushgo_image.clone(),
                ));
            } else {
                warn!("Pushgo notifications enabled but missing required configuration");
            }
        }

        if chanify.is_none() && email.is_none() && pushgo.is_none() {
            warn!("No notification channels configured");
        }

        Self {
            chanify,
            email,
            pushgo,
        }
    }

    /// Send pre-formatted bodies to every configured channel
    fn dispatch(&self, subject: &str, chanify_text: &str, email_html: &str, pushgo_md: &str) {
        if let Some(notifier) = &self.chanify
            && let Err(err) = notifier.send(subject, chanify_text)
        {
            warn!("Chanify notification failed: {}", err);
        }

        if let Some(notifier) = &self.email
            && let Err(err) = notifier.send_html(subject, email_html)
        {
            warn!("Email notification failed: {}", err);
        }

        if let Some(notifier) = &self.pushgo
            && let Err(err) = notifier.send(subject, pushgo_md)
        {
            warn!("Pushgo notification failed: {}", err);
        }
    }

    /// Send upload completion notification
    pub fn send_upload_result(&self, server_location: &str, date_str: &str, result: &UploadResult) {
        if result.attempts.is_empty() {
            return;
        }

        let report = UploadReport {
            server_location,
            date_str,
            success: result.overall_success,
            attempts: &result.attempts,
        };

        let subject = format!(
            "[{}] 数据上传{} - {}",
            server_location,
            report.status_label(),
            date_str
        );
        self.dispatch(
            &subject,
            &chanify_upload_message(&report),
            &email_upload_html(&subject, &report),
            &pushgo_upload_markdown(&report),
        );
    }

    /// Send archive failure notification
    pub fn send_archive_failure(&self, server_location: &str, date_str: &str, error: &str) {
        let subject = format!("[{}] 数据打包失败 - {}", server_location, date_str);
        let headline = format!("{date_str}数据打包失败");

        let chanify_text =
            format!("❌ {headline}\n\n🖥️ 服务器: {server_location}\n\n错误: {error}");
        let email_html = email_shell(
            &subject,
            "#FF5252",
            "❌",
            &format!(
                "<p><strong>{headline}</strong></p>\
                 <p>🖥️ <strong>服务器:</strong> {server_location}</p>\
                 <p><strong>错误:</strong> {error}</p>"
            ),
        );
        let pushgo_md = format!(
            "> {headline}\n\n- **服务器**: {server_location}\n- **状态**: 失败\n\n**错误**\n\n{error}\n"
        );

        self.dispatch(&subject, &chanify_text, &email_html, &pushgo_md);
    }
}

/// Structured upload outcome handed to the per-channel formatters,
/// instead of round-tripping through a formatted string.
struct UploadReport<'a> {
    server_location: &'a str,
    date_str: &'a str,
    success: bool,
    attempts: &'a [UploadAttempt],
}

impl UploadReport<'_> {
    fn status_label(&self) -> &'static str {
        if self.success {
            "完成"
        } else {
            "部分失败"
        }
    }

    fn headline(&self) -> String {
        format!("{}数据上传{}", self.date_str, self.status_label())
    }
}

fn attempt_status(attempt: &UploadAttempt) -> String {
    if attempt.success {
        "成功".to_string()
    } else if let Some(error) = &attempt.error {
        format!("失败 ({})", error)
    } else {
        "失败".to_string()
    }
}

fn chanify_upload_message(report: &UploadReport) -> String {
    let icon = if report.success { "✅" } else { "❌" };
    let mut text = format!(
        "{icon} {}\n\n🖥️ 服务器: {}\n",
        report.headline(),
        report.server_location
    );
    for attempt in report.attempts {
        let icon = if attempt.success { "✅" } else { "❌" };
        text.push_str(&format!(
            "\n{}: {} {}\n",
            attempt.name,
            icon,
            attempt_status(attempt)
        ));
    }
    text
}

fn pushgo_upload_markdown(report: &UploadReport) -> String {
    let status = if report.success { "成功" } else { "失败" };
    let mut body = format!(
        "> {}\n\n- **服务器**: {}\n- **状态**: {}\n\n## 上传结果\n\n| 服务 | 状态 |\n| --- | --- |\n",
        report.headline(),
        report.server_location,
        status
    );
    for attempt in report.attempts {
        body.push_str(&format!(
            "| {} | {} |\n",
            attempt.name,
            attempt_status(attempt)
        ));
    }
    body
}

fn email_upload_html(subject: &str, report: &UploadReport) -> String {
    let (theme_color, status_icon) = if report.success {
        ("#4CAF50", "✅")
    } else {
        ("#FF5252", "❌")
    };

    let mut content = format!(
        "<p><strong>{}</strong></p>\
         <p>🖥️ <strong>服务器:</strong> {}</p>\
         <h3>上传结果</h3><table><tr><th>服务</th><th>状态</th></tr>",
        report.headline(),
        report.server_location
    );
    for attempt in report.attempts {
        let (class, icon) = if attempt.success {
            ("status-success", "✅")
        } else {
            ("status-fail", "❌")
        };
        content.push_str(&format!(
            "<tr><td>{}</td><td class=\"{}\">{} {}</td></tr>",
            attempt.name,
            class,
            icon,
            attempt_status(attempt)
        ));
    }
    content.push_str("</table>");

    email_shell(subject, theme_color, status_icon, &content)
}

fn email_shell(subject: &str, theme_color: &str, status_icon: &str, body_html: &str) -> String {
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
            background-color: {theme_color}; color: white;
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
            <h1>{status_icon} {subject}</h1>
        </div>
        <div class="content">{body_html}
        </div>
        <div class="footer">
            <p>此邮件由系统自动生成，请勿回复</p>
            <p>© 2025 云存储上传系统</p>
        </div>
    </div>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report<'a>(attempts: &'a [UploadAttempt]) -> UploadReport<'a> {
        UploadReport {
            server_location: "ServerA",
            date_str: "20260611",
            success: false,
            attempts,
        }
    }

    #[test]
    fn chanify_message_includes_each_attempt() {
        let attempts = vec![
            UploadAttempt::success("BaiduPan"),
            UploadAttempt::failure("Cloud189", "timeout"),
        ];
        let text = chanify_upload_message(&sample_report(&attempts));
        assert!(text.contains("❌ 20260611数据上传部分失败"));
        assert!(text.contains("🖥️ 服务器: ServerA"));
        assert!(text.contains("BaiduPan: ✅ 成功"));
        assert!(text.contains("Cloud189: ❌ 失败 (timeout)"));
    }

    #[test]
    fn pushgo_markdown_renders_result_table() {
        let attempts = vec![UploadAttempt::success("BaiduPan")];
        let md = pushgo_upload_markdown(&sample_report(&attempts));
        assert!(md.contains("| 服务 | 状态 |"));
        assert!(md.contains("| BaiduPan | 成功 |"));
    }

    #[test]
    fn email_html_renders_status_classes() {
        let attempts = vec![
            UploadAttempt::success("BaiduPan"),
            UploadAttempt::failure("Cloud189", "timeout"),
        ];
        let html = email_upload_html("subject", &sample_report(&attempts));
        assert!(html.contains("status-success"));
        assert!(html.contains("status-fail"));
        assert!(html.contains("失败 (timeout)"));
    }
}
