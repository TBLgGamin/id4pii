// Privacy discipline: never log `text`, `output`, `restored`, `previous_text`,
// `expected_current_text`, or any `vault.entries[*]` field. Only kinds, categories,
// counts, durations, step names.

use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use id4pii_core::{Detector, Rng, Vault, anonymize_with_subs, deanonymize};
use tracing::{debug, error, info, instrument, warn};

use super::automation;
use super::bus::{
    BridgeReply, Command, EngineStatus, Event, EventBus, NoChangeReason, OpKind, OpSummary,
    Source,
};
use super::store::VaultStore;

const UNDO_TTL: Duration = Duration::from_secs(300);
const IO_TIMEOUT: Duration = Duration::from_secs(8);
const DETECT_TIMEOUT: Duration = Duration::from_secs(15);
const SAVE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Engine {
    detector: Arc<Mutex<Detector>>,
    vault: Arc<Mutex<Vault>>,
    rng: Rng,
    store: Arc<dyn VaultStore>,
    bus: Arc<EventBus>,
    status: Arc<EngineStatus>,
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
    ) -> Result<Self> {
        let detector =
            Detector::load(model, model_file, threads).context("failed to load model")?;

        let outcome = match store.load() {
            Ok(outcome) => {
                bus.publish(Event::VaultLoaded {
                    entries: outcome.entries,
                });
                outcome
            }
            Err(err) => {
                let quarantined = store.quarantine();
                let detail = err.to_string();
                bus.publish(Event::VaultLoadFailed {
                    error: detail.clone(),
                    quarantined_to: quarantined.clone(),
                });
                return Err(err.context(format!(
                    "refusing to start with unreadable vault (quarantined to {quarantined:?})"
                )));
            }
        };

        Ok(Self {
            detector: Arc::new(Mutex::new(detector)),
            vault: Arc::new(Mutex::new(outcome.vault)),
            rng: Rng::from_entropy(),
            store,
            bus,
            status,
            undo: None,
        })
    }

    pub(crate) fn vault_handle(&self) -> Arc<Mutex<Vault>> {
        Arc::clone(&self.vault)
    }

    pub(crate) fn run(mut self, commands: Receiver<Command>) {
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
                self.bus.publish(Event::EnginePanicked { error: msg });
                self.status.end();
                if let Err(err) = self.reload_vault() {
                    error!("vault reload after panic: {err}");
                }
            }
        }
        self.bus.publish(Event::Shutdown);
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
            Command::AnonymizeText { req_id, source, text, reply } => {
                self.status.begin(OpKind::Anonymize);
                self.handle_anonymize_text(req_id, source, text, reply);
                self.status.end();
            }
            Command::RestoreText { req_id, source, text, reply } => {
                self.status.begin(OpKind::Restore);
                self.handle_restore_text(req_id, source, text, reply);
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

    #[instrument(skip_all, fields(req_id = %req_id, kind = "anonymize"))]
    fn handle_anonymize(&mut self, req_id: String, source: Source) {
        let started = Instant::now();
        self.status.step("read");
        let text = match read_focused() {
            Ok(text) => text,
            Err(err) => {
                error!("read_focused: {err}");
                self.bus.publish(Event::OperationFailed {
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
            self.bus.publish(Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                reason: NoChangeReason::EmptyField,
                source,
            });
            return;
        }

        self.status.step("detect");
        let detect_started = Instant::now();
        let spans = match detect_with_timeout(&self.detector, text.clone()) {
            Ok(spans) => spans,
            Err(err) => {
                error!("detection: {err}");
                self.bus.publish(Event::OperationFailed {
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
            self.bus.publish(Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Anonymize,
                reason: NoChangeReason::NoPii,
                source,
            });
            return;
        }

        self.status.step("save");
        match save_with_timeout(&self.store, candidate.clone()) {
            Ok(entries) => {
                self.bus.publish(Event::VaultSaved { entries });
                vault_lock(&self.vault, |v| *v = candidate);
                debug!(vault_size_before, vault_size_after = entries, "vault-write");
            }
            Err(err) => {
                error!("vault save: {err}");
                self.bus.publish(Event::VaultSaveFailed {
                    error: err.to_string(),
                });
                self.bus.publish(Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Anonymize,
                    error: format!("vault save failed: {err}"),
                    source,
                });
                return;
            }
        }

        self.status.step("write");
        if let Err(err) = write_substitutions_or_full(subs.clone(), output.clone()) {
            error!("write-back: {err}");
            self.bus.publish(Event::OperationFailed {
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

        info!(elapsed_ms = started.elapsed().as_millis() as u64, "anonymized PII in the focused field");
        let added: Vec<(String, String)> = subs.into_iter().map(|(real, fake)| (fake, real)).collect();
        debug!(subs_count = added.len(), "vault-delta-publish");
        self.bus.publish(Event::VaultDelta {
            req_id: req_id.clone(),
            added,
        });
        self.bus.publish(Event::OperationCompleted {
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
        let text = match read_focused() {
            Ok(text) => text,
            Err(err) => {
                error!("read_focused: {err}");
                self.bus.publish(Event::OperationFailed {
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
            self.bus.publish(Event::OperationNoChange {
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
        subs.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        if subs.is_empty() {
            info!("nothing to restore in the focused field");
            self.bus.publish(Event::OperationNoChange {
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
        if let Err(err) = write_substitutions_or_full(subs, restored.clone()) {
            error!("write-back: {err}");
            self.bus.publish(Event::OperationFailed {
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

        info!(elapsed_ms = started.elapsed().as_millis() as u64, "restored real values in the focused field");
        self.bus.publish(Event::OperationCompleted {
            req_id,
            kind: OpKind::Restore,
            summary: OpSummary::Restored { count },
            source,
        });
    }

    #[instrument(skip_all, fields(req_id = %req_id, kind = "undo"))]
    fn handle_undo(&mut self, req_id: String, source: Source) {
        let snapshot = match self.undo.clone() {
            Some(s) => s,
            None => {
                self.bus.publish(Event::OperationNoChange {
                    req_id: req_id.clone(),
                    kind: OpKind::Undo,
                    reason: NoChangeReason::NoUndoAvailable,
                    source,
                });
                return;
            }
        };

        if snapshot.captured_at.elapsed() > UNDO_TTL {
            self.undo = None;
            self.bus.publish(Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                reason: NoChangeReason::UndoExpired,
                source,
            });
            return;
        }

        self.status.step("read");
        let current = match read_focused() {
            Ok(text) => text,
            Err(err) => {
                error!("read_focused: {err}");
                self.bus.publish(Event::OperationFailed {
                    req_id: req_id.clone(),
                    kind: OpKind::Undo,
                    error: err.to_string(),
                    source,
                });
                return;
            }
        };

        if current != snapshot.expected_current_text {
            self.bus.publish(Event::OperationNoChange {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                reason: NoChangeReason::FieldChangedSinceLastOp,
                source,
            });
            return;
        }

        self.status.step("write");
        if let Err(err) = write_focused(snapshot.previous_text.clone()) {
            error!("write-back: {err}");
            self.bus.publish(Event::OperationFailed {
                req_id: req_id.clone(),
                kind: OpKind::Undo,
                error: err.to_string(),
                source,
            });
            return;
        }

        self.undo = None;
        info!("undid last operation");
        self.bus.publish(Event::OperationCompleted {
            req_id,
            kind: OpKind::Undo,
            summary: OpSummary::Undone,
            source,
        });
    }

    #[instrument(skip_all, fields(req_id = %req_id, kind = "anonymize-text"))]
    fn handle_anonymize_text(
        &mut self,
        req_id: String,
        source: Source,
        text: String,
        reply: SyncSender<BridgeReply>,
    ) {
        let started = Instant::now();
        if text.trim().is_empty() {
            self.bus.publish(Event::OperationNoChange {
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
        let spans = match detect_with_timeout(&self.detector, text.clone()) {
            Ok(spans) => spans,
            Err(err) => {
                error!("detection: {err}");
                let msg = format!("detection failed: {err}");
                self.bus.publish(Event::OperationFailed {
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
        let (output, subs) = anonymize_with_subs(&text, &spans, &mut self.rng, &mut candidate);
        if output == text {
            self.bus.publish(Event::OperationNoChange {
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

        self.status.step("save");
        match save_with_timeout(&self.store, candidate.clone()) {
            Ok(entries) => {
                self.bus.publish(Event::VaultSaved { entries });
                vault_lock(&self.vault, |v| *v = candidate);
                debug!(vault_size_before, vault_size_after = entries, "vault-write");
            }
            Err(err) => {
                error!("vault save: {err}");
                let msg = format!("vault save failed: {err}");
                self.bus.publish(Event::VaultSaveFailed {
                    error: err.to_string(),
                });
                self.bus.publish(Event::OperationFailed {
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
        self.bus.publish(Event::VaultDelta {
            req_id: req_id.clone(),
            added,
        });
        self.bus.publish(Event::OperationCompleted {
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
                self.bus.publish(Event::VaultSaved { entries: 0 });
                self.bus.publish(Event::VaultCleared {
                    req_id,
                    removed,
                });
            }
            Err(err) => {
                error!("vault save (clear): {err}");
                self.bus.publish(Event::VaultSaveFailed {
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
        text: String,
        reply: SyncSender<BridgeReply>,
    ) {
        if text.trim().is_empty() {
            self.bus.publish(Event::OperationNoChange {
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
            self.bus.publish(Event::OperationNoChange {
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
        let restored = deanonymize(&text, &vault_snapshot);

        self.bus.publish(Event::OperationCompleted {
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

fn read_focused() -> Result<String> {
    run_with_timeout("read_focused", IO_TIMEOUT, automation::read_focused)
}

fn write_focused(text: String) -> Result<()> {
    run_with_timeout("write_focused", IO_TIMEOUT, move || {
        automation::write_focused(&text)
    })
}

fn write_substitutions_or_full(subs: Vec<(String, String)>, fallback: String) -> Result<()> {
    let subs_for_call = subs.clone();
    let applied = run_with_timeout("apply_substitutions", IO_TIMEOUT, move || {
        automation::apply_substitutions(&subs_for_call)
    })?;
    if applied {
        return Ok(());
    }
    warn!("TextPattern unavailable; falling back to full replace (formatting will be lost)");
    write_focused(fallback)
}

fn detect_with_timeout(
    detector: &Arc<Mutex<Detector>>,
    text: String,
) -> Result<Vec<id4pii_core::PiiSpan>> {
    let det = Arc::clone(detector);
    run_with_timeout("detect", DETECT_TIMEOUT, move || {
        let mut guard = det
            .lock()
            .map_err(|_| anyhow!("detector mutex poisoned"))?;
        guard.detect(&text).map_err(|e| anyhow!("{e}"))
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
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => {
            warn!("{name} timed out after {timeout:?}; abandoning worker");
            Err(anyhow!("{name} timed out"))
        }
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
