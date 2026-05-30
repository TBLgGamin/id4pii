use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, SyncSender as StdSyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::Vault;
use anyhow::{Context, Result};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::{Engine as _, prelude::BASE64_STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::bus::{BridgeReply, Command, Event, NoChangeReason, OpKind, OpSummary, Source};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const EVENT_BROADCAST_CAPACITY: usize = 256;
const PUBLISHED_EXTENSION_ID: &str = env!("ID4PII_PUBLISHED_EXTENSION_ID");
const ALLOWED_ORIGIN_PREFIXES_DEV: &[&str] = &[
    "chrome-extension://",
    "moz-extension://",
    "safari-web-extension://",
];

#[derive(Clone)]
struct AppState {
    command_tx: StdSyncSender<Command>,
    event_tx: broadcast::Sender<Event>,
    next_client_id: Arc<AtomicU64>,
    vault: Arc<Mutex<Vault>>,
    dev_extensions: bool,
}

pub(crate) fn spawn(
    port: u16,
    dev_extensions: bool,
    command_tx: StdSyncSender<Command>,
    bus_rx: StdReceiver<Event>,
    vault: Arc<Mutex<Vault>>,
) -> Result<std::thread::JoinHandle<()>> {
    let handle = std::thread::Builder::new()
        .name("id4pii-bridge".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    warn!("bridge tokio runtime: {err}");
                    return;
                }
            };
            if let Err(err) = runtime.block_on(run(port, dev_extensions, command_tx, bus_rx, vault))
            {
                warn!("bridge stopped: {err}");
            }
        })
        .context("spawn bridge thread")?;
    Ok(handle)
}

async fn run(
    port: u16,
    dev_extensions: bool,
    command_tx: StdSyncSender<Command>,
    bus_rx: StdReceiver<Event>,
    vault: Arc<Mutex<Vault>>,
) -> Result<()> {
    let (event_tx, _) = broadcast::channel::<Event>(EVENT_BROADCAST_CAPACITY);
    let pump_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("id4pii-bridge-pump".into())
        .spawn(move || {
            while let Ok(event) = bus_rx.recv() {
                let _ = pump_tx.send(event);
            }
        })
        .context("spawn bridge pump thread")?;

    let state = AppState {
        command_tx,
        event_tx,
        next_client_id: Arc::new(AtomicU64::new(1)),
        vault,
        dev_extensions,
    };

    let app = Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/health", get(|| async { "ok" }))
        .with_state(state);

    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .context("invalid bridge addr")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    info!("id4pii bridge listening on ws://{addr}/ws");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("bridge server error")?;
    Ok(())
}

async fn ws_upgrade(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get("origin")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !origin_allowed(origin, state.dev_extensions) {
        warn!("bridge rejected ws upgrade from {addr}, origin={origin:?}");
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let client_id = state.next_client_id.fetch_add(1, Ordering::Relaxed);
    let event_rx = state.event_tx.subscribe();
    let command_tx = state.command_tx.clone();
    let vault = Arc::clone(&state.vault);
    info!(client_id, %addr, "bridge accepted client");
    ws.on_upgrade(move |socket| client_loop(socket, client_id, command_tx, event_rx, vault))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Hello {
        #[serde(default)]
        host: String,
        #[serde(default)]
        tab_id: String,
    },
    Anonymize {
        id: String,
        text: String,
    },
    AnonymizeFile {
        id: String,
        #[serde(default)]
        filename: String,
        data: String,
    },
    Restore {
        id: String,
        text: String,
    },
    VaultGet {
        #[serde(default)]
        #[allow(dead_code)]
        id: String,
    },
    VaultClear {
        #[serde(default)]
        id: String,
    },
    Ping {},
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage<'a> {
    HelloAck {
        client_id: u64,
    },
    Anonymized {
        id: String,
        text: String,
        subs: Vec<[String; 2]>,
        count: usize,
    },
    AnonymizedFile {
        id: String,
        data: String,
        mime: &'a str,
        count: usize,
    },
    Restored {
        id: String,
        text: String,
        count: usize,
    },
    Vault {
        entries: &'a [VaultEntrySer<'a>],
    },
    VaultDelta {
        added: Vec<[String; 2]>,
    },
    VaultCleared {
        removed: usize,
    },
    NoChange {
        id: String,
        reason: &'static str,
    },
    Error {
        id: String,
        message: String,
    },
    Event {
        kind: &'static str,
        op: &'static str,
        detail: String,
    },
    Pong {},
}

#[derive(Serialize)]
struct VaultEntrySer<'a> {
    category: &'a str,
    real: &'a str,
    fake: &'a str,
}

