use crate::Result;

pub trait Notifier: Send + Sync {
    fn name(&self) -> &str;
    fn send(&self, subject: &str, message: &str) -> Result<()>;
}

pub mod chanify;

pub mod email;

pub mod pushgo;
