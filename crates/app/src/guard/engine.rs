use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use id4pii_core::{Detector, PiiSpan, Rng, Vault, anonymize_with_subs, deanonymize};
use tracing::{debug, error, info, instrument, warn};

use super::automation;
use super::bus::{
    BridgeReply, Command, EngineStatus, Event, EventBus, NoChangeReason, OpKind, OpSummary, Source,
};
use super::store::VaultStore;

const UNDO_TTL: Duration = Duration::from_mins(5);
const IO_TIMEOUT: Duration = Duration::from_secs(8);
const DETECT_TIMEOUT: Duration = Duration::from_secs(15);
const SAVE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) trait Detect: Send {
    fn detect(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>>;
}

impl Detect for Detector {
    fn detect(&mut self, text: &str, min_score: f32) -> Result<Vec<PiiSpan>> {
        Detector::detect(self, text, min_score).map_err(|e| anyhow!("{e}"))
    }
}

pub(crate) trait Field: Send + Sync {
    fn read(&self) -> Result<String>;
    fn write(&self, text: &str) -> Result<()>;
    fn apply_substitutions(&self, subs: &[(String, String)]) -> Result<bool>;
}

struct UiaField;

impl Field for UiaField {
    fn read(&self) -> Result<String> {
        automation::read_focused()
    }
    fn write(&self, text: &str) -> Result<()> {
        automation::write_focused(text)
    }
    fn apply_substitutions(&self, subs: &[(String, String)]) -> Result<bool> {
        automation::apply_substitutions(subs)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EngineConfig {
    pub min_score: f32,
    pub max_vault_entries: usize,
}

pub(crate) struct Engine {
    detector: Arc<Mutex<dyn Detect>>,
    vault: Arc<Mutex<Vault>>,
    rng: Rng,
    store: Arc<dyn VaultStore>,
    bus: Arc<EventBus>,
    status: Arc<EngineStatus>,
    field: Arc<dyn Field>,
    min_score: f32,
    max_vault_entries: usize,
    undo: Option<UndoSnapshot>,
}

fn vault_lock<T>(v: &Arc<Mutex<Vault>>, f: impl FnOnce(&mut Vault) -> T) -> T {
    let mut g = v.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut g)
}

#[derive(Clone)]
struct UndoSnapshot {
    previous_text: String,
    expected_current_text: String,
    captured_at: Instant,
}

impl Engine {
    pub(crate) fn load(
        model: &Path,
        model_file: &str,
        threads: usize,
        store: Arc<dyn VaultStore>,
        bus: Arc<EventBus>,
        status: Arc<EngineStatus>,
        config: EngineConfig,
    ) -> Result<Self> {
        let detector =
            Detector::load(model, model_file, threads).context("failed to load model")?;
        Self::with_components(
            Arc::new(Mutex::new(detector)),
            Arc::new(UiaField),
            store,
            bus,
            status,
            config,
        )
    }

    fn with_components(
        detector: Arc<Mutex<dyn Detect>>,
        field: Arc<dyn Field>,
        store: Arc<dyn VaultStore>,
        bus: Arc<EventBus>,
        status: Arc<EngineStatus>,
        config: EngineConfig,
    ) -> Result<Self> {
        let outcome = match store.load() {
            Ok(outcome) => {
                bus.publish(&Event::VaultLoaded {
                    entries: outcome.entries,
                });
                outcome
            }
            Err(err) => {
                let quarantined = store.quarantine();
                let detail = err.to_string();
                bus.publish(&Event::VaultLoadFailed {
                    error: detail.clone(),
                    quarantined_to: quarantined.clone(),
                });
                return Err(err.context(format!(
                    "refusing to start with unreadable vault (quarantined to {quarantined:?})"
                )));
            }
        };

        Ok(Self {
            detector,
            vault: Arc::new(Mutex::new(outcome.vault)),
            rng: Rng::from_entropy(),
            store,
            bus,
            status,
            field,
            min_score: config.min_score,
            max_vault_entries: config.max_vault_entries,
            undo: None,
        })
    }

    pub(crate) fn vault_handle(&self) -> Arc<Mutex<Vault>> {
        Arc::clone(&self.vault)
    }

    pub(crate) fn run(mut self, commands: &Receiver<Command>) {
        loop {
            let cmd = match commands.recv() {
                Ok(Command::Shutdown) | Err(_) => break,
                Ok(other) => other,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.dispatch(cmd);
            }));
            if let Err(payload) = result {
                let msg = panic_message(&payload);
                error!("engine panic: {msg}");
                self.bus.publish(&Event::EnginePanicked { error: msg });
                self.status.end();
                if let Err(err) = self.reload_vault() {
                    error!("vault reload after panic: {err}");
                }
            }
        }
        self.bus.publish(&Event::Shutdown);
    }

    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Anonymize { req_id, source } => {
                self.status.begin(OpKind::Anonymize);
                self.handle_anonymize(req_id, source);
                self.status.end();
            }
            Command::Restore { req_id, source } => {
                self.status.begin(OpKind::Restore);
                self.handle_restore(req_id, source);
                self.status.end();
            }
            Command::Undo { req_id, source } => {
                self.status.begin(OpKind::Undo);
                self.handle_undo(req_id, source);
                self.status.end();
            }
            Command::AnonymizeText {
                req_id,
                source,
                text,
                reply,
            } => {
                self.status.begin(OpKind::Anonymize);
                self.handle_anonymize_text(req_id, source, &text, &reply);
                self.status.end();
            }
            Command::RestoreText {
                req_id,
                source,
                text,
                reply,
            } => {
                self.status.begin(OpKind::Restore);
                self.handle_restore_text(req_id, source, &text, &reply);
                self.status.end();
            }
            Command::ClearVault { req_id } => {
                self.handle_clear_vault(req_id);
            }
            Command::Shutdown => {}
        }
    }

    fn reload_vault(&mut self) -> Result<()> {
        let outcome = self.store.load()?;
        vault_lock(&self.vault, |v| *v = outcome.vault);
        self.undo = None;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    #[instrument(skip_all, fields(req_id = %req_id, kind = "anonymize"))]
    fn handle_anonymize(&mut self, req_id: String, source: Source) {
        let started = Instant::now();
        self.status.step("read");
        let text = match read_focused(&self.field) {
            Ok(text) => text,
            Err(err) => {
                error!("read_focused: {err}");
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Anonymize,
                    error: err.to_string(),
                    source,
                });
                return;
            }
        };
        if text.trim().is_empty() {
            info!("hotkey fired but focused field is empty");
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                reason: NoChangeReason::EmptyField,
                source,
            });
            return;
        }

        self.status.step("detect");
        let detect_started = Instant::now();
        let spans = match detect_with_timeout(&self.detector, text.clone(), self.min_score) {
            Ok(spans) => spans,
            Err(err) => {
                error!("detection: {err}");
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Anonymize,
                    error: format!("detection failed: {err}"),
                    source,
                });
                return;
            }
        };
        debug!(
            text_len = text.len(),
            spans_found = spans.len(),
            duration_ms = detect_started.elapsed().as_millis() as u64,
            "detect"
        );
        let count = spans.len();

        let vault_size_before = vault_lock(&self.vault, |v| v.entries.len());
        let mut candidate = vault_lock(&self.vault, |v| v.clone());
        let (output, subs) = anonymize_with_subs(&text, &spans, &mut self.rng, &mut candidate);
        if output == text {
            info!("no PII detected in the focused field");
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                reason: NoChangeReason::NoPii,
                source,
            });
            return;
        }

        let evicted = candidate.enforce_cap(self.max_vault_entries);
        if evicted > 0 {
            warn!(
                evicted,
                cap = self.max_vault_entries,
                "vault cap reached; evicted oldest entries (their surrogates can no longer be restored)"
            );
        }

        self.status.step("save");
        match save_with_timeout(&self.store, candidate.clone()) {
            Ok(entries) => {
                self.bus.publish(&Event::VaultSaved { entries });
                vault_lock(&self.vault, |v| *v = candidate);
                debug!(vault_size_before, vault_size_after = entries, "vault-write");
            }
            Err(err) => {
                error!("vault save: {err}");
                self.bus.publish(&Event::VaultSaveFailed {
                    error: err.to_string(),
                });
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Anonymize,
                    error: format!("vault save failed: {err}"),
                    source,
                });
                return;
            }
        }

        self.status.step("write");
        if let Err(err) = write_substitutions_or_full(&self.field, &subs, output.clone()) {
            error!("write-back: {err}");
            self.bus.publish(&Event::OperationFailed {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                error: err.to_string(),
                source,
            });
            return;
        }

        self.undo = Some(UndoSnapshot {
            previous_text: text,
            expected_current_text: output,
            captured_at: Instant::now(),
        });

        info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "anonymized PII in the focused field"
        );
        let added: Vec<(String, String)> =
            subs.into_iter().map(|(real, fake)| (fake, real)).collect();
        debug!(subs_count = added.len(), "vault-delta-publish");
        self.bus.publish(&Event::VaultDelta {
            req_id: req_id.clone(),
            added,
        });
        self.bus.publish(&Event::OperationCompleted {
            req_id,
            kind: OpKind::Anonymize,
            summary: OpSummary::Anonymized { count },
            source,
        });
    }

    #[instrument(skip_all, fields(req_id = %req_id, kind = "restore"))]
    fn handle_restore(&mut self, req_id: String, source: Source) {
        let started = Instant::now();
        self.status.step("read");
        let text = match read_focused(&self.field) {
            Ok(text) => text,
            Err(err) => {
                error!("read_focused: {err}");
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Restore,
                    error: err.to_string(),
                    source,
                });
                return;
            }
        };
        if text.trim().is_empty() {
            info!("hotkey fired but focused field is empty");
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Restore,
                reason: NoChangeReason::EmptyField,
                source,
            });
            return;
        }

        let vault_snapshot = vault_lock(&self.vault, |v| v.clone());
        debug!(vault_size = vault_snapshot.entries.len(), "vault-read");
        let mut subs: Vec<(String, String)> = vault_snapshot
            .entries
            .iter()
            .filter(|e| !e.fake.is_empty() && text.contains(e.fake.as_str()))
            .map(|e| (e.fake.clone(), e.real.clone()))
            .collect();
        subs.sort_by_key(|b| std::cmp::Reverse(b.0.len()));

        if subs.is_empty() {
            info!("nothing to restore in the focused field");
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Restore,
                reason: NoChangeReason::NothingToRestore,
                source,
            });
            return;
        }

        let count = subs.len();
        let restored = deanonymize(&text, &vault_snapshot);

        self.status.step("write");
        if let Err(err) = write_substitutions_or_full(&self.field, &subs, restored.clone()) {
            error!("write-back: {err}");
            self.bus.publish(&Event::OperationFailed {
                req_id: req_id.clone(),
                kind: OpKind::Restore,
                error: err.to_string(),
                source,
            });
            return;
        }

        self.undo = Some(UndoSnapshot {
            previous_text: text,
            expected_current_text: restored,
            captured_at: Instant::now(),
        });

        info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "restored real values in the focused field"
        );
        self.bus.publish(&Event::OperationCompleted {
            req_id,
            kind: OpKind::Restore,
            summary: OpSummary::Restored { count },
            source,
        });
    }

    #[instrument(skip_all, fields(req_id = %req_id, kind = "undo"))]
    fn handle_undo(&mut self, req_id: String, source: Source) {
        let Some(snapshot) = self.undo.clone() else {
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                reason: NoChangeReason::NoUndoAvailable,
                source,
            });
            return;
        };

        if snapshot.captured_at.elapsed() > UNDO_TTL {
            self.undo = None;
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                reason: NoChangeReason::UndoExpired,
                source,
            });
            return;
        }

        self.status.step("read");
        let current = match read_focused(&self.field) {
            Ok(text) => text,
            Err(err) => {
                error!("read_focused: {err}");
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Undo,
                    error: err.to_string(),
                    source,
                });
                return;
            }
        };

        if current != snapshot.expected_current_text {
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                reason: NoChangeReason::FieldChangedSinceLastOp,
                source,
            });
            return;
        }

        self.status.step("write");
        if let Err(err) = write_focused(&self.field, snapshot.previous_text.clone()) {
            error!("write-back: {err}");
            self.bus.publish(&Event::OperationFailed {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                error: err.to_string(),
                source,
            });
            return;
        }

        self.undo = None;
        info!("undid last operation");
        self.bus.publish(&Event::OperationCompleted {
            req_id,
            kind: OpKind::Undo,
            summary: OpSummary::Undone,
            source,
        });
    }

    #[allow(clippy::too_many_lines)]
    #[instrument(skip_all, fields(req_id = %req_id, kind = "anonymize-text"))]
    fn handle_anonymize_text(
        &mut self,
        req_id: String,
        source: Source,
        text: &str,
        reply: &SyncSender<BridgeReply>,
    ) {
        let started = Instant::now();
        if text.trim().is_empty() {
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                reason: NoChangeReason::EmptyField,
                source,
            });
            let _ = reply.try_send(BridgeReply::NoChange {
                reason: NoChangeReason::EmptyField,
            });
            return;
        }

        self.status.step("detect");
        let detect_started = Instant::now();
        let spans = match detect_with_timeout(&self.detector, text.to_string(), self.min_score) {
            Ok(spans) => spans,
            Err(err) => {
                error!("detection: {err}");
                let msg = format!("detection failed: {err}");
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Anonymize,
                    error: msg.clone(),
                    source,
                });
                let _ = reply.try_send(BridgeReply::Failed { error: msg });
                return;
            }
        };
        debug!(
            text_len = text.len(),
            spans_found = spans.len(),
            duration_ms = detect_started.elapsed().as_millis() as u64,
            "detect"
        );
        let count = spans.len();

        let vault_size_before = vault_lock(&self.vault, |v| v.entries.len());
        let mut candidate = vault_lock(&self.vault, |v| v.clone());
        let (output, subs) = anonymize_with_subs(text, &spans, &mut self.rng, &mut candidate);
        if output == text {
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                reason: NoChangeReason::NoPii,
                source,
            });
            let _ = reply.try_send(BridgeReply::NoChange {
                reason: NoChangeReason::NoPii,
            });
            return;
        }

        let evicted = candidate.enforce_cap(self.max_vault_entries);
        if evicted > 0 {
            warn!(
                evicted,
                cap = self.max_vault_entries,
                "vault cap reached; evicted oldest entries (their surrogates can no longer be restored)"
            );
        }

        self.status.step("save");
        match save_with_timeout(&self.store, candidate.clone()) {
            Ok(entries) => {
                self.bus.publish(&Event::VaultSaved { entries });
                vault_lock(&self.vault, |v| *v = candidate);
                debug!(vault_size_before, vault_size_after = entries, "vault-write");
            }
            Err(err) => {
                error!("vault save: {err}");
                let msg = format!("vault save failed: {err}");
                self.bus.publish(&Event::VaultSaveFailed {
                    error: err.to_string(),
                });
                self.bus.publish(&Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Anonymize,
                    error: msg.clone(),
                    source,
                });
                let _ = reply.try_send(BridgeReply::Failed { error: msg });
                return;
            }
        }

        info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "anonymized PII for browser source"
        );
        let added: Vec<(String, String)> = subs
            .iter()
            .map(|(real, fake)| (fake.clone(), real.clone()))
            .collect();
        debug!(subs_count = added.len(), "vault-delta-publish");
        self.bus.publish(&Event::VaultDelta {
            req_id: req_id.clone(),
            added,
        });
        self.bus.publish(&Event::OperationCompleted {
            req_id,
            kind: OpKind::Anonymize,
            summary: OpSummary::Anonymized { count },
            source,
        });
        let _ = reply.try_send(BridgeReply::Anonymized {
            text: output,
            subs,
            count,
        });
    }

    #[instrument(skip_all, fields(req_id = %req_id, kind = "clear-vault"))]
    fn handle_clear_vault(&mut self, req_id: String) {
        let removed = vault_lock(&self.vault, |v| {
            let n = v.entries.len();
            v.entries.clear();
            n
        });
        let empty = Vault::default();
        match save_with_timeout(&self.store, empty) {
            Ok(_) => {
                self.undo = None;
                info!(removed, "vault cleared");
                self.bus.publish(&Event::VaultSaved { entries: 0 });
                self.bus.publish(&Event::VaultCleared { req_id, removed });
            }
            Err(err) => {
                error!("vault save (clear): {err}");
                self.bus.publish(&Event::VaultSaveFailed {
                    error: err.to_string(),
                });
            }
        }
    }

    #[instrument(skip_all, fields(req_id = %req_id, kind = "restore-text"))]
    fn handle_restore_text(
        &mut self,
        req_id: String,
        source: Source,
        text: &str,
        reply: &SyncSender<BridgeReply>,
    ) {
        if text.trim().is_empty() {
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Restore,
                reason: NoChangeReason::EmptyField,
                source,
            });
            let _ = reply.try_send(BridgeReply::NoChange {
                reason: NoChangeReason::EmptyField,
            });
            return;
        }

        let vault_snapshot = vault_lock(&self.vault, |v| v.clone());
        debug!(vault_size = vault_snapshot.entries.len(), "vault-read");
        let matching_count = vault_snapshot
            .entries
            .iter()
            .filter(|e| !e.fake.is_empty() && text.contains(e.fake.as_str()))
            .count();

        if matching_count == 0 {
            self.bus.publish(&Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Restore,
                reason: NoChangeReason::NothingToRestore,
                source,
            });
            let _ = reply.try_send(BridgeReply::NoChange {
                reason: NoChangeReason::NothingToRestore,
            });
            return;
        }

        let count = matching_count;
        let restored = deanonymize(text, &vault_snapshot);

        self.bus.publish(&Event::OperationCompleted {
            req_id,
            kind: OpKind::Restore,
            summary: OpSummary::Restored { count },
            source,
        });
        let _ = reply.try_send(BridgeReply::Restored {
            text: restored,
            count,
        });
    }
}

