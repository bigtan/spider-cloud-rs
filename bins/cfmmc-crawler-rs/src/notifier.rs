use indexmap::IndexMap;
use spider_notify::{NotificationPayload, escape_html};
use std::collections::HashMap;
use tracing::debug;

/// Per-account settlement values; `None` means extraction failed or no
/// settlement data was published, which must not be rendered as zeros.
pub type AccountsData = IndexMap<String, Option<HashMap<String, f64>>>;

struct SettlementMetrics {
    equity: f64,
    closed_pnl: f64,
    float_pnl: f64,
    risk: f64,
    fee: f64,
}

impl SettlementMetrics {
    fn from_map(data: &HashMap<String, f64>) -> Self {
        Self {
            equity: *data.get("客户权益").unwrap_or(&0.0),
            closed_pnl: *data.get("平仓盈亏").unwrap_or(&0.0),
            float_pnl: *data.get("浮动盈亏").unwrap_or(&0.0),
            risk: *data.get("风险度").unwrap_or(&0.0),
            fee: *data.get("交易手续费").unwrap_or(&0.0),
        }
    }
}

pub fn build_notification_payload(
    date_str: &str,
    accounts_data: &AccountsData,
) -> NotificationPayload {
    NotificationPayload {
        subject: format!("CFMMC 账户数据 - {date_str}"),
        plain_text: build_chanify_message(date_str, accounts_data),
        html_body: Some(build_email_html(date_str, accounts_data)),
        markdown_body: Some(build_pushgo_markdown(date_str, accounts_data)),
    }
}

fn pnl_emoji(value: f64) -> &'static str {
    if value > 0.0 {
        "📈"
    } else if value < 0.0 {
        "📉"
    } else {
        "⏸️"
    }
}

fn risk_emoji(risk: f64) -> &'static str {
    if risk < 30.0 {
        "🟢"
    } else if risk < 70.0 {
        "🟠"
    } else {
        "🔴"
    }
}

fn pnl_css_class(value: f64) -> &'static str {
    if value > 0.0 {
        "positive"
    } else if value < 0.0 {
        "negative"
    } else {
        "neutral"
    }
}

fn risk_css_class(risk: f64) -> &'static str {
    if risk < 30.0 {
        "risk-low"
    } else if risk < 70.0 {
        "risk-medium"
    } else {
        "risk-high"
    }
}

pub fn build_chanify_message(date_str: &str, accounts_data: &AccountsData) -> String {
    let mut text = format!("📅 {date_str}\n════════════════════\n");
    for (account, data) in accounts_data {
        debug!("Formatting Chanify report for account: {}", account);

        let Some(data) = data else {
            text.push_str(&format!(
                "👤 账户: {account}\n⚠️ 未获取到结算数据\n────────────────────\n"
            ));
            continue;
        };

        let m = SettlementMetrics::from_map(data);
        let closed_pnl_prefix = pnl_emoji(m.closed_pnl);
        let float_pnl_prefix = pnl_emoji(m.float_pnl);
        let risk_prefix = risk_emoji(m.risk);

        text.push_str(&format!(
            "👤 账户: {account}\n💰 客户权益: {:.2}\n{closed_pnl_prefix} 平仓盈亏: {:.2}\n{float_pnl_prefix} 浮动盈亏: {:.2}\n{risk_prefix} 风险度: {:.2}%\n💸 交易手续费: {:.2}\n────────────────────\n",
            m.equity, m.closed_pnl, m.float_pnl, m.risk, m.fee
        ));
    }
    text
}

pub fn build_pushgo_markdown(date_str: &str, accounts_data: &AccountsData) -> String {
    let mut body = format!("> {date_str}\n\n");
    for (account, data) in accounts_data {
        let Some(data) = data else {
            body.push_str(&format!("## 账户 {account}\n\n⚠️ 未获取到结算数据\n\n"));
            continue;
        };

        let m = SettlementMetrics::from_map(data);
        body.push_str(&format!(
            "## 账户 {account}\n\n| 项目 | 数值 |\n| --- | --- |\n| 客户权益 | ￥{:.2} |\n| 平仓盈亏 | ￥{:.2} |\n| 浮动盈亏 | ￥{:.2} |\n| 风险度 | {:.2}% |\n| 交易手续费 | ￥{:.2} |\n\n",
            m.equity, m.closed_pnl, m.float_pnl, m.risk, m.fee
        ));
    }
    body
}

