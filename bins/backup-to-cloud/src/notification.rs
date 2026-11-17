use anyhow::Result;
use spider_notify::{NotificationDispatcher, NotificationPayload, escape_html, render_email_shell};
use spider_uploader::UploadAttempt;
use tracing::warn;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct BackupItemOutcome {
    pub archive_name: String,
    pub remote_dir: String,
    pub stage_error: Option<String>,
    pub attempts: Vec<UploadAttempt>,
}

impl BackupItemOutcome {
    pub fn succeeded(&self) -> bool {
        self.stage_error.is_none()
            && !self.attempts.is_empty()
            && self.attempts.iter().all(|attempt| attempt.success)
    }
}

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

    pub fn send_backup_result(
        &self,
        server_location: &str,
        date_str: &str,
        outcomes: &[BackupItemOutcome],
    ) {
        if outcomes.is_empty() || !self.dispatcher.has_channels() {
            return;
        }

        let overall_success = outcomes.iter().all(BackupItemOutcome::succeeded);
        let report = BackupReport {
            server_location,
            date_str,
            overall_success,
            outcomes,
        };

        let subject = format!(
            "[{}] 备份{} - {}",
            server_location,
            report.status_label(),
            date_str
        );
        let payload = NotificationPayload {
            subject: subject.clone(),
            plain_text: chanify_backup_message(&report),
            html_body: Some(email_backup_html(&subject, &report)),
            markdown_body: Some(pushgo_backup_markdown(&report)),
        };

        for attempt in self.dispatcher.dispatch(&payload) {
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

struct BackupReport<'a> {
    server_location: &'a str,
    date_str: &'a str,
    overall_success: bool,
    outcomes: &'a [BackupItemOutcome],
}

impl BackupReport<'_> {
    fn status_label(&self) -> &'static str {
        if self.overall_success {
            "完成"
        } else if self.outcomes.iter().any(BackupItemOutcome::succeeded) {
            "部分失败"
        } else {
            "失败"
        }
    }

    fn headline(&self) -> String {
        format!("{}备份{}", self.date_str, self.status_label())
    }
}

fn outcome_summary(outcome: &BackupItemOutcome) -> String {
    if let Some(err) = &outcome.stage_error {
        return format!("失败 ({err})");
    }
    if outcome.attempts.is_empty() {
        return "未执行上传".to_string();
    }
    let parts: Vec<String> = outcome
        .attempts
        .iter()
        .map(|attempt| {
            if attempt.success {
                format!("{}: 成功", attempt.name)
            } else if let Some(err) = &attempt.error {
                format!("{}: 失败 ({err})", attempt.name)
            } else {
                format!("{}: 失败", attempt.name)
            }
        })
        .collect();
    parts.join("; ")
}

fn chanify_backup_message(report: &BackupReport<'_>) -> String {
    let icon = if report.overall_success { "✅" } else { "❌" };
    let mut text = format!(
        "{icon} {}\n\n🖥️ 服务器: {}\n",
        report.headline(),
        report.server_location
    );
    for outcome in report.outcomes {
        let item_icon = if outcome.succeeded() { "✅" } else { "❌" };
        text.push_str(&format!(
            "\n{item_icon} {} ({}): {}\n",
            outcome.archive_name,
            outcome.remote_dir,
            outcome_summary(outcome)
        ));
    }
    text
}

fn pushgo_backup_markdown(report: &BackupReport<'_>) -> String {
    let status = if report.overall_success {
        "成功"
    } else {
        "失败"
    };
    let mut body = format!(
        "> {}\n\n- **服务器**: {}\n- **状态**: {}\n\n## 备份任务\n\n| 任务 | 目标路径 | 结果 |\n| --- | --- | --- |\n",
        report.headline(),
        report.server_location,
        status
    );
    for outcome in report.outcomes {
        body.push_str(&format!(
            "| {} | {} | {} |\n",
            outcome.archive_name,
            outcome.remote_dir,
            outcome_summary(outcome)
        ));
    }
    body
}

fn email_backup_html(subject: &str, report: &BackupReport<'_>) -> String {
    let (theme_color, status_icon) = if report.overall_success {
        ("#4CAF50", "✅")
    } else {
        ("#FF5252", "❌")
    };

    let mut content = format!(
        "<p><strong>{}</strong></p>\
         <p>🖥️ <strong>服务器:</strong> {}</p>\
         <h3>备份结果</h3><table><tr><th>任务</th><th>目标路径</th><th>状态</th></tr>",
        escape_html(&report.headline()),
        escape_html(report.server_location)
    );
    for outcome in report.outcomes {
        let (class, icon) = if outcome.succeeded() {
            ("status-success", "✅")
        } else {
            ("status-fail", "❌")
        };
        content.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td class=\"{}\">{} {}</td></tr>",
            escape_html(&outcome.archive_name),
            escape_html(&outcome.remote_dir),
            class,
            icon,
            escape_html(&outcome_summary(outcome))
        ));
    }
    content.push_str("</table>");

    render_email_shell(subject, theme_color, status_icon, &content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_html_escapes_dynamic_values() {
        let outcomes = vec![
            BackupItemOutcome {
                archive_name: "db<1>".to_string(),
                remote_dir: "/backups/&test".to_string(),
                stage_error: Some("<script>alert(1)</script>".to_string()),
                attempts: vec![],
            },
            BackupItemOutcome {
                archive_name: "app".to_string(),
                remote_dir: "/backups/app".to_string(),
                stage_error: None,
                attempts: vec![UploadAttempt::success("BaiduPan")],
            },
        ];
        let report = BackupReport {
            server_location: "Srv & <Prod>",
            date_str: "20260611",
            overall_success: false,
            outcomes: &outcomes,
        };
        let html = email_backup_html("[Srv & <Prod>] 备份部分失败", &report);
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("db&lt;1&gt;"));
        assert!(html.contains("Srv &amp; &lt;Prod&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn markdown_and_chanify_include_item_details() {
        let outcomes = vec![BackupItemOutcome {
            archive_name: "app".to_string(),
            remote_dir: "/apps/backups/20260611".to_string(),
            stage_error: None,
            attempts: vec![
                UploadAttempt::success("BaiduPan"),
                UploadAttempt::failure("Cloud189", "timeout"),
            ],
        }];
        let report = BackupReport {
            server_location: "Prod",
            date_str: "20260611",
            overall_success: false,
            outcomes: &outcomes,
        };

        let text = chanify_backup_message(&report);
        assert!(text.contains("❌ 20260611备份失败"));
        assert!(text.contains("BaiduPan: 成功"));
        assert!(text.contains("Cloud189: 失败 (timeout)"));

        let md = pushgo_backup_markdown(&report);
        assert!(md.contains("| app | /apps/backups/20260611 |"));
    }
}
