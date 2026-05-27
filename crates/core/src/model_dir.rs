use std::path::{Path, PathBuf};

pub const DEFAULT_MODEL_FILE: &str = "onnx/model_q4.onnx";
pub const DEFAULT_MODEL_DATA: &str = "onnx/model_q4.onnx_data";
pub const DEFAULT_CONFIG: &str = "config.json";

#[must_use]
pub fn default_dir() -> PathBuf {
    if let Some(dir) = data_dir()
        && dir.join(DEFAULT_CONFIG).exists()
    {
        return dir;
    }
    let legacy = PathBuf::from("model");
    if legacy.join(DEFAULT_CONFIG).exists() {
        return legacy;
    }
    data_dir().unwrap_or(legacy)
}

#[must_use]
pub fn is_complete(dir: &Path, model_file: &str) -> bool {
    let config = dir.join(DEFAULT_CONFIG);
    let onnx = dir.join(model_file);
    if !config.is_file() || !onnx.is_file() {
        return false;
    }
    let shards = dir.join(format!("{model_file}_data"));
    if shards.exists() && !shards.is_file() {
        return false;
    }
    true
}

fn data_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }?;
    Some(base.join("id4pii").join("model"))
}
