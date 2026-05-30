use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_FILTER: &str = "info,ort=warn";

pub fn init_cli() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(crate::progress::AdaptiveWriter)
        .init();
}

pub fn init_guard(also_stderr: bool) -> Result<PathBuf> {
    let dir =
        crate::paths::log_dir().context("could not resolve the id4pii data directory for logs")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("guard")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .context("creating rolling file appender")?;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let file_layer = fmt::layer().with_writer(appender).with_ansi(false);
    let registry = tracing_subscriber::registry().with(filter).with(file_layer);

    if also_stderr {
        registry
            .with(fmt::layer().with_writer(std::io::stderr))
            .try_init()
            .context("global tracing subscriber already set")?;
    } else {
        registry
            .try_init()
            .context("global tracing subscriber already set")?;
    }

    Ok(dir)
}
