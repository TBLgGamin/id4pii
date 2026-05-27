use std::path::{Path, PathBuf};

use crate::paths;

pub const DEFAULT_MODEL_FILE: &str = "onnx/model_q4.onnx";
pub const DEFAULT_MODEL_DATA: &str = "onnx/model_q4.onnx_data";
pub const DEFAULT_CONFIG: &str = "config.json";

#[must_use]
pub fn default_dir() -> PathBuf {
    if let Some(dir) = paths::model_dir()
        && dir.join(DEFAULT_CONFIG).exists()
    {
        return dir;
    }
    let legacy = PathBuf::from("model");
    if legacy.join(DEFAULT_CONFIG).exists() {
        return legacy;
    }
    paths::model_dir().unwrap_or(legacy)
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
