use std::path::PathBuf;

#[must_use]
pub fn data_root() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }?;
    Some(base.join("id4pii"))
}

#[must_use]
pub fn model_dir() -> Option<PathBuf> {
    data_root().map(|r| r.join("model"))
}

#[must_use]
pub fn log_dir() -> Option<PathBuf> {
    data_root().map(|r| r.join("logs"))
}

#[must_use]
pub fn vault_file() -> Option<PathBuf> {
    data_root().map(|r| r.join("vault.bin"))
}
