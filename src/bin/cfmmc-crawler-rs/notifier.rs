use indexmap::IndexMap;
use std::collections::HashMap;
use tracing::{debug, error, info};

use spider_cloud_rs::notify;
use spider_cloud_rs::notify::Notifier as EsNotifier;

pub trait Notifier {
    fn send(&self, date_str: &str, accounts_data: &IndexMap<String, HashMap<String, f64>>) -> bool;
}

pub struct ChanifyNotifier {
    pub chanify_url: String,
    pub chanify_token: String,
}

impl Notifier for ChanifyNotifier {
    fn send(&self, date_str: &str, accounts_data: &IndexMap<String, HashMap<String, f64>>) -> bool {
        info!("Sending Chanify notification for date: {}", date_str);

        let text = build_chanify_message(date_str, accounts_data);
        let subject = format!("CFMMC 账户数据 - {}", date_str);
        let notifier = notify::chanify::ChanifyNotifier::new(
            self.chanify_url.clone(),
            self.chanify_token.clone(),
        );

        match notifier.send(&subject, &text) {
            Ok(()) => true,
            Err(e) => {
                error!("Error sending Chanify notification: {}", e);
                false
            }
        }
    }
}

pub struct QQEmailNotifier {
    pub sender: String,
    pub password: String,
    pub recipient: String,
}

impl Notifier for QQEmailNotifier {
    fn send(&self, date_str: &str, accounts_data: &IndexMap<String, HashMap<String, f64>>) -> bool {
        info!("Sending QQ Email notification for date: {}", date_str);

        let html = build_email_html(date_str, accounts_data);
        let subject = format!("CFMMC 账户数据汇总 - {}", date_str);
        let notifier = notify::email::EmailNotifier::new(
            self.sender.clone(),
            self.password.clone(),
            self.recipient.clone(),
        );

        match notifier.send_html(&subject, &html) {
            Ok(()) => true,
            Err(e) => {
                error!("Error sending QQ Email notification: {}", e);
                false
            }
        }
    }
}

pub struct PushgoNotifier {
    pub api_token: String,
    pub url: String,
    pub channel_id: String,
    pub password: String,
    pub hex_key: String,
    pub icon: Option<String>,
    pub image: Option<String>,
}

impl Notifier for PushgoNotifier {
    fn send(&self, date_str: &str, accounts_data: &IndexMap<String, HashMap<String, f64>>) -> bool {
        info!("Sending Pushgo notification for date: {}", date_str);

        let markdown = build_pushgo_markdown(date_str, accounts_data);
        let subject = format!("CFMMC 账户数据 - {}", date_str);
        let notifier = notify::pushgo::PushgoNotifier::new(
            self.url.clone(),
            self.api_token.clone(),
            self.hex_key.clone(),
            self.channel_id.clone(),
            self.password.clone(),
            self.icon.clone(),
            self.image.clone(),
        );

        match notifier.send(&subject, &markdown) {
            Ok(()) => true,
            Err(e) => {
                error!("Error sending Pushgo notification: {}", e);
                false
            }
        }
    }
}

fn build_chanify_message(
    date_str: &str,
    accounts_data: &IndexMap<String, HashMap<String, f64>>,
) -> String {
    let mut text = format!("📅 {date_str}\n════════════════════\n");
    for (account, data) in accounts_data {
        debug!("Processing account data: {}", account);

        let equity = data.get("客户权益").unwrap_or(&0.0);
        let closed_pnl = data.get("平仓盈亏").unwrap_or(&0.0);
        let float_pnl = data.get("浮动盈亏").unwrap_or(&0.0);
        let risk = data.get("风险度").unwrap_or(&0.0);
        let fee = data.get("交易手续费").unwrap_or(&0.0);

        let closed_pnl_prefix = if *closed_pnl > 0.0 {
            "📈"
        } else if *closed_pnl < 0.0 {
            "📉"
        } else {
            "⏸️"
        };
        let float_pnl_prefix = if *float_pnl > 0.0 {
            "📈"
        } else if *float_pnl < 0.0 {
            "📉"
        } else {
            "⏸️"
        };
        let risk_prefix = if *risk < 30.0 {
            "🟢"
        } else if *risk < 70.0 {
            "🟠"
        } else {
            "🔴"
        };

        text.push_str(&format!(
            "👤 账户: {account}\n💰 客户权益: {equity:.2}\n{closed_pnl_prefix} 平仓盈亏: {closed_pnl:.2}\n{float_pnl_prefix} 浮动盈亏: {float_pnl:.2}\n{risk_prefix} 风险度: {risk:.2}%\n💸 交易手续费: {fee:.2}\n────────────────────\n"
        ));
    }
    text
}

fn build_pushgo_markdown(
    date_str: &str,
    accounts_data: &IndexMap<String, HashMap<String, f64>>,
) -> String {
    let mut body = format!("> {date_str}\n\n");
    for (account, data) in accounts_data {
        let equity = data.get("客户权益").unwrap_or(&0.0);
        let closed_pnl = data.get("平仓盈亏").unwrap_or(&0.0);
        let float_pnl = data.get("浮动盈亏").unwrap_or(&0.0);
        let risk = data.get("风险度").unwrap_or(&0.0);
        let fee = data.get("交易手续费").unwrap_or(&0.0);

        body.push_str(&format!(
            "## 账户 {account}\n\n| 项目 | 数值 |\n| --- | --- |\n| 客户权益 | ￥{equity:.2} |\n| 平仓盈亏 | ￥{closed_pnl:.2} |\n| 浮动盈亏 | ￥{float_pnl:.2} |\n| 风险度 | {risk:.2}% |\n| 交易手续费 | ￥{fee:.2} |\n\n"
        ));
    }
    body
}

fn build_email_html(
    date_str: &str,
    accounts_data: &IndexMap<String, HashMap<String, f64>>,
) -> String {
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

    let mut content = format!("{css_styles}<h2>CFMMC 账户数据汇总 - {date_str}</h2>");
    for (account, data) in accounts_data {
        let equity = data.get("客户权益").unwrap_or(&0.0);
        let closed_pnl = data.get("平仓盈亏").unwrap_or(&0.0);
        let float_pnl = data.get("浮动盈亏").unwrap_or(&0.0);
        let risk = data.get("风险度").unwrap_or(&0.0);
        let fee = data.get("交易手续费").unwrap_or(&0.0);

        let closed_pnl_class = if *closed_pnl > 0.0 {
            "positive"
        } else if *closed_pnl < 0.0 {
            "negative"
        } else {
            "neutral"
        };
        let float_pnl_class = if *float_pnl > 0.0 {
            "positive"
        } else if *float_pnl < 0.0 {
            "negative"
        } else {
            "neutral"
        };
        let risk_class = if *risk < 30.0 {
            "risk-low"
        } else if *risk < 70.0 {
            "risk-medium"
        } else {
            "risk-high"
        };

        content.push_str(&format!(
            "<div class=\"summary-card\"><h3>账户: {account}</h3><table class=\"data-table\"><tr><th>项目</th><th>数值</th></tr><tr><td>客户权益</td><td>￥{equity:.2}</td></tr><tr><td>平仓盈亏</td><td class=\"{closed_pnl_class}\">￥{closed_pnl:.2}</td></tr><tr><td>浮动盈亏</td><td class=\"{float_pnl_class}\">￥{float_pnl:.2}</td></tr><tr><td>风险度</td><td class=\"{risk_class}\">{risk:.2}%</td></tr><tr><td>交易手续费</td><td>￥{fee:.2}</td></tr></table></div>"
        ));
    }

    content
}
