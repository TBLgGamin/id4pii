#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    id4pii_app::cli::run_guard_bin()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("id4pii-guard runs only on Windows.");
    std::process::exit(1);
}
