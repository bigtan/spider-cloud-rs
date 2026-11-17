use std::collections::HashMap;

/// Placeholder key/value substitution context for paths and remote destinations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TemplateContext {
    vars: HashMap<String, String>,
}

/// Backwards-compatible alias for [`TemplateContext`].
pub type UploadContext = TemplateContext;

impl TemplateContext {
    /// Create an empty template context.
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }

    /// Create a template context pre-populated with `{date}`.
    pub fn with_date(date: impl Into<String>) -> Self {
        let mut ctx = Self::new();
        ctx.insert("date", date);
        ctx
    }

    /// Insert or overwrite a placeholder variable without braces (e.g. `"date"`).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    /// Expand `{key}` placeholders in `template`, leaving unknown or unclosed placeholders intact.
    pub fn expand(&self, template: &str) -> String {
        let mut output = String::with_capacity(template.len());
        let mut chars = template.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch != '{' {
                output.push(ch);
                continue;
            }

            let mut key = String::new();
            let mut closed = false;
            while let Some(&next) = chars.peek() {
                if next == '{' {
                    break;
                }
                chars.next();
                if next == '}' {
                    closed = true;
                    break;
                }
                key.push(next);
            }

            if !closed {
                output.push('{');
                output.push_str(&key);
            } else if key.is_empty() {
                output.push_str("{}");
            } else if let Some(value) = self.vars.get(&key) {
                output.push_str(value);
            } else {
                output.push('{');
                output.push_str(&key);
                output.push('}');
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_placeholders_and_preserves_unknown() {
        let mut ctx = UploadContext::with_date("20260611");
        ctx.insert("server_location", "Tokyo");
        assert_eq!(
            ctx.expand("/cloud/{server_location}/{date}/{missing}/"),
            "/cloud/Tokyo/20260611/{missing}/"
        );
    }

    #[test]
    fn preserves_empty_and_unclosed_braces() {
        let ctx = TemplateContext::with_date("20260611");
        assert_eq!(ctx.expand("{}/{date}/{unclosed"), "{}/20260611/{unclosed");
        assert_eq!(ctx.expand("{prefix {date}}"), "{prefix 20260611}");
    }
}