fn read_focused(field: &Arc<dyn Field>) -> Result<String> {
    let f = Arc::clone(field);
    run_with_timeout("read_focused", IO_TIMEOUT, move || f.read())
}

fn write_focused(field: &Arc<dyn Field>, text: String) -> Result<()> {
    let f = Arc::clone(field);
    run_with_timeout("write_focused", IO_TIMEOUT, move || f.write(&text))
}

fn write_substitutions_or_full(
    field: &Arc<dyn Field>,
    subs: &[(String, String)],
    fallback: String,
) -> Result<()> {
    let f = Arc::clone(field);
    let subs_for_call = subs.to_owned();
    let applied = run_with_timeout("apply_substitutions", IO_TIMEOUT, move || {
        f.apply_substitutions(&subs_for_call)
    })?;
    if applied {
        return Ok(());
    }
    warn!("TextPattern unavailable; falling back to full replace (formatting will be lost)");
    write_focused(field, fallback)
}

fn detect_with_timeout(
    detector: &Arc<Mutex<dyn Detect>>,
    text: String,
    min_score: f32,
) -> Result<Vec<PiiSpan>> {
    let det = Arc::clone(detector);
    run_with_timeout("detect", DETECT_TIMEOUT, move || {
        let mut guard = det.lock().map_err(|_| anyhow!("detector mutex poisoned"))?;
        guard.detect(&text, min_score)
    })
}

