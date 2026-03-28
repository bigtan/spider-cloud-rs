#![forbid(unsafe_code)]

pub type Result<T> = anyhow::Result<T>;

pub mod logging;
pub mod notify;
pub mod uploader;