pub fn build_email_html(date_str: &str, accounts_data: &AccountsData) -> String {
    let css_styles = r#"
    <style>
        body { font-family: Arial, sans-serif; margin: 20px; color: #333; }
        h2 { color: #2c3e50; border-bottom: 2px solid #3498db; padding-bottom: 10px; }
        h3 { color: #2980b9; margin-top: 20px; }
        .summary-card { 
            background-color: #f8f9fa; 
            border-radius: 8px; 
            box-shadow: 0 4px 6px rgba(0, 0, 0, 0.1); 
            padding: 15px; 
            margin-bottom: 20px; 
        }
        .data-table { 
            width: 100%; 
            border-collapse: collapse; 
            border-radius: 8px; 
            overflow: hidden;
            box-shadow: 0 2px 3px rgba(0, 0, 0, 0.1);
        }
        .data-table th, .data-table td { 
            padding: 12px 15px; 
            text-align: left; 
            border-bottom: 1px solid #e0e0e0; 
        }
        .data-table th { 
            background-color: #3498db; 
            color: white; 
        }
        .data-table tr:nth-child(even) { 
            background-color: #f8f9fa; 
        }
        .data-table tr:hover { 
            background-color: #e8f4f8; 
        }
        .positive { color: #27ae60; }
        .negative { color: #e74c3c; }
        .neutral { color: #7f8c8d; }
        .risk-low { color: #27ae60; }
        .risk-medium { color: #f39c12; }
        .risk-high { color: #e74c3c; }
    </style>
    "#;

    let escaped_date = escape_html(date_str);
    let mut content = format!("{css_styles}<h2>CFMMC 账户数据汇总 - {escaped_date}</h2>");
    for (account, data) in accounts_data {
        let escaped_account = escape_html(account);
        let Some(data) = data else {
            content.push_str(&format!(
                "<div class=\"summary-card\"><h3>账户: {escaped_account}</h3><p class=\"negative\">⚠️ 未获取到结算数据</p></div>"
            ));
            continue;
        };

        let m = SettlementMetrics::from_map(data);
        let closed_pnl_class = pnl_css_class(m.closed_pnl);
        let float_pnl_class = pnl_css_class(m.float_pnl);
        let risk_class = risk_css_class(m.risk);

        content.push_str(&format!(
            "<div class=\"summary-card\"><h3>账户: {escaped_account}</h3><table class=\"data-table\"><tr><th>项目</th><th>数值</th></tr><tr><td>客户权益</td><td>￥{:.2}</td></tr><tr><td>平仓盈亏</td><td class=\"{closed_pnl_class}\">￥{:.2}</td></tr><tr><td>浮动盈亏</td><td class=\"{float_pnl_class}\">￥{:.2}</td></tr><tr><td>风险度</td><td class=\"{risk_class}\">{:.2}%</td></tr><tr><td>交易手续费</td><td>￥{:.2}</td></tr></table></div>",
            m.equity, m.closed_pnl, m.float_pnl, m.risk, m.fee
        ));
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_email_html_escapes_account_and_date() {
        let mut data: AccountsData = IndexMap::new();
        let mut metrics = HashMap::new();
        metrics.insert("客户权益".to_string(), 10_000.5);
        metrics.insert("平仓盈亏".to_string(), 200.0);
        metrics.insert("浮动盈亏".to_string(), -50.25);
        metrics.insert("风险度".to_string(), 15.0);
        metrics.insert("交易手续费".to_string(), 12.34);

        data.insert("<script>alert(1)</script>".to_string(), Some(metrics));
        data.insert("acc&\"2\"".to_string(), None);

        let html = build_email_html("2026-01-15<img src=x>", &data);
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains("2026-01-15<img src=x>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("2026-01-15&lt;img src=x&gt;"));
        assert!(html.contains("acc&amp;&quot;2&quot;"));
        assert!(html.contains("10000.50"));
        assert!(html.contains("15.00%"));
    }

    #[test]
    fn build_notification_payload_populates_all_formats() {
        let mut data: AccountsData = IndexMap::new();
        data.insert("acc_01".to_string(), None);

        let payload = build_notification_payload("2026-01-15", &data);
        assert_eq!(payload.subject, "CFMMC 账户数据 - 2026-01-15");
        assert!(payload.plain_text.contains("acc_01"));
        assert!(payload.html_body.as_deref().unwrap().contains("acc_01"));
        assert!(payload.markdown_body.as_deref().unwrap().contains("acc_01"));
    }
}
