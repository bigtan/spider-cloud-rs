use anyhow::Result;
use spider_notify::{NotificationDispatcher, NotificationPayload, escape_html, render_email_shell};
use spider_uploader::{UploadAttempt, UploadResult};
use tracing::warn;

use crate::config::Config;

pub struct NotificationManager {
    dispatcher: NotificationDispatcher,
}

impl NotificationManager {
    pub fn from_config(config: &Config) -> Result<Self> {
        let dispatcher = NotificationDispatcher::from_config(&config.notifier)?;
        if !dispatcher.has_channels() {
            warn!("No notification channels configured");
        }
        Ok(Self { dispatcher })
    }

    pub fn send_upload_result(&self, server_location: &str, date_str: &str, result: &UploadResult) {
        if result.attempts.is_empty() || !self.dispatcher.has_channels() {
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
        let payload = NotificationPayload {
            subject: subject.clone(),
            plain_text: chanify_upload_message(&report),
            html_body: Some(email_upload_html(&subject, &report)),
            markdown_body: Some(pushgo_upload_markdown(&report)),
        };

        self.dispatch_and_log(&payload);
    }

    pub fn send_archive_failure(&self, server_location: &str, date_str: &str, error: &str) {
        if !self.dispatcher.has_channels() {
            return;
        }

        let subject = format!("[{}] 数据打包失败 - {}", server_location, date_str);
        let headline = format!("{date_str}数据打包失败");

        let plain_text = format!("❌ {headline}\n\n🖥️ 服务器: {server_location}\n\n错误: {error}");
        let html_body = render_email_shell(
            &subject,
            "#FF5252",
            "❌",
            &format!(
                "<p><strong>{}</strong></p>\
                 <p>🖥️ <strong>服务器:</strong> {}</p>\
                 <p><strong>错误:</strong> {}</p>",
                escape_html(&headline),
                escape_html(server_location),
                escape_html(error)
            ),
        );
        let markdown_body = format!(
            "> {headline}\n\n- **服务器**: {server_location}\n- **状态**: 失败\n\n**错误**\n\n{error}\n"
        );

        let payload = NotificationPayload {
            subject,
            plain_text,
            html_body: Some(html_body),
            markdown_body: Some(markdown_body),
        };

        self.dispatch_and_log(&payload);
    }

    fn dispatch_and_log(&self, payload: &NotificationPayload) {
        for attempt in self.dispatcher.dispatch(payload) {
            if !attempt.success {
                warn!(
                    "{} notification failed: {}",
                    attempt.channel,
                    attempt.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }
}

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
        format!("失败 ({error})")
    } else {
        "失败".to_string()
    }
}

fn chanify_upload_message(report: &UploadReport<'_>) -> String {
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

fn pushgo_upload_markdown(report: &UploadReport<'_>) -> String {
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

fn email_upload_html(subject: &str, report: &UploadReport<'_>) -> String {
    let (theme_color, status_icon) = if report.success {
        ("#4CAF50", "✅")
    } else {
        ("#FF5252", "❌")
    };

    let mut content = format!(
        "<p><strong>{}</strong></p>\
         <p>🖥️ <strong>服务器:</strong> {}</p>\
         <h3>上传结果</h3><table><tr><th>服务</th><th>状态</th></tr>",
        escape_html(&report.headline()),
        escape_html(report.server_location)
    );
    for attempt in report.attempts {
        let (class, icon) = if attempt.success {
            ("status-success", "✅")
        } else {
            ("status-fail", "❌")
        };
        content.push_str(&format!(
            "<tr><td>{}</td><td class=\"{}\">{} {}</td></tr>",
            escape_html(&attempt.name),
            class,
            icon,
            escape_html(&attempt_status(attempt))
        ));
    }
    content.push_str("</table>");

    render_email_shell(subject, theme_color, status_icon, &content)
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
    fn email_html_renders_status_classes_and_escapes_html() {
        let attempts = vec![
            UploadAttempt::success("BaiduPan"),
            UploadAttempt::failure("Cloud189", "<html>502</html>"),
        ];
        let html = email_upload_html("subject <test>", &sample_report(&attempts));
        assert!(html.contains("status-success"));
        assert!(html.contains("status-fail"));
        assert!(html.contains("失败 (&lt;html&gt;502&lt;/html&gt;)"));
        assert!(html.contains("subject &lt;test&gt;"));
    }
}
