use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use id4pii_core::model_dir;
use id4pii_core::model_fetch::{self, FetchProgress};
use indicatif::{ProgressBar, ProgressStyle};

pub(crate) fn ensure_model(dir: &Path, model_file: &str) -> Result<()> {
    if model_dir::is_complete(dir, model_file) {
        return Ok(());
    }
    eprintln!(
        "id4pii: model not found at {}. Downloading from openai/privacy-filter on Hugging Face …",
        dir.display()
    );
    let mut progress = CliProgress::default();
    model_fetch::ensure_present(dir, model_file, &mut progress)
        .with_context(|| format!("downloading model into {}", dir.display()))?;
    eprintln!("id4pii: model ready.");
    Ok(())
}

#[derive(Default)]
struct CliProgress {
    bar: Option<ProgressBar>,
}

impl FetchProgress for CliProgress {
    fn on_start(&mut self, file: &str, total: Option<u64>) {
        let bar = if let Some(t) = total {
            let pb = ProgressBar::new(t);
            pb.set_style(
                ProgressStyle::with_template(
                    "  {msg:<40} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta})",
                )
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=>-"),
            );
            pb
        } else {
            let pb = ProgressBar::new_spinner();
            pb.enable_steady_tick(Duration::from_millis(120));
            pb.set_style(
                ProgressStyle::with_template("  {spinner} {msg} {bytes}")
                    .unwrap_or_else(|_| ProgressStyle::default_spinner()),
            );
            pb
        };
        bar.set_message(file.to_string());
        self.bar = Some(bar);
    }

    fn on_chunk(&mut self, _file: &str, written: u64, _total: Option<u64>) {
        if let Some(bar) = self.bar.as_ref() {
            bar.set_position(written);
        }
    }

    fn on_finish(&mut self, _file: &str, _written: u64) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }

    fn on_skip(&mut self, file: &str, size: u64) {
        eprintln!("  {file} already present ({size} bytes)");
    }
}
