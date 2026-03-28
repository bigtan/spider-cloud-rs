use tracing::{info, warn};

use super::config::Config;
use super::uploader::UploadResult;
use spider_cloud_rs::notify;
use spider_cloud_rs::notify::Notifier as EsNotifier;

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

    /// Send notification to all configured channels
    pub fn send(&self, subject: &str, message: &str) -> bool {
        let mut success = false;

        if let Some(notifier) = &self.chanify {
            let formatted = format_chanify_message(subject, message);
            match notifier.send(subject, &formatted) {
                Ok(()) => success = true,
                Err(err) => warn!("Chanify notification failed: {}", err),
            }
        }

        if let Some(notifier) = &self.email {
            let html = format_email_html(subject, message);
            match notifier.send_html(subject, &html) {
                Ok(()) => success = true,
                Err(err) => warn!("Email notification failed: {}", err),
            }
        }

        if let Some(notifier) = &self.pushgo {
            let markdown = format_pushgo_markdown(subject, message);
            match notifier.send(subject, &markdown) {
                Ok(()) => success = true,
                Err(err) => warn!("Pushgo notification failed: {}", err),
            }
        }

        success
    }

    /// Send upload completion notification
    pub fn send_upload_result(&self, server_location: &str, date_str: &str, result: &UploadResult) {
        if result.attempts.is_empty() {
            return;
        }

        let result_str = result
            .attempts
            .iter()
            .map(|attempt| {
                if attempt.success {
                    format!("{}: 成功", attempt.name)
                } else if let Some(error) = &attempt.error {
                    format!("{}: 失败 ({})", attempt.name, error)
                } else {
                    format!("{}: 失败", attempt.name)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let (subject, message) = if result.overall_success {
            (
                format!("[{}] 数据上传完成 - {}", server_location, date_str),
                format!(
                    "{}数据上传完成\n服务器: {}\n{}",
                    date_str, server_location, result_str
                ),
            )
        } else {
            (
                format!("[{}] 数据上传部分失败 - {}", server_location, date_str),
                format!(
                    "{}数据上传部分失败\n服务器: {}\n{}",
                    date_str, server_location, result_str
                ),
            )
        };

        self.send(&subject, &message);
    }

    /// Send archive failure notification
    pub fn send_archive_failure(&self, server_location: &str, date_str: &str, error: &str) {
        let subject = format!("[{}] 数据打包失败 - {}", server_location, date_str);
        let message = format!(
            "{}数据打包失败\n服务器: {}\n错误: {}",
            date_str, server_location, error
        );
        self.send(&subject, &message);
    }

}

fn format_chanify_message(subject: &str, message: &str) -> String {
    let mut formatted = message.to_string();

    if subject.contains("失败") {
        formatted = format!("❌ {}", formatted);
    } else if subject.contains("完成") || subject.contains("成功") {
        formatted = format!("✅ {}", formatted);
    } else {
        formatted = format!("📢 {}", formatted);
    }

    formatted = formatted.replace('\n', "\n\n");
    formatted = formatted.replace("服务器:", "🖥️ 服务器:");
    formatted = formatted.replace("成功", "✅ 成功");
    formatted = formatted.replace("失败", "❌ 失败");

    formatted
}

fn format_email_html(subject: &str, message: &str) -> String {
    let (theme_color, status_icon) = if subject.contains("失败") {
        ("#FF5252", "❌")
    } else if subject.contains("完成") || subject.contains("成功") {
        ("#4CAF50", "✅")
    } else {
        ("#2196F3", "ℹ️")
    };

    let lines: Vec<&str> = message.lines().collect();
    let mut date_str = String::new();
    let mut server_location = String::new();
    let mut results = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }

        if line.contains("数据") && (line.contains("上传") || line.contains("打包")) {
            date_str = line.to_string();
        } else if line.contains("服务器:") {
            server_location = line.replace("服务器:", "").trim().to_string();
        } else if line.contains(':')
            && (line.contains("成功") || line.contains("失败"))
            && let Some((service, status)) = line.split_once(':')
        {
            let status_class = if status.contains("成功") {
                "status-success"
            } else {
                "status-fail"
            };
            let status_icon = if status.contains("成功") {
                "✅"
            } else {
                "❌"
            };
            results.push((
                service.trim().to_string(),
                status.trim().to_string(),
                status_class,
                status_icon,
            ));
        }
    }

    let mut html = format!(
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
            background-color: {}; color: white;
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
            <h1>{} {}</h1>
        </div>
        <div class="content">"#,
        theme_color, status_icon, subject
    );

    if !date_str.is_empty() {
        html.push_str(&format!("<p><strong>{}</strong></p>", date_str));
    }

    if !server_location.is_empty() {
        html.push_str(&format!(
            "<p>🖥️ <strong>服务器:</strong> {}</p>",
            server_location
        ));
    }

    if !results.is_empty() {
        html.push_str("<h3>上传结果</h3><table><tr><th>服务</th><th>状态</th></tr>");
        for (service, status, status_class, status_icon) in results {
            html.push_str(&format!(
                "<tr><td>{}</td><td class=\"{}\">{} {}</td></tr>",
                service, status_class, status_icon, status
            ));
        }
        html.push_str("</table>");
    }

    html.push_str(
        r#"
        </div>
        <div class="footer">
            <p>此邮件由系统自动生成，请勿回复</p>
            <p>© 2025 云存储上传系统</p>
        </div>
    </div>
</body>
</html>"#,
    );

    html
}

fn format_pushgo_markdown(subject: &str, message: &str) -> String {
    let mut date_str = String::new();
    let mut server_location = String::new();
    let mut results = Vec::new();
    let mut other_lines = Vec::new();

    for line in message.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line.contains("数据") && (line.contains("上传") || line.contains("打包")) {
            date_str = line.to_string();
        } else if line.starts_with("服务器:") {
            server_location = line.replace("服务器:", "").trim().to_string();
        } else if line.contains(':') && (line.contains("成功") || line.contains("失败")) {
            if let Some((service, status)) = line.split_once(':') {
                results.push((service.trim().to_string(), status.trim().to_string()));
            }
        } else {
            other_lines.push(line.to_string());
        }
    }

    let mut body = String::new();

    if !date_str.is_empty() {
        body.push_str(&format!("> {}\n\n", date_str));
    }

    if !server_location.is_empty() {
        body.push_str(&format!("- **服务器**: {}\n", server_location));
    }

    let status_label = if subject.contains("失败") {
        "失败"
    } else if subject.contains("完成") || subject.contains("成功") {
        "成功"
    } else {
        "通知"
    };
    body.push_str(&format!("- **状态**: {}\n", status_label));

    if !results.is_empty() {
        body.push_str("\n## 上传结果\n\n| 服务 | 状态 |\n| --- | --- |\n");
        for (service, status) in results {
            body.push_str(&format!("| {} | {} |\n", service, status));
        }
    }

    if !other_lines.is_empty() {
        body.push_str("\n---\n\n**补充信息**\n\n");
        for line in other_lines {
            body.push_str(&format!("- {}\n", line));
        }
    }

    body
}
