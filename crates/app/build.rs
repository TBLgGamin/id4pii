use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED_KEYS: &[&str] = &[
    "ID4PII_PUBLISHED_EXTENSION_ID",
    "ID4PII_INSTALLER_URL",
    "ID4PII_GITHUB_REPO",
];

fn main() {
    let root = workspace_root();
    let env_path = root.join(".env");
    println!("cargo:rerun-if-changed={}", env_path.display());

    if !env_path.exists() {
        fail(&format!(
            "missing {}\n\n\
             id4pii is built from source via a .env file. End users do NOT need this\n\
             (they install the prebuilt exe or the Chrome Web Store extension).\n\n\
             Devs and CI: copy .env.example to .env and fill it in.\n\
             See CONTRIBUTING.md for the full setup.",
            env_path.display()
        ));
    }

    let values = read_env_file(&env_path);

    for key in REQUIRED_KEYS {
        println!("cargo:rerun-if-env-changed={key}");
        let from_env = std::env::var(key).ok();
        let from_file = values
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone());
        let value = from_env.or(from_file).unwrap_or_else(|| {
            fail(&format!(
                "key `{key}` missing from {} (see .env.example)",
                env_path.display()
            ));
        });
        println!("cargo:rustc-env={key}={value}");
    }
}

fn fail(msg: &str) -> ! {
    for line in msg.lines() {
        println!("cargo:warning={line}");
    }
    std::process::exit(1);
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    manifest_dir
        .ancestors()
        .find(|p| p.join("Cargo.lock").exists())
        .map(Path::to_path_buf)
        .unwrap_or(manifest_dir)
}

fn read_env_file(path: &Path) -> Vec<(String, String)> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let mut value = value.trim().to_string();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = value[1..value.len() - 1].to_string();
        }
        out.push((key, value));
    }
    out
}
