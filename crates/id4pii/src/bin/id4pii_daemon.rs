#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    id4pii::cli::run_daemon_bin()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("id4pii-daemon runs only on Windows.");
    std::process::exit(1);
}
