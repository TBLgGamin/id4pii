use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpKind {
    Anonymize,
    Restore,
    Undo,
}

#[derive(Clone, Debug)]
pub(crate) enum OpSummary {
    Anonymized { count: usize },
    Restored { count: usize },
    Undone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoChangeReason {
    EmptyField,
    NoPii,
    NothingToRestore,
    NoUndoAvailable,
    FieldChangedSinceLastOp,
    UndoExpired,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Source {
    Hotkey { cursor: (i32, i32) },
    Browser { client_id: u64 },
}

#[derive(Debug)]
pub(crate) enum Command {
    Anonymize {
        req_id: String,
        source: Source,
    },
    Restore {
        req_id: String,
        source: Source,
    },
    Undo {
        req_id: String,
        source: Source,
    },
    AnonymizeText {
        req_id: String,
        source: Source,
        text: String,
        reply: SyncSender<BridgeReply>,
    },
    RestoreText {
        req_id: String,
        source: Source,
        text: String,
        reply: SyncSender<BridgeReply>,
    },
    ClearVault {
        req_id: String,
    },
    Shutdown,
}

#[derive(Clone, Debug)]
pub(crate) enum BridgeReply {
    Anonymized {
        text: String,
        subs: Vec<(String, String)>,
        count: usize,
    },
    Restored {
        text: String,
        count: usize,
    },
    NoChange {
        reason: NoChangeReason,
    },
    Failed {
        error: String,
    },
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum Event {
    HotkeyRegistrationFailed {
        combo: &'static str,
        error: String,
    },
    VaultLoaded {
        entries: usize,
    },
    VaultLoadFailed {
        error: String,
        quarantined_to: Option<PathBuf>,
    },
    VaultSaved {
        entries: usize,
    },
    VaultSaveFailed {
        error: String,
    },
    VaultDelta {
        req_id: String,
        added: Vec<(String, String)>,
    },
    VaultCleared {
        req_id: String,
        removed: usize,
    },
    OperationCompleted {
        req_id: String,
        kind: OpKind,
        summary: OpSummary,
        source: Source,
    },
    OperationFailed {
        req_id: String,
        kind: OpKind,
        error: String,
        source: Source,
    },
    OperationNoChange {
        req_id: String,
        kind: OpKind,
        reason: NoChangeReason,
        source: Source,
    },
    BackpressureDropped {
        kind: OpKind,
    },
    EnginePanicked {
        error: String,
    },
    Shutdown,
}

const SUBSCRIBER_CHANNEL_DEPTH: usize = 32;

#[derive(Default)]
pub(crate) struct EventBus {
    subscribers: Vec<SyncSender<Event>>,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn subscribe(&mut self) -> Receiver<Event> {
        let (tx, rx) = sync_channel(SUBSCRIBER_CHANNEL_DEPTH);
        self.subscribers.push(tx);
        rx
    }

    pub(crate) fn publish(&self, event: &Event) {
        for sub in &self.subscribers {
            let _ = sub.try_send(event.clone());
        }
    }
}

#[derive(Default)]
pub(crate) struct EngineStatus {
    state: Mutex<Option<BusyState>>,
    stats: Mutex<EngineStats>,
}

struct BusyState {
    kind: OpKind,
    started_at: Instant,
    step: &'static str,
}

#[derive(Default, Clone)]
pub(crate) struct EngineStats {
    pub anonymized: u64,
    pub restored: u64,
    pub undone: u64,
    pub no_change: u64,
    pub failed: u64,
    pub dropped: u64,
    pub received: u64,
    pub last_complete: Option<Instant>,
}

impl EngineStatus {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn begin(&self, kind: OpKind) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(BusyState {
            kind,
            started_at: Instant::now(),
            step: "start",
        });
    }

    pub(crate) fn step(&self, step: &'static str) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(b) = guard.as_mut() {
            b.step = step;
        }
    }

    pub(crate) fn end(&self) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = None;
    }

    pub(crate) fn snapshot(&self) -> Option<(OpKind, &'static str, Duration)> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .as_ref()
            .map(|b| (b.kind, b.step, b.started_at.elapsed()))
    }

    pub(crate) fn record_completed(&self, kind: OpKind) {
        let mut s = self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match kind {
            OpKind::Anonymize => s.anonymized += 1,
            OpKind::Restore => s.restored += 1,
            OpKind::Undo => s.undone += 1,
        }
        s.last_complete = Some(Instant::now());
    }

    pub(crate) fn record_no_change(&self) {
        let mut s = self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.no_change += 1;
    }

    pub(crate) fn record_failed(&self) {
        let mut s = self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.failed += 1;
    }

    pub(crate) fn record_dropped(&self) {
        let mut s = self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.dropped += 1;
    }

    pub(crate) fn record_received(&self) {
        let mut s = self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.received += 1;
    }

    pub(crate) fn stats(&self) -> EngineStats {
        self.stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