async fn client_loop(
    socket: WebSocket,
    client_id: u64,
    command_tx: StdSyncSender<Command>,
    mut event_rx: broadcast::Receiver<Event>,
    vault: Arc<Mutex<Vault>>,
) {
    let (mut sink, mut stream) = socket.split();

    loop {
        tokio::select! {
            biased;
            incoming = stream.next() => {
                let Some(frame) = incoming else { break };
                let Ok(frame) = frame else { break };
                let text = match frame {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    Message::Ping(p) => {
                        if sink.send(Message::Pong(p)).await.is_err() { break; }
                        continue;
                    }
                    _ => continue,
                };
                let parsed: serde_json::Result<ClientMessage> = serde_json::from_str(&text);
                let msg = match parsed {
                    Ok(m) => m,
                    Err(err) => {
                        let payload = serde_json::to_string(&ServerMessage::Error {
                            id: String::new(),
                            message: format!("invalid json: {err}"),
                        }).unwrap_or_default();
                        if sink.send(Message::Text(payload.into())).await.is_err() { break; }
                        continue;
                    }
                };
                if let Some(response) = handle_client_message(msg, client_id, &command_tx, &vault).await
                    && sink.send(Message::Text(response.into())).await.is_err() { break; }
            }
            event = event_rx.recv() => {
                let event = match event {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if let Some(payload) = event_to_message(event, client_id)
                    && sink.send(Message::Text(payload.into())).await.is_err() { break; }
            }
        }
    }

    info!(client_id, "bridge client disconnected");
}

async fn handle_client_message(
    msg: ClientMessage,
    client_id: u64,
    command_tx: &StdSyncSender<Command>,
    vault: &Arc<Mutex<Vault>>,
) -> Option<String> {
    match msg {
        ClientMessage::Hello { host, tab_id } => {
            info!(
                client_id,
                host = host.as_str(),
                tab_id = tab_id.as_str(),
                "bridge hello"
            );
            let ack = serde_json::to_string(&ServerMessage::HelloAck { client_id }).ok()?;
            let snapshot = vault_snapshot_message(vault);
            match snapshot {
                Some(s) => Some(format!("{ack}\n{s}")),
                None => Some(ack),
            }
        }
        ClientMessage::Anonymize { id, text } => {
            debug!(client_id, req_id = %id, msg_type = "anonymize", text_len = text.len(), "ws-msg-in");
            let reply = run_text_op(
                command_tx,
                id.clone(),
                Source::Browser { client_id },
                true,
                text,
            )
            .await;
            let out = serialize_reply(id.clone(), reply);
            debug!(client_id, req_id = %id, msg_type = "anonymize-reply", body_len = out.len(), "ws-msg-out");
            Some(out)
        }
        ClientMessage::AnonymizeFile { id, filename, data } => {
            debug!(client_id, req_id = %id, msg_type = "anonymize_file", filename_len = filename.len(), data_len = data.len(), "ws-msg-in");
            let out = run_file_op(
                command_tx,
                id.clone(),
                Source::Browser { client_id },
                filename,
                data,
            )
            .await;
            debug!(client_id, req_id = %id, msg_type = "anonymize-file-reply", body_len = out.len(), "ws-msg-out");
            Some(out)
        }
        ClientMessage::Restore { id, text } => {
            debug!(client_id, req_id = %id, msg_type = "restore", text_len = text.len(), "ws-msg-in");
            let reply = run_text_op(
                command_tx,
                id.clone(),
                Source::Browser { client_id },
                false,
                text,
            )
            .await;
            let out = serialize_reply(id.clone(), reply);
            debug!(client_id, req_id = %id, msg_type = "restore-reply", body_len = out.len(), "ws-msg-out");
            Some(out)
        }
        ClientMessage::VaultGet { id: _ } => {
            let snap = vault_snapshot_message(vault);
            debug!(
                client_id,
                kind = "vault-snapshot",
                body_len = snap.as_ref().map_or(0, std::string::String::len),
                "ws-msg-out"
            );
            snap
        }
        ClientMessage::VaultClear { id } => {
            debug!(client_id, req_id = %id, "ws-msg-in vault_clear");
            let cmd = Command::ClearVault { req_id: id };
            let tx = command_tx.clone();
            let _ = tokio::task::spawn_blocking(move || tx.try_send(cmd)).await;
            None
        }
        ClientMessage::Ping {} => {
            debug!(client_id, "ws-ping");
            serde_json::to_string(&ServerMessage::Pong {}).ok()
        }
    }
}

fn origin_allowed(origin: &str, dev_extensions: bool) -> bool {
    if origin.is_empty() {
        return false;
    }
    if dev_extensions {
        return ALLOWED_ORIGIN_PREFIXES_DEV
            .iter()
            .any(|prefix| origin.starts_with(prefix));
    }
    if PUBLISHED_EXTENSION_ID.is_empty() {
        return false;
    }
    let pinned = format!("chrome-extension://{PUBLISHED_EXTENSION_ID}");
    origin == pinned || origin == format!("{pinned}/")
}

fn vault_snapshot_message(vault: &Arc<Mutex<Vault>>) -> Option<String> {
    let snapshot = vault
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let entries: Vec<VaultEntrySer<'_>> = snapshot
        .entries
        .iter()
        .map(|e| VaultEntrySer {
            category: e.category.as_str(),
            real: e.real.as_str(),
            fake: e.fake.as_str(),
        })
        .collect();
    serde_json::to_string(&ServerMessage::Vault { entries: &entries }).ok()
}

async fn run_text_op(
    command_tx: &StdSyncSender<Command>,
    req_id: String,
    source: Source,
    anonymize: bool,
    text: String,
) -> Result<BridgeReply, String> {
    let (reply_tx, reply_rx) = sync_channel::<BridgeReply>(1);
    let cmd = if anonymize {
        Command::AnonymizeText {
            req_id,
            source,
            text,
            reply: reply_tx,
        }
    } else {
        Command::RestoreText {
            req_id,
            source,
            text,
            reply: reply_tx,
        }
    };
    let tx = command_tx.clone();
    let send_result = tokio::task::spawn_blocking(move || tx.try_send(cmd))
        .await
        .map_err(|e| format!("join: {e}"))?;
    if let Err(err) = send_result {
        return Err(format!("engine busy: {err}"));
    }
    let received = tokio::task::spawn_blocking(move || reply_rx.recv_timeout(COMMAND_TIMEOUT))
        .await
        .map_err(|e| format!("join: {e}"))?;
    received.map_err(|e| format!("engine reply: {e}"))
}

async fn run_file_op(
    command_tx: &StdSyncSender<Command>,
    req_id: String,
    source: Source,
    filename: String,
    data_b64: String,
) -> String {
    let filename_for_plan = filename;
    let planned =
        tokio::task::spawn_blocking(move || -> Result<crate::extract::DocPlan, String> {
            let bytes = BASE64_STANDARD
                .decode(data_b64.as_bytes())
                .map_err(|e| format!("base64 decode failed: {e}"))?;
            crate::extract::plan(&bytes, &filename_for_plan).map_err(|e| e.to_string())
        })
        .await;

    let plan = match planned {
        Ok(Ok(plan)) => plan,
        Ok(Err(msg)) => return file_error(req_id, msg),
        Err(join) => return file_error(req_id, format!("plan task failed: {join}")),
    };

    let placements = if plan.text.trim().is_empty() {
        Vec::new()
    } else {
        match run_spans_op(command_tx, req_id.clone(), source, plan.text.clone()).await {
            Ok(BridgeReply::AnonymizedSpans { placements, .. }) => placements,
            Ok(BridgeReply::NoChange { .. }) => Vec::new(),
            Ok(other) => return file_error(req_id, format!("unexpected engine reply: {other:?}")),
            Err(msg) => return file_error(req_id, msg),
        }
    };
    let count = placements.len();

    let finished =
        tokio::task::spawn_blocking(move || -> Result<crate::extract::RewriteOutput, String> {
            plan.finish(&placements).map_err(|e| e.to_string())
        })
        .await;
    let output = match finished {
        Ok(Ok(output)) => output,
        Ok(Err(msg)) => return file_error(req_id, msg),
        Err(join) => return file_error(req_id, format!("rewrite task failed: {join}")),
    };

    let data = BASE64_STANDARD.encode(&output.data);
    serde_json::to_string(&ServerMessage::AnonymizedFile {
        id: req_id,
        data,
        mime: output.mime,
        count,
    })
    .unwrap_or_else(|_| String::from("{\"type\":\"error\"}"))
}

async fn run_spans_op(
    command_tx: &StdSyncSender<Command>,
    req_id: String,
    source: Source,
    text: String,
) -> Result<BridgeReply, String> {
    let (reply_tx, reply_rx) = sync_channel::<BridgeReply>(1);
    let cmd = Command::AnonymizeSpans {
        req_id,
        source,
        text,
        reply: reply_tx,
    };
    let tx = command_tx.clone();
    let send_result = tokio::task::spawn_blocking(move || tx.try_send(cmd))
        .await
        .map_err(|e| format!("join: {e}"))?;
    if let Err(err) = send_result {
        return Err(format!("engine busy: {err}"));
    }
    let received = tokio::task::spawn_blocking(move || reply_rx.recv_timeout(COMMAND_TIMEOUT))
        .await
        .map_err(|e| format!("join: {e}"))?;
    received.map_err(|e| format!("engine reply: {e}"))
}

fn file_error(id: String, message: String) -> String {
    serde_json::to_string(&ServerMessage::Error { id, message })
        .unwrap_or_else(|_| String::from("{\"type\":\"error\"}"))
}

fn serialize_reply(id: String, reply: Result<BridgeReply, String>) -> String {
    let msg = match reply {
        Ok(BridgeReply::Anonymized { text, subs, count }) => ServerMessage::Anonymized {
            id,
            text,
            subs: subs.into_iter().map(|(real, fake)| [fake, real]).collect(),
            count,
        },
        Ok(BridgeReply::AnonymizedSpans { .. }) => ServerMessage::Error {
            id,
            message: "unexpected spans reply on text path".to_string(),
        },
        Ok(BridgeReply::Restored { text, count }) => ServerMessage::Restored { id, text, count },
        Ok(BridgeReply::NoChange { reason }) => ServerMessage::NoChange {
            id,
            reason: no_change_reason_str(reason),
        },
        Ok(BridgeReply::Failed { error }) => ServerMessage::Error { id, message: error },
        Err(err) => ServerMessage::Error { id, message: err },
    };
    serde_json::to_string(&msg).unwrap_or_else(|_| String::from("{\"type\":\"error\"}"))
}

fn no_change_reason_str(reason: NoChangeReason) -> &'static str {
    match reason {
        NoChangeReason::EmptyField => "empty_field",
        NoChangeReason::NoPii => "no_pii",
        NoChangeReason::NothingToRestore => "nothing_to_restore",
        NoChangeReason::NoUndoAvailable => "no_undo_available",
        NoChangeReason::FieldChangedSinceLastOp => "field_changed_since_last_op",
        NoChangeReason::UndoExpired => "undo_expired",
    }
}

