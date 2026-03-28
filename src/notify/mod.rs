use crate::Result;

pub trait Notifier: Send + Sync {
    fn name(&self) -> &str;
    fn send(&self, subject: &str, message: &str) -> Result<()>;
}

pub struct NotificationManager {
    services: Vec<Box<dyn Notifier>>,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
        }
    }

    pub fn add<N>(&mut self, notifier: N) -> &mut Self
    where
        N: Notifier + 'static,
    {
        self.services.push(Box::new(notifier));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    pub fn send(&self, subject: &str, message: &str) -> Result<NotificationResult> {
        if self.services.is_empty() {
            return Ok(NotificationResult::empty());
        }

        let mut attempts = Vec::with_capacity(self.services.len());
        for service in &self.services {
            let channel = service.name().to_string();
            match service.send(subject, message) {
                Ok(()) => attempts.push(NotificationAttempt::success(channel)),
                Err(err) => attempts.push(NotificationAttempt::failure(channel, err.to_string())),
            }
        }
        Ok(NotificationResult::from_attempts(attempts))
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAttempt {
    pub channel: String,
    pub success: bool,
    pub error: Option<String>,
}

impl NotificationAttempt {
    pub fn success(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            success: true,
            error: None,
        }
    }

    pub fn failure(channel: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            success: false,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationResult {
    pub overall_success: bool,
    pub attempts: Vec<NotificationAttempt>,
}

impl NotificationResult {
    pub fn empty() -> Self {
        Self {
            overall_success: false,
            attempts: Vec::new(),
        }
    }

    pub fn from_attempts(attempts: Vec<NotificationAttempt>) -> Self {
        let overall_success =
            !attempts.is_empty() && attempts.iter().all(|attempt| attempt.success);
        Self {
            overall_success,
            attempts,
        }
    }
}

pub mod chanify;

pub mod email;

pub mod pushgo;

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeNotifier {
        succeed: bool,
    }

    impl Notifier for FakeNotifier {
        fn name(&self) -> &str {
            "FakeNotifier"
        }

        fn send(&self, _subject: &str, _message: &str) -> Result<()> {
            if self.succeed {
                Ok(())
            } else {
                anyhow::bail!("send failed");
            }
        }
    }

    #[test]
    fn manager_empty_returns_false() {
        let manager = NotificationManager::new();
        let result = manager.send("sub", "msg").unwrap();
        assert!(!result.overall_success);
        assert!(result.attempts.is_empty());
    }

    #[test]
    fn manager_aggregates_success() {
        let mut manager = NotificationManager::new();
        manager.add(FakeNotifier { succeed: false });
        manager.add(FakeNotifier { succeed: true });
        let result = manager.send("sub", "msg").unwrap();
        assert!(!result.overall_success);
        assert_eq!(result.attempts.len(), 2);
        assert!(!result.attempts[0].success);
        assert!(result.attempts[1].success);
    }
}
