mod ca;
mod handler;
mod hosts;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use hudsucker::Proxy;
use hudsucker::rustls::crypto::aws_lc_rs;
use id4pii_core::{Detector, Rng, Vault};
use tracing::info;

use self::handler::PiiHandler;
use self::hosts::HostMatcher;

#[derive(Args)]
pub(crate) struct ProxyArgs {
    #[command(subcommand)]
    action: ProxyAction,
}

#[derive(Subcommand)]
enum ProxyAction {
    Run(RunArgs),
    Cert(CertArgs),
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, default_value_t = 8788)]
    port: u16,
    #[arg(long, env = "ID4PII_MODEL", default_value = "model")]
    model: PathBuf,
    #[arg(long, default_value = "onnx/model_q4.onnx")]
    model_file: String,
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Args)]
struct CertArgs {
    #[command(subcommand)]
    action: CertAction,
}

#[derive(Subcommand)]
enum CertAction {
    Install,
    Path,
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

pub(crate) async fn run(args: ProxyArgs) -> Result<()> {
    match args.action {
        ProxyAction::Run(run_args) => run_proxy(run_args).await,
        ProxyAction::Cert(cert_args) => run_cert(&cert_args),
    }
}

async fn run_proxy(args: RunArgs) -> Result<()> {
    let _ = aws_lc_rs::default_provider().install_default();

    let detector = Detector::load(&args.model, &args.model_file, args.threads)
        .context("failed to load model")?;
    let authority = ca::authority().context("failed to prepare local CA")?;
    let handler = PiiHandler::new(
        Arc::new(Mutex::new(detector)),
        Arc::new(Mutex::new(Vault::default())),
        Arc::new(Mutex::new(Rng::from_entropy())),
        Arc::new(HostMatcher::load()),
    );

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let proxy = Proxy::builder()
        .with_addr(addr)
        .with_ca(authority)
        .with_rustls_connector(aws_lc_rs::default_provider())
        .with_http_handler(handler)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .build()
        .context("failed to build proxy")?;

    info!("id4pii proxy listening on {addr}");
    info!("set this as your system HTTP/HTTPS proxy (run `id4pii proxy cert install` first)");
    proxy.start().await.context("proxy error")?;
    Ok(())
}

fn run_cert(args: &CertArgs) -> Result<()> {
    match &args.action {
        CertAction::Install => ca::install(),
        CertAction::Path => {
            println!("{}", ca::ensure()?.display());
            Ok(())
        }
        CertAction::Export { out } => ca::export(out.as_deref()),
    }
}
