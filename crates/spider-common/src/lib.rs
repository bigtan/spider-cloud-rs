#![forbid(unsafe_code)]

pub type Result<T> = anyhow::Result<T>;

#[cfg(feature = "archive")]
pub mod archive;
pub mod cli;
pub mod fs;
pub mod logging;
pub mod retry;
pub mod template;