fn op_kind_str(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Anonymize => "anonymize",
        OpKind::Restore => "restore",
        OpKind::Undo => "undo",
    }
}

fn event_to_message(event: Event, client_id: u64) -> Option<String> {
    match event {
        Event::VaultDelta { req_id, added } => {
            debug!(client_id, req_id = %req_id, added = added.len(), "event-forward-vault-delta");
            let payload = ServerMessage::VaultDelta {
                added: added.into_iter().map(|(fake, real)| [fake, real]).collect(),
            };
            serde_json::to_string(&payload).ok()
        }
        Event::VaultCleared { req_id, removed } => {
            debug!(client_id, req_id = %req_id, removed, "event-forward-vault-cleared");
            serde_json::to_string(&ServerMessage::VaultCleared { removed }).ok()
        }
        Event::OperationCompleted {
            req_id,
            kind,
            summary,
            source,
        } => {
            let detail = match summary {
                OpSummary::Anonymized { count } | OpSummary::Restored { count } => {
                    format!("count={count}")
                }
                OpSummary::Undone => "undone".into(),
            };
            forward_op_event(&req_id, kind, "completed", detail, source, client_id)
        }
        Event::OperationFailed {
            req_id,
            kind,
            error,
            source,
        } => forward_op_event(&req_id, kind, "failed", error, source, client_id),
        Event::OperationNoChange {
            req_id,
            kind,
            reason,
            source,
        } => forward_op_event(
            &req_id,
            kind,
            "no_change",
            no_change_reason_str(reason).into(),
            source,
            client_id,
        ),
        Event::BackpressureDropped { kind } => serde_json::to_string(&ServerMessage::Event {
            kind: "backpressure",
            op: op_kind_str(kind),
            detail: String::new(),
        })
        .ok(),
        Event::EnginePanicked { error } => serde_json::to_string(&ServerMessage::Event {
            kind: "engine_panicked",
            op: "engine",
            detail: error,
        })
        .ok(),
        _ => None,
    }
}

fn forward_op_event(
    req_id: &str,
    kind: OpKind,
    label: &'static str,
    detail: String,
    source: Source,
    client_id: u64,
) -> Option<String> {
    match source {
        Source::Browser { client_id: target } if target != client_id => {
            debug!(target, client_id, req_id = %req_id, "event-forward-skip-sibling");
            None
        }
        _ => {
            debug!(client_id, req_id = %req_id, label, op = op_kind_str(kind), "event-forward");
            serde_json::to_string(&ServerMessage::Event {
                kind: label,
                op: op_kind_str(kind),
                detail,
            })
            .ok()
        }
    }
}
