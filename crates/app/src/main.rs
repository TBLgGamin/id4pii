#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod serve;

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use id4pii_core::{Detector, PiiSpan, RedactStyle, redact};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "id4pii", version, about = "Detect and redact PII in text")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Scan(ScanArgs),
    Serve(ServeArgs),
}

#[derive(Args)]
struct ModelArgs {
    #[arg(long, env = "ID4PII_MODEL", default_value = "model")]
    model: PathBuf,
    #[arg(long, default_value = "model.onnx")]
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,ort=warn")),
        )
        .init();

    match Cli::parse().command {
        Command::Scan(args) => run_scan(&args),
        Command::Serve(args) => {
            serve::run(
                args.addr,
                args.model.model,
                args.model.model_file,
                args.model.threads,
            )
            .await
        }
    }
}

fn run_scan(args: &ScanArgs) -> Result<()> {
    let text = read_input(args)?;
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

fn read_input(args: &ScanArgs) -> Result<String> {
    if let Some(text) = &args.text {
        return Ok(text.clone());
    }
    if let Some(path) = &args.file {
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
