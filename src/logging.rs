use anyhow::Context;
use chrono::Local;
use std::fs::OpenOptions;
use std::io::BufWriter;
use tracing::Level;
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_default(debug: bool) -> anyhow::Result<()> {
    let log_level = if debug { Level::DEBUG } else { Level::INFO };
    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .try_init()
        .map_err(|err| anyhow::anyhow!("Failed to initialize tracing subscriber: {}", err))
}

pub fn init_with_file(prefix: &str, debug: bool) -> anyhow::Result<String> {
    let log_level = if debug { "debug" } else { "info" };

    if !std::path::Path::new("logs").exists() {
        std::fs::create_dir_all("logs").context("Failed to create logs directory")?;
    }

    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let log_file_path = format!("logs/{prefix}_{timestamp}.log");

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("Failed to open log file: {log_file_path}"))?;
    let file_writer = BufWriter::new(file);

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_filter(EnvFilter::from_default_env().add_directive(log_level.parse()?));

    let file_layer = fmt::layer()
        .with_writer(BoxMakeWriter::new(move || {
            file_writer
                .get_ref()
                .try_clone()
                .map(BufWriter::new)
                .unwrap_or_else(|_| {
                    BufWriter::new(std::fs::File::create("logs/fallback.log").unwrap())
                })
        }))
        .with_ansi(false)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_filter(EnvFilter::from_default_env().add_directive(log_level.parse()?));

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .try_init()
        .map_err(|err| anyhow::anyhow!("Failed to initialize tracing subscriber: {}", err))?;

    Ok(log_file_path)
}
