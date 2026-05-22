#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_possible_truncation
)]

#[cfg(windows)]
mod guard;
mod serve;

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use id4pii_core::{Detector, PiiSpan, RedactStyle, Rng, Vault, anonymize, deanonymize, redact};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "id4pii",
    version,
    about = "Detect, redact and reversibly anonymize PII in text"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Scan(ScanArgs),
    Anonymize(AnonymizeArgs),
    Deanonymize(DeanonymizeArgs),
    Serve(ServeArgs),
    #[cfg(windows)]
    Guard(guard::GuardArgs),
}

#[derive(Args)]
struct ModelArgs {
    #[arg(long, env = "ID4PII_MODEL", default_value = "model")]
    model: PathBuf,
    #[arg(long, default_value = "onnx/model_q4.onnx")]
    model_file: String,
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Args)]
struct ScanArgs {
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

#[derive(Args)]
struct AnonymizeArgs {
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

#[derive(Args)]
struct DeanonymizeArgs {
    text: Option<String>,
    #[arg(short, long)]
    file: Option<PathBuf>,
    #[arg(long)]
    vault: PathBuf,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    #[command(flatten)]
    model: ModelArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum Style {
    Label,
    Block,
    Char,
}

impl From<Style> for RedactStyle {
    fn from(value: Style) -> Self {
        match value {
            Style::Label => RedactStyle::Label,
            Style::Block => RedactStyle::Block,
            Style::Char => RedactStyle::Char,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Json,
    Text,
}

#[derive(Serialize)]
struct AnonymizeOutput {
    anonymized: String,
    vault: Vault,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn")),
        )
        .init();

    match Cli::parse().command {
        Command::Scan(args) => run_scan(&args),
        Command::Anonymize(args) => run_anonymize(&args),
        Command::Deanonymize(args) => run_deanonymize(&args),
        Command::Serve(args) => {
            serve::run(
                args.addr,
                args.model.model,
                args.model.model_file,
                args.model.threads,
            )
            .await
        }
        #[cfg(windows)]
        Command::Guard(args) => guard::run(&args),
    }
}

fn run_scan(args: &ScanArgs) -> Result<()> {
    let text = read_input(args.text.as_ref(), args.file.as_ref())?;
    let mut detector = Detector::load(
        &args.model.model,
        &args.model.model_file,
        args.model.threads,
    )
    .context("failed to load model")?;
    let spans = detector.detect(&text).context("detection failed")?;

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

fn run_anonymize(args: &AnonymizeArgs) -> Result<()> {
    let text = read_input(args.text.as_ref(), args.file.as_ref())?;
    let mut detector = Detector::load(
        &args.model.model,
        &args.model.model_file,
        args.model.threads,
    )
    .context("failed to load model")?;
    let spans = detector.detect(&text).context("detection failed")?;

    let mut rng = args.seed.map_or_else(Rng::from_entropy, Rng::new);
    let (anonymized, vault) = anonymize(&text, &spans, &mut rng);

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

fn run_deanonymize(args: &DeanonymizeArgs) -> Result<()> {
    let text = read_input(args.text.as_ref(), args.file.as_ref())?;
    let vault_text = std::fs::read_to_string(&args.vault)
        .with_context(|| format!("failed to read {}", args.vault.display()))?;
    let vault: Vault = serde_json::from_str(&vault_text).context("invalid vault file")?;
    println!("{}", deanonymize(&text, &vault));
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
