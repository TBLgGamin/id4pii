use std::io::Read;
use std::path::PathBuf;

use crate::{PiiSpan, RedactStyle, Rng, Vault, anonymize, deanonymize, model_dir, redact};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::{corpus, logging, model_setup, serve};
#[cfg(windows)]
use crate::{daemon, install};

#[derive(Parser, Debug)]
#[command(
    name = "id4pii",
    version,
    about = "Detect, redact and reversibly anonymize PII in text"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Scan(ScanArgs),
    Anonymize(AnonymizeArgs),
    Deanonymize(DeanonymizeArgs),
    Corpus(corpus::CorpusArgs),
    Serve(ServeArgs),
    #[cfg(windows)]
    Daemon(daemon::DaemonArgs),
    #[cfg(windows)]
    Install(install::InstallArgs),
    #[cfg(windows)]
    Uninstall(install::UninstallArgs),
    #[cfg(windows)]
    Doctor(install::DoctorArgs),
}

#[derive(Args, Debug)]
pub(crate) struct ModelArgs {
    #[arg(long, env = "ID4PII_MODEL", default_value_os_t = model_dir::default_dir())]
    pub(crate) model: PathBuf,
    #[arg(long, default_value = model_dir::DEFAULT_MODEL_FILE)]
    pub(crate) model_file: String,
    #[arg(long, default_value_t = 0)]
    pub(crate) threads: usize,
    #[arg(long, default_value_t = 0.0)]
    pub(crate) min_score: f32,
}

#[derive(Args, Debug)]
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

#[derive(Args, Debug)]
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

#[derive(Args, Debug)]
struct DeanonymizeArgs {
    text: Option<String>,
    #[arg(short, long)]
    file: Option<PathBuf>,
    #[arg(long)]
    vault: PathBuf,
}

#[derive(Args, Debug)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    #[command(flatten)]
    model: ModelArgs,
}

#[derive(Clone, Copy, ValueEnum, Debug)]
pub(crate) enum Style {
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

#[tokio::main]
pub async fn run() -> Result<()> {
    logging::init_cli();

    match Cli::parse().command {
        Command::Scan(args) => run_scan(&args),
        Command::Anonymize(args) => run_anonymize(&args),
        Command::Deanonymize(args) => run_deanonymize(&args),
        Command::Corpus(args) => corpus::run(&args),
        Command::Serve(args) => {
            serve::run(
                args.addr,
                args.model.model,
                args.model.model_file,
                args.model.threads,
                args.model.min_score,
            )
            .await
        }
        #[cfg(windows)]
        Command::Daemon(args) => daemon::run(&args),
        #[cfg(windows)]
        Command::Install(args) => install::run_install(&args),
        #[cfg(windows)]
        Command::Uninstall(args) => install::run_uninstall(&args),
        #[cfg(windows)]
        Command::Doctor(args) => install::run_doctor(&args),
    }
}

fn run_scan(args: &ScanArgs) -> Result<()> {
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

fn run_anonymize(args: &AnonymizeArgs) -> Result<()> {
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

#[cfg(windows)]
#[derive(Parser, Debug)]
#[command(name = "id4pii-daemon", version, about = "id4pii daemon (system tray)")]
struct DaemonBin {
    #[command(flatten)]
    args: daemon::DaemonArgs,
}

#[cfg(windows)]
pub fn run_daemon_bin() -> Result<()> {
    let bin = DaemonBin::parse();
    let _log_dir = logging::init_daemon(bin.args.dev_extensions)?;
    daemon::run(&bin.args)
}
