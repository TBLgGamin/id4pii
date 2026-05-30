use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{RedactStyle, corpus, logging, model_dir, ops, serve};
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
    Scan(ops::ScanArgs),
    Anonymize(ops::AnonymizeArgs),
    Deanonymize(ops::DeanonymizeArgs),
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

#[tokio::main]
pub async fn run() -> Result<()> {
    logging::init_cli();

    match Cli::parse().command {
        Command::Scan(args) => ops::scan(&args),
        Command::Anonymize(args) => ops::anonymize(&args),
        Command::Deanonymize(args) => ops::deanonymize(&args),
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
