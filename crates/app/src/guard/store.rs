#![allow(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use id4pii_core::{Vault, VaultEntry};
use serde::{Deserialize, Serialize};
use tracing::warn;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

const MAGIC: &[u8; 4] = b"ID4P";
const VERSION: u8 = 1;
const ENTROPY: &[u8] = b"id4pii.vault.v1";

#[derive(Serialize, Deserialize)]
#[serde(tag = "version")]
enum VaultDoc {
    #[serde(rename = "1")]
    V1 { entries: Vec<VaultEntry> },
}

impl VaultDoc {
    fn from_vault(vault: &Vault) -> Self {
        Self::V1 {
            entries: vault.entries.clone(),
        }
    }
    fn into_vault(self) -> Vault {
        match self {
            Self::V1 { entries } => Vault { entries },
        }
    }
}

pub(crate) struct LoadOutcome {
    pub vault: Vault,
    pub entries: usize,
}

pub(crate) trait VaultStore: Send + Sync {
    fn load(&self) -> Result<LoadOutcome>;
    fn save(&self, vault: &Vault) -> Result<usize>;
    fn quarantine(&self) -> Option<PathBuf>;
}

pub(crate) struct DpapiStore {
    path: PathBuf,
}

impl DpapiStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn default_path() -> Result<PathBuf> {
        let base = dirs::data_local_dir().context("could not resolve %LOCALAPPDATA%")?;
        let dir = base.join("id4pii");
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        Ok(dir.join("vault.bin"))
    }

}

impl VaultStore for DpapiStore {
    fn load(&self) -> Result<LoadOutcome> {
        if !self.path.exists() {
            return Ok(LoadOutcome {
                vault: Vault::default(),
                entries: 0,
            });
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        if bytes.len() < MAGIC.len() + 1 || &bytes[..MAGIC.len()] != MAGIC {
            bail!("missing magic header");
        }
        let version = bytes[MAGIC.len()];
        if version != VERSION {
            bail!("unknown vault version {version}");
        }
        let ciphertext = &bytes[MAGIC.len() + 1..];
        let plain = unsafe { unprotect(ciphertext) }?;
        let doc: VaultDoc = serde_json::from_slice(&plain).context("vault JSON parse failed")?;
        let vault = doc.into_vault();
        let entries = vault.entries.len();
        Ok(LoadOutcome { vault, entries })
    }

    fn save(&self, vault: &Vault) -> Result<usize> {
        let doc = VaultDoc::from_vault(vault);
        let plain = serde_json::to_vec(&doc).context("vault JSON encode failed")?;
        let cipher = unsafe { protect(&plain) }?;
        let mut blob = Vec::with_capacity(MAGIC.len() + 1 + cipher.len());
        blob.extend_from_slice(MAGIC);
        blob.push(VERSION);
        blob.extend_from_slice(&cipher);

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, &blob).with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to commit {}", self.path.display()))?;
        Ok(vault.entries.len())
    }

    fn quarantine(&self) -> Option<PathBuf> {
        if !self.path.exists() {
            return None;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("vault.bin");
        let target = self.path.with_file_name(format!("{name}.corrupt-{ts}"));
        match fs::rename(&self.path, &target) {
            Ok(()) => Some(target),
            Err(err) => {
                warn!("failed to quarantine {}: {err}", self.path.display());
                None
            }
        }
    }
}

unsafe fn protect(plain: &[u8]) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr().cast_mut(),
    };
    let ent = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            None,
            Some(&ent),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| anyhow!("CryptProtectData: {e}"))?;
    }
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
    }
    Ok(bytes)
}

unsafe fn unprotect(cipher: &[u8]) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: cipher.len() as u32,
        pbData: cipher.as_ptr().cast_mut(),
    };
    let ent = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&ent),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| anyhow!("CryptUnprotectData: {e}"))?;
    }
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
    }
    Ok(bytes)
}

#[cfg(test)]
pub(crate) struct MemoryStore {
    state: std::sync::Mutex<Option<Vault>>,
}

#[cfg(test)]
impl MemoryStore {
    pub(crate) fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(None),
        }
    }
}

#[cfg(test)]
impl VaultStore for MemoryStore {
    fn load(&self) -> Result<LoadOutcome> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let vault = guard.clone().unwrap_or_default();
        let entries = vault.entries.len();
        Ok(LoadOutcome { vault, entries })
    }
    fn save(&self, vault: &Vault) -> Result<usize> {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(vault.clone());
        Ok(vault.entries.len())
    }
    fn quarantine(&self) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use id4pii_core::{Category, VaultEntry};

    fn sample_vault() -> Vault {
        Vault {
            entries: vec![
                VaultEntry {
                    category: Category::PrivatePerson,
                    real: "Alice".into(),
                    fake: "Bob".into(),
                },
                VaultEntry {
                    category: Category::PrivateEmail,
                    real: "alice@corp.com".into(),
                    fake: "bob@example.com".into(),
                },
            ],
        }
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        let LoadOutcome { entries, .. } = store.load().unwrap();
        assert_eq!(entries, 0);
        store.save(&sample_vault()).unwrap();
        let LoadOutcome { vault, entries } = store.load().unwrap();
        assert_eq!(entries, 2);
        assert_eq!(vault.entries[0].real, "Alice");
    }

    #[test]
    fn vault_round_trips_across_two_sessions() {
        use id4pii_core::{Category, PiiSpan, Rng, anonymize_into, deanonymize};

        let store = MemoryStore::new();

        let mut session1 = store.load().unwrap().vault;
        let mut rng = Rng::new(42);
        let text1 = "Hi John Smith, your appointment is set.";
        let spans1 = vec![PiiSpan {
            category: Category::PrivatePerson,
            start: 3,
            end: 13,
            text: "John Smith".into(),
            score: 1.0,
        }];
        let anon1 = anonymize_into(text1, &spans1, &mut rng, &mut session1);
        store.save(&session1).unwrap();

        let mut session2 = store.load().unwrap().vault;
        assert_eq!(session2.entries.len(), 1);

        let text2 = "Tell Jane Doe to call back.";
        let spans2 = vec![PiiSpan {
            category: Category::PrivatePerson,
            start: 5,
            end: 13,
            text: "Jane Doe".into(),
            score: 1.0,
        }];
        let anon2 = anonymize_into(text2, &spans2, &mut rng, &mut session2);
        store.save(&session2).unwrap();

        let restored_vault = store.load().unwrap().vault;
        assert_eq!(deanonymize(&anon1, &restored_vault), text1);
        assert_eq!(deanonymize(&anon2, &restored_vault), text2);
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "DPAPI round-trip requires interactive Windows session"]
    fn dpapi_store_round_trip() {
        let tmp = std::env::temp_dir().join(format!("id4pii-test-{}.bin", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let store = DpapiStore::new(tmp.clone());
        store.save(&sample_vault()).unwrap();
        let LoadOutcome { vault, entries } = store.load().unwrap();
        assert_eq!(entries, 2);
        assert_eq!(vault.entries[1].fake, "bob@example.com");
        let _ = fs::remove_file(&tmp);
    }
}
