use std::io;
use std::sync::OnceLock;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

static PROGRESS_BAR: OnceLock<ProgressBar> = OnceLock::new();

/// Install (idempotently) a spinner-style progress bar pinned to the bottom of the terminal.
/// Subsequent tracing log lines go through [`AdaptiveWriter`] which prints them above the
/// bar without clobbering it. Call from a long-running command (e.g. `guard`) on startup.
pub(crate) fn install_bar() -> &'static ProgressBar {
    PROGRESS_BAR.get_or_init(|| {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.enable_steady_tick(Duration::from_millis(120));
        bar.set_message("starting…");
        bar
    })
}

/// Cleanly clear the bar from the terminal on shutdown.
pub(crate) fn finish_bar() {
    if let Some(bar) = PROGRESS_BAR.get() {
        bar.finish_and_clear();
    }
}

#[derive(Clone, Copy)]
pub(crate) struct AdaptiveWriter;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AdaptiveWriter {
    type Writer = Box<dyn io::Write + Send>;
    fn make_writer(&'a self) -> Self::Writer {
        match PROGRESS_BAR.get() {
            Some(bar) => Box::new(IndicatifWriter {
                bar: bar.clone(),
                buf: Vec::new(),
            }),
            None => Box::new(io::stderr()),
        }
    }
}

struct IndicatifWriter {
    bar: ProgressBar,
    buf: Vec<u8>,
}

impl io::Write for IndicatifWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let s = String::from_utf8_lossy(&self.buf);
            let trimmed = s.trim_end_matches('\n');
            if !trimmed.is_empty() {
                self.bar.println(trimmed);
            }
            self.buf.clear();
        }
        Ok(())
    }
}

impl Drop for IndicatifWriter {
    fn drop(&mut self) {
        let _ = <Self as io::Write>::flush(self);
    }
}
