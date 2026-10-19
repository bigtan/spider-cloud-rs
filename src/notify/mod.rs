use crate::Result;

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

pub mod chanify;

pub mod email;

pub mod pushgo;

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escape_html_replaces_special_characters() {
        assert_eq!(
            escape_html("<html>502 & \"Bad\" 'Gateway'</html>"),
            "&lt;html&gt;502 &amp; &quot;Bad&quot; &#39;Gateway&#39;&lt;/html&gt;"
        );
    }
}
