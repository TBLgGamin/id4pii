use std::path::PathBuf;
use std::process::Command;

use crate::model_dir;
use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use serde::Serialize;

const CHROME_EXTENSIONS_KEY_64: &str = r"Software\Wow6432Node\Google\Chrome\Extensions";
const CHROME_EXTENSIONS_KEY_32: &str = r"Software\Google\Chrome\Extensions";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "id4pii";
const CHROME_UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

#[derive(Args, Debug)]
pub(crate) struct InstallArgs {
    #[arg(long, default_value_t = true)]
    with_model: bool,
    #[arg(long, default_value_os_t = crate::model_dir::default_dir())]
    model_dir: PathBuf,
    #[arg(long, default_value = crate::model_dir::DEFAULT_MODEL_FILE)]
    model_file: String,
    #[arg(long)]
    register_extension: Option<String>,
    #[arg(long, default_value_t = true)]
    autostart: bool,
    #[arg(long)]
    exe_path: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct UninstallArgs {
    #[arg(long)]
    keep_model: bool,
    #[arg(long)]
    extension_id: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct DoctorArgs {
    #[arg(long)]
    extension_id: Option<String>,
    #[arg(long, default_value_os_t = crate::model_dir::default_dir())]
    model_dir: PathBuf,
    #[arg(long, default_value = crate::model_dir::DEFAULT_MODEL_FILE)]
    model_file: String,
    #[arg(long, default_value_t = 7878)]
    bridge_port: u16,
}

pub(crate) fn run_install(args: &InstallArgs) -> Result<()> {
    if args.with_model {
        crate::model_setup::ensure_model(&args.model_dir, &args.model_file)?;
    }
    if let Some(id) = args.register_extension.as_deref() {
        register_chrome_extension(id)?;
        eprintln!("id4pii: registered Chrome extension {id} for next launch.");
    }
    if args.autostart {
        let exe = resolve_guard_exe(args.exe_path.as_ref())?;
        register_autostart(&exe)?;
        eprintln!("id4pii: autostart registered at {RUN_KEY}\\{RUN_VALUE}.");
    }
    Ok(())
}

pub(crate) fn run_uninstall(args: &UninstallArgs) -> Result<()> {
    if !args.keep_model {
        for dir in id4pii_data_dirs() {
            if dir.exists()
                && let Err(err) = std::fs::remove_dir_all(&dir)
            {
                eprintln!("id4pii: could not remove {}: {err}", dir.display());
            }
        }
    }
    remove_autostart();
    let extension_id = args.extension_id.clone().or_else(|| {
        let id = published_id();
        if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        }
    });
    if let Some(id) = extension_id {
        deregister_chrome_extension(&id)?;
    }
    Ok(())
}

fn id4pii_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = crate::paths::data_root() {
        dirs.push(root);
    }
    let legacy = PathBuf::from("model");
    if legacy.join(model_dir::DEFAULT_CONFIG).exists() {
        dirs.push(legacy);
    }
    dirs
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct DoctorReport {
    model_present: bool,
    model_dir: String,
    autostart: bool,
    registry_chrome: Option<String>,
    bridge_reachable: bool,
    published_extension_id_placeholder: bool,
}

pub(crate) fn run_doctor(args: &DoctorArgs) -> Result<()> {
    let report = DoctorReport {
        model_present: model_dir::is_complete(&args.model_dir, &args.model_file),
        model_dir: args.model_dir.display().to_string(),
        autostart: reg_query(HKCU, RUN_KEY, RUN_VALUE).is_ok(),
        registry_chrome: args.extension_id.as_deref().and_then(chrome_registry_value),
        bridge_reachable: bridge_ping(args.bridge_port),
        published_extension_id_placeholder: published_id_is_placeholder(),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

const HKCU: &str = "HKCU";
const HKLM: &str = "HKLM";

fn register_chrome_extension(extension_id: &str) -> Result<()> {
    validate_extension_id(extension_id)?;
    for root in [CHROME_EXTENSIONS_KEY_64, CHROME_EXTENSIONS_KEY_32] {
        let key = format!(r"{root}\{extension_id}");
        reg_add(HKLM, &key, "update_url", "REG_SZ", CHROME_UPDATE_URL)?;
    }
    Ok(())
}

fn deregister_chrome_extension(extension_id: &str) -> Result<()> {
    validate_extension_id(extension_id)?;
    for root in [CHROME_EXTENSIONS_KEY_64, CHROME_EXTENSIONS_KEY_32] {
        let key = format!(r"{root}\{extension_id}");
        let _ = reg_delete(HKLM, &key);
    }
    Ok(())
}

fn register_autostart(exe: &std::path::Path) -> Result<()> {
    let value = format!("\"{}\"", exe.display());
    reg_add(HKCU, RUN_KEY, RUN_VALUE, "REG_SZ", &value)
}

fn remove_autostart() {
    let _ = reg_delete_value(HKCU, RUN_KEY, RUN_VALUE);
}

fn resolve_guard_exe(provided: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = provided {
        return Ok(path.clone());
    }
    let here = std::env::current_exe().context("resolve current_exe")?;
    let dir = here.parent().context("current_exe has no parent")?;
    Ok(dir.join("id4pii-guard.exe"))
}

fn reg_add(root: &str, key: &str, value: &str, kind: &str, data: &str) -> Result<()> {
    let full = format!("{root}\\{key}");
    let status = Command::new("reg")
        .args(["ADD", &full, "/v", value, "/t", kind, "/d", data, "/f"])
        .status()
        .context("invoking reg.exe")?;
    if !status.success() {
        bail!("reg ADD {full} \\{value} failed: exit {status}");
    }
    Ok(())
}

fn reg_delete(root: &str, key: &str) -> Result<()> {
    let full = format!("{root}\\{key}");
    let status = Command::new("reg")
        .args(["DELETE", &full, "/f"])
        .status()
        .context("invoking reg.exe")?;
    if !status.success() {
        bail!("reg DELETE {full} failed: exit {status}");
    }
    Ok(())
}

fn reg_delete_value(root: &str, key: &str, value: &str) -> Result<()> {
    let full = format!("{root}\\{key}");
    let status = Command::new("reg")
        .args(["DELETE", &full, "/v", value, "/f"])
        .status()
        .context("invoking reg.exe")?;
    if !status.success() {
        bail!("reg DELETE {full} \\{value} failed: exit {status}");
    }
    Ok(())
}

fn reg_query(root: &str, key: &str, value: &str) -> Result<String> {
    let full = format!("{root}\\{key}");
    let output = Command::new("reg")
        .args(["QUERY", &full, "/v", value])
        .output()
        .context("invoking reg.exe")?;
    if !output.status.success() {
        return Err(anyhow!("reg QUERY failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(text)
}

fn chrome_registry_value(extension_id: &str) -> Option<String> {
    if validate_extension_id(extension_id).is_err() {
        return None;
    }
    for root in [CHROME_EXTENSIONS_KEY_64, CHROME_EXTENSIONS_KEY_32] {
        let key = format!(r"{root}\{extension_id}");
        if let Ok(text) = reg_query(HKLM, &key, "update_url") {
            return Some(text.trim().to_string());
        }
    }
    None
}

fn bridge_ping(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr: SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

fn validate_extension_id(id: &str) -> Result<()> {
    if id.len() != 32 || !id.chars().all(|c| matches!(c, 'a'..='p')) {
        bail!("invalid Chrome extension id (expected 32 lowercase a-p chars): {id}");
    }
    Ok(())
}

fn published_id_is_placeholder() -> bool {
    published_id().is_empty()
}

pub(crate) fn published_id() -> &'static str {
    env!("ID4PII_PUBLISHED_EXTENSION_ID")
}
