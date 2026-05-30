use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model_dir;

const HF_BASE: &str = "https://huggingface.co/openai/privacy-filter/resolve/main";
const READ_CHUNK: usize = 64 * 1024;

pub trait FetchProgress: Send {
    fn on_start(&mut self, file: &str, total: Option<u64>);
    fn on_chunk(&mut self, file: &str, written: u64, total: Option<u64>);
    fn on_finish(&mut self, file: &str, written: u64);
    fn on_skip(&mut self, file: &str, size: u64);
}

#[derive(Debug, Default)]
pub struct NoopProgress;

impl FetchProgress for NoopProgress {
    fn on_start(&mut self, _file: &str, _total: Option<u64>) {}
    fn on_chunk(&mut self, _file: &str, _written: u64, _total: Option<u64>) {}
    fn on_finish(&mut self, _file: &str, _written: u64) {}
    fn on_skip(&mut self, _file: &str, _size: u64) {}
}

#[must_use]
pub fn required_files(model_file: &str) -> Vec<String> {
    vec![
        model_dir::DEFAULT_CONFIG.to_string(),
        model_file.to_string(),
        format!("{model_file}_data"),
    ]
}

pub fn ensure_present(
    dir: &Path,
    model_file: &str,
    progress: &mut dyn FetchProgress,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    for relative in required_files(model_file) {
        fetch_one(dir, &relative, progress)?;
    }
    Ok(())
}

fn fetch_one(dir: &Path, relative: &str, progress: &mut dyn FetchProgress) -> Result<()> {
    let target = dir.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let url = format!("{HF_BASE}/{relative}");
    let remote_size = head_content_length(&url).ok().flatten();

    if let Ok(meta) = fs::metadata(&target)
        && meta.is_file()
    {
        match remote_size {
            Some(expected) if meta.len() == expected => {
                progress.on_skip(relative, meta.len());
                return Ok(());
            }
            None => {
                progress.on_skip(relative, meta.len());
                return Ok(());
            }
            _ => {}
        }
    }

    let tmp: PathBuf = tmp_path(&target);
    let _ = fs::remove_file(&tmp);

    progress.on_start(relative, remote_size);
    let mut response = reqwest::blocking::Client::builder()
        .build()
        .map_err(net_err)?
        .get(&url)
        .send()
        .map_err(net_err)?;
    if !response.status().is_success() {
        return Err(Error::Model(format!(
            "download {relative}: HTTP {}",
            response.status()
        )));
    }

    let mut file = fs::File::create(&tmp)?;
    let mut buffer = vec![0_u8; READ_CHUNK];
    let mut written: u64 = 0;
    loop {
        let read = response.read(&mut buffer).map_err(io_err)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        written += read as u64;
        progress.on_chunk(relative, written, remote_size);
    }
    file.flush()?;
    drop(file);

    if let Some(expected) = remote_size
        && written != expected
    {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Model(format!(
            "download {relative}: expected {expected} bytes, got {written}"
        )));
    }

    if target.exists() {
        let _ = fs::remove_file(&target);
    }
    fs::rename(&tmp, &target)?;
    progress.on_finish(relative, written);
    Ok(())
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_default();
    name.push(".partial");
    target.with_file_name(name)
}

fn head_content_length(url: &str) -> Result<Option<u64>> {
    let client = reqwest::blocking::Client::builder()
        .build()
        .map_err(net_err)?;
    let response = client.head(url).send().map_err(net_err)?;
    if !response.status().is_success() {
        return Ok(None);
    }
    Ok(response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok()))
}

#[allow(clippy::needless_pass_by_value)]
fn net_err(err: reqwest::Error) -> Error {
    Error::Model(format!("network: {err}"))
}

fn io_err(err: std::io::Error) -> Error {
    Error::from(err)
}