fn save_with_timeout(store: &Arc<dyn VaultStore>, vault: Vault) -> Result<usize> {
    let store_arc = Arc::clone(store);
    run_with_timeout("save", SAVE_TIMEOUT, move || {
        store_arc.save(&vault).map_err(|e| anyhow!("{e}"))
    })
}

fn run_with_timeout<F, T>(name: &'static str, timeout: Duration, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(format!("id4pii-{name}"))
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| anyhow!("spawn {name}: {e}"))?;
    if let Ok(result) = rx.recv_timeout(timeout) {
        result
    } else {
        warn!("{name} timed out after {timeout:?}; abandoning worker");
        Err(anyhow!("{name} timed out"))
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with unknown payload".to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::mpsc::{Receiver, sync_channel};

    use id4pii_core::{Category, PiiSpan};

    use super::super::store::MemoryStore;
    use super::*;

    struct FakeDetector {
        needle: String,
        category: Category,
    }

    impl Detect for FakeDetector {
        fn detect(&mut self, text: &str, _min_score: f32) -> Result<Vec<PiiSpan>> {
            let mut spans = Vec::new();
            let mut from = 0;
            while let Some(pos) = text[from..].find(&self.needle) {
                let start = from + pos;
                let end = start + self.needle.len();
                spans.push(PiiSpan {
                    category: self.category,
                    start,
                    end,
                    text: self.needle.clone(),
                    score: 1.0,
                });
                from = end;
            }
            Ok(spans)
        }
    }

    struct FakeField {
        text: Mutex<String>,
        supports_substitutions: bool,
    }

    impl FakeField {
        fn new(initial: &str, supports_substitutions: bool) -> Self {
            Self {
                text: Mutex::new(initial.to_string()),
                supports_substitutions,
            }
        }
        fn current(&self) -> String {
            self.text.lock().unwrap().clone()
        }
    }

    impl Field for FakeField {
        fn read(&self) -> Result<String> {
            Ok(self.text.lock().unwrap().clone())
        }
        fn write(&self, text: &str) -> Result<()> {
            *self.text.lock().unwrap() = text.to_string();
            Ok(())
        }
        fn apply_substitutions(&self, subs: &[(String, String)]) -> Result<bool> {
            if !self.supports_substitutions {
                return Ok(false);
            }
            let mut guard = self.text.lock().unwrap();
            let mut updated = guard.clone();
            for (find, replace) in subs {
                updated = updated.replace(find, replace);
            }
            *guard = updated;
            Ok(true)
        }
    }

    fn engine_with(
        detector: FakeDetector,
        field: Arc<FakeField>,
    ) -> (Engine, Receiver<Event>, Arc<dyn VaultStore>) {
        engine_with_cap(detector, field, 0)
    }

    fn engine_with_cap(
        detector: FakeDetector,
        field: Arc<FakeField>,
        max_vault_entries: usize,
    ) -> (Engine, Receiver<Event>, Arc<dyn VaultStore>) {
        let mut bus = EventBus::new();
        let rx = bus.subscribe();
        let bus = Arc::new(bus);
        let status = Arc::new(EngineStatus::new());
        let store: Arc<dyn VaultStore> = Arc::new(MemoryStore::new());
        let engine = Engine::with_components(
            Arc::new(Mutex::new(detector)),
            field,
            Arc::clone(&store),
            bus,
            status,
            EngineConfig {
                min_score: 0.0,
                max_vault_entries,
            },
        )
        .unwrap();
        for _ in rx.try_iter() {}
        (engine, rx, store)
    }

    fn person_detector(needle: &str) -> FakeDetector {
        FakeDetector {
            needle: needle.to_string(),
            category: Category::PrivatePerson,
        }
    }

    fn browser() -> Source {
        Source::Browser { client_id: 1 }
    }

    fn hotkey() -> Source {
        Source::Hotkey { cursor: (0, 0) }
    }

    #[test]
    fn anonymize_text_swaps_pii_and_records_vault() {
        let field = Arc::new(FakeField::new("", true));
        let (mut engine, rx, store) =
            engine_with(person_detector("Sarah Connor"), Arc::clone(&field));
        let (reply_tx, reply_rx) = sync_channel(1);
        engine.dispatch(Command::AnonymizeText {
            req_id: "r1".into(),
            source: browser(),
            text: "call Sarah Connor now".into(),
            reply: reply_tx,
        });

        match reply_rx.try_recv().unwrap() {
            BridgeReply::Anonymized { text, count, .. } => {
                assert_eq!(count, 1);
                assert!(!text.contains("Sarah Connor"));
            }
            other => panic!("expected Anonymized, got {other:?}"),
        }
        assert_eq!(store.load().unwrap().entries, 1);
        assert!(
            rx.try_iter()
                .any(|e| matches!(e, Event::OperationCompleted { .. }))
        );
    }

    #[test]
    fn anonymize_text_with_no_pii_reports_no_change() {
        let field = Arc::new(FakeField::new("", true));
        let (mut engine, _rx, store) = engine_with(person_detector("Nobody"), field);
        let (reply_tx, reply_rx) = sync_channel(1);
        engine.dispatch(Command::AnonymizeText {
            req_id: "r1".into(),
            source: browser(),
            text: "no pii in this sentence".into(),
            reply: reply_tx,
        });
        assert!(matches!(
            reply_rx.try_recv().unwrap(),
            BridgeReply::NoChange {
                reason: NoChangeReason::NoPii
            }
        ));
        assert_eq!(store.load().unwrap().entries, 0);
    }

    #[test]
    fn anonymize_then_restore_text_round_trips() {
        let field = Arc::new(FakeField::new("", true));
        let (mut engine, _rx, _store) = engine_with(person_detector("Sarah Connor"), field);

        let (atx, arx) = sync_channel(1);
        engine.dispatch(Command::AnonymizeText {
            req_id: "a".into(),
            source: browser(),
            text: "call Sarah Connor now".into(),
            reply: atx,
        });
        let anonymized = match arx.try_recv().unwrap() {
            BridgeReply::Anonymized { text, .. } => text,
            other => panic!("expected Anonymized, got {other:?}"),
        };

        let (rtx, rrx) = sync_channel(1);
        engine.dispatch(Command::RestoreText {
            req_id: "b".into(),
            source: browser(),
            text: anonymized,
            reply: rtx,
        });
        match rrx.try_recv().unwrap() {
            BridgeReply::Restored { text, count } => {
                assert_eq!(count, 1);
                assert_eq!(text, "call Sarah Connor now");
            }
            other => panic!("expected Restored, got {other:?}"),
        }
    }

    #[test]
    fn hotkey_anonymize_rewrites_field_then_undo_restores_it() {
        let field = Arc::new(FakeField::new("meet Sarah Connor", true));
        let (mut engine, _rx, _store) =
            engine_with(person_detector("Sarah Connor"), Arc::clone(&field));

        engine.dispatch(Command::Anonymize {
            req_id: "a".into(),
            source: hotkey(),
        });
        let after_anon = field.current();
        assert_ne!(after_anon, "meet Sarah Connor");
        assert!(!after_anon.contains("Sarah Connor"));

        engine.dispatch(Command::Undo {
            req_id: "u".into(),
            source: hotkey(),
        });
        assert_eq!(field.current(), "meet Sarah Connor");
    }

    #[test]
    fn undo_without_prior_op_reports_no_change() {
        let field = Arc::new(FakeField::new("anything", true));
        let (mut engine, rx, _store) = engine_with(person_detector("x"), field);
        engine.dispatch(Command::Undo {
            req_id: "u".into(),
            source: hotkey(),
        });
        assert!(rx.try_iter().any(|e| matches!(
            e,
            Event::OperationNoChange {
                reason: NoChangeReason::NoUndoAvailable,
                ..
            }
        )));
    }

    #[test]
    fn vault_cap_evicts_oldest_entries() {
        let field = Arc::new(FakeField::new("", true));
        let (mut engine, _rx, store) = engine_with_cap(person_detector("Sarah Connor"), field, 1);
        for (i, name) in ["Sarah Connor", "Kyle Reese"].iter().enumerate() {
            let detector = person_detector(name);
            engine.detector = Arc::new(Mutex::new(detector));
            let (tx, _rx) = sync_channel(1);
            engine.dispatch(Command::AnonymizeText {
                req_id: format!("r{i}"),
                source: browser(),
                text: (*name).to_string(),
                reply: tx,
            });
        }
        assert_eq!(store.load().unwrap().entries, 1);
    }

    #[test]
    fn clear_vault_empties_and_emits_event() {
        let field = Arc::new(FakeField::new("", true));
        let (mut engine, rx, store) = engine_with(person_detector("Sarah Connor"), field);
        let (atx, _arx) = sync_channel(1);
        engine.dispatch(Command::AnonymizeText {
            req_id: "a".into(),
            source: browser(),
            text: "Sarah Connor".into(),
            reply: atx,
        });
        assert_eq!(store.load().unwrap().entries, 1);

        engine.dispatch(Command::ClearVault { req_id: "c".into() });
        assert_eq!(store.load().unwrap().entries, 0);
        assert!(
            rx.try_iter()
                .any(|e| matches!(e, Event::VaultCleared { removed: 1, .. }))
        );
    }
}
