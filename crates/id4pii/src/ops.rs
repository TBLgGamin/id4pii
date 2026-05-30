use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::cli::{ModelArgs, Style};
use crate::{PiiSpan, Rng, Vault, model_setup, redact};

#[derive(Args, Debug)]
pub(crate) struct ScanArgs {
    text: Option<String>,
    #[arg(short, long)]
    file: Option<PathBuf>,
    #[arg(long)]
    redact: bool,
    #[arg(long, value_enum, default_value_t = Style::Label)]
    style: Style,
    #[arg(long, value_enum, default_value_t = Format::Json)]
    format: Format,
    #[command(flatten)]
    model: ModelArgs,
}

#[derive(Args, Debug)]
pub(crate) struct AnonymizeArgs {
    text: Option<String>,
    #[arg(short, long)]
    file: Option<PathBuf>,
    #[arg(long)]
    seed: Option<u64>,
    #[arg(long)]
    vault_out: Option<PathBuf>,
    #[command(flatten)]
    model: ModelArgs,
}

#[derive(Args, Debug)]
pub(crate) struct DeanonymizeArgs {
    text: Option<String>,
    #[arg(short, long)]
    file: Option<PathBuf>,
    #[arg(long)]
    vault: PathBuf,
}

#[derive(Clone, Copy, ValueEnum, Debug)]
enum Format {
    Json,
    Text,
}

#[derive(Serialize)]
struct AnonymizeOutput {
    anonymized: String,
    vault: Vault,
}

pub(crate) fn scan(args: &ScanArgs) -> Result<()> {
    let text = read_input(args.text.as_ref(), args.file.as_ref())?;
    let mut detector = model_setup::load_detector(
        &args.model.model,
        &args.model.model_file,
        args.model.threads,
    )?;
    let spans = detector
        .detect(&text, args.model.min_score)
        .context("detection failed")?;

    if args.redact {
        println!("{}", redact(&text, &spans, args.style.into()));
        return Ok(());
    }

    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&spans)?),
        Format::Text => print_text(&spans),
    }
    Ok(())
}

pub(crate) fn anonymize(args: &AnonymizeArgs) -> Result<()> {
    let text = read_input(args.text.as_ref(), args.file.as_ref())?;
    let mut detector = model_setup::load_detector(
        &args.model.model,
        &args.model.model_file,
        args.model.threads,
    )?;
    let spans = detector
        .detect(&text, args.model.min_score)
        .context("detection failed")?;

    let mut rng = args.seed.map_or_else(Rng::from_entropy, Rng::new);
    let (anonymized, vault) = crate::anonymize(&text, &spans, &mut rng);

    if let Some(path) = &args.vault_out {
        std::fs::write(path, serde_json::to_string_pretty(&vault)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("{anonymized}");
    } else {
        let output = AnonymizeOutput { anonymized, vault };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    Ok(())
}

pub(crate) fn deanonymize(args: &DeanonymizeArgs) -> Result<()> {
    let text = read_input(args.text.as_ref(), args.file.as_ref())?;
    let vault_text = std::fs::read_to_string(&args.vault)
        .with_context(|| format!("failed to read {}", args.vault.display()))?;
    let vault: Vault = serde_json::from_str(&vault_text).context("invalid vault file")?;
    println!("{}", crate::deanonymize(&text, &vault));
    Ok(())
}

fn read_input(text: Option<&String>, file: Option<&PathBuf>) -> Result<String> {
    if let Some(text) = text {
        return Ok(text.clone());
    }
    if let Some(path) = file {
        return std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()));
    }
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .context("failed to read stdin")?;
    Ok(buffer)
}

fn print_text(spans: &[PiiSpan]) {
    if spans.is_empty() {
        println!("no PII detected");
        return;
    }
    for span in spans {
        println!(
            "{:<16} [{}..{}] score={:.3}  {}",
            span.category.as_str(),
            span.start,
            span.end,
            span.score,
            span.text
        );
    }
}
