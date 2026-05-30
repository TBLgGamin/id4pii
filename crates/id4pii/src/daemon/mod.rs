mod automation;
mod bridge;
mod bus;
mod engine;
mod feedback;
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tracing::{error, info, warn};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use self::bus::{Command, EngineStatus, Event, EventBus, OpKind, Source};
use self::engine::{Engine, EngineConfig};
use self::store::DpapiStore;

#[derive(Args, Debug)]
pub(crate) struct DaemonArgs {
    #[arg(long, env = "ID4PII_MODEL", default_value_os_t = crate::model_dir::default_dir())]
    pub(crate) model: PathBuf,
    #[arg(long, default_value = crate::model_dir::DEFAULT_MODEL_FILE)]
    pub(crate) model_file: String,
    #[arg(long, default_value_t = 0)]
    pub(crate) threads: usize,
    #[arg(long, default_value_t = 0.0)]
    pub(crate) min_score: f32,
    #[arg(long, default_value_t = 0)]
    pub(crate) max_vault_entries: usize,
    #[arg(long, default_value_t = 7878)]
    pub(crate) bridge_port: u16,
    #[arg(long)]
    pub(crate) no_bridge: bool,
    #[arg(long)]
    pub(crate) dev_extensions: bool,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn run(args: &DaemonArgs) -> Result<()> {
    let progress_bar = crate::progress::install_bar();

    std::thread::Builder::new()
        .name("id4pii-pool-warmup".into())
        .spawn(crate::warm_up_pools)
        .ok();

    let mut bus = EventBus::new();
    let feedback_rx = bus.subscribe();
    let stats_rx = bus.subscribe();
    let bridge_rx = if args.no_bridge {
        None
    } else {
        Some(bus.subscribe())
    };
    let bus = Arc::new(bus);

    crate::model_setup::ensure_model(&args.model, &args.model_file)?;

    let store_path = DpapiStore::default_path()?;
    let store: Arc<dyn store::VaultStore> = Arc::new(DpapiStore::new(store_path));
    let status = Arc::new(EngineStatus::new());
    let engine = Engine::load(
        &args.model,
        &args.model_file,
        args.threads,
        Arc::clone(&store) as Arc<dyn store::VaultStore>,
        Arc::clone(&bus),
        Arc::clone(&status),
        EngineConfig {
            min_score: args.min_score,
            max_vault_entries: args.max_vault_entries,
        },
    )?;
    let engine_vault_handle = engine.vault_handle();

    let (command_tx, command_rx) = sync_channel::<Command>(1);

    let engine_handle = std::thread::Builder::new()
        .name("id4pii-engine".into())
        .spawn(move || engine.run(&command_rx))
        .context("failed to spawn engine thread")?;

    spawn_feedback_adapter(feedback_rx);
    spawn_stats_recorder(stats_rx, Arc::clone(&status));
    spawn_status_watchdog(Arc::clone(&status), progress_bar.clone());

    if let Some(rx) = bridge_rx {
        let vault_handle = engine_vault_handle.clone();
        if let Err(err) = bridge::spawn(
            args.bridge_port,
            args.dev_extensions,
            command_tx.clone(),
            rx,
            vault_handle,
        ) {
            warn!("bridge failed to start: {err}");
        }
    }

    let event_loop = EventLoopBuilder::<()>::with_user_event().build();

    let manager = GlobalHotKeyManager::new().context("failed to create the hotkey manager")?;
    let anonymize_key = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyA);
    let restore_key = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyZ);
    let undo_key = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyU);
    register_hotkey(&manager, &bus, anonymize_key, "Ctrl+Shift+A");
    register_hotkey(&manager, &bus, restore_key, "Ctrl+Shift+Z");
    register_hotkey(&manager, &bus, undo_key, "Ctrl+Shift+U");
    let _manager = manager;

    let menu = Menu::new();
    let bridge_label = if args.no_bridge {
        "Browser bridge: disabled".to_string()
    } else {
        format!("Browser bridge: ws://127.0.0.1:{}/ws", args.bridge_port)
    };
    let bridge_item = MenuItem::new(bridge_label, false, None);
    let log_dir = crate::paths::log_dir();
    let log_file_path = log_dir.as_ref().map(|d| d.join("daemon.log"));
    let show_log_item = MenuItem::new("Open log file", log_file_path.is_some(), None);
    let open_log_folder_item = MenuItem::new("Open log folder", log_dir.is_some(), None);
    let quit_item = MenuItem::new("Quit id4pii daemon", true, None);
    menu.append(&bridge_item)
        .context("failed to build the tray menu")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("failed to build the tray menu")?;
    menu.append(&show_log_item)
        .context("failed to build the tray menu")?;
    menu.append(&open_log_folder_item)
        .context("failed to build the tray menu")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("failed to build the tray menu")?;
    menu.append(&quit_item)
        .context("failed to build the tray menu")?;
    let tooltip = if args.no_bridge {
        "id4pii daemon — Ctrl+Shift+A anonymize, Ctrl+Shift+Z restore, Ctrl+Shift+U undo"
            .to_string()
    } else {
        format!(
            "id4pii daemon — Ctrl+Shift+A/Z/U, browser bridge on :{}",
            args.bridge_port
        )
    };
    let _tray = TrayIconBuilder::new()
        .with_tooltip(tooltip)
        .with_menu(Box::new(menu))
        .with_icon(tray_icon())
        .build()
        .context("failed to create the tray icon")?;

    let hotkey_events = GlobalHotKeyEvent::receiver();
    let menu_events = MenuEvent::receiver();

    info!(
        "id4pii daemon running — Ctrl+Shift+A anonymize, Ctrl+Shift+Z restore, Ctrl+Shift+U undo"
    );

    let mut event_loop = event_loop;
    let loop_tx = command_tx.clone();
    event_loop.run_return(move |_event, _target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(120));

        while let Ok(event) = hotkey_events.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let cursor = automation::cursor_position();
            let source = Source::Hotkey { cursor };
            let kind = if event.id == anonymize_key.id() {
                OpKind::Anonymize
            } else if event.id == restore_key.id() {
                OpKind::Restore
            } else if event.id == undo_key.id() {
                OpKind::Undo
            } else {
                continue;
            };
            status.record_received();
            let req_id = fresh_req_id();
            let command = match kind {
                OpKind::Anonymize => Command::Anonymize { req_id, source },
                OpKind::Restore => Command::Restore { req_id, source },
                OpKind::Undo => Command::Undo { req_id, source },
            };
            match loop_tx.try_send(command) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    match status.snapshot() {
                        Some((cur_kind, step, elapsed)) => {
                            info!(
                                "backpressure: dropped {kind:?} (engine busy with {cur_kind:?} step={step} elapsed_ms={})",
                                elapsed.as_millis()
                            );
                        }
                        None => {
                            info!("backpressure: dropped {kind:?} (engine busy, no active op)");
                        }
                    }
                    bus.publish(&Event::BackpressureDropped { kind });
                }
                Err(TrySendError::Disconnected(_)) => {
                    error!("engine channel disconnected");
                    *control_flow = ControlFlow::Exit;
                }
            }
        }
        while let Ok(event) = menu_events.try_recv() {
            if event.id == quit_item.id() {
                let _ = loop_tx.try_send(Command::Shutdown);
                *control_flow = ControlFlow::Exit;
            } else if event.id == show_log_item.id() {
                if let Some(path) = log_file_path.as_ref() {
                    open_with_shell(path);
                }
            } else if event.id == open_log_folder_item.id()
                && let Some(dir) = log_dir.as_ref() {
                    open_with_shell(dir);
                }
        }
    });

    drop(command_tx);
    if let Err(err) = engine_handle.join() {
        error!("engine thread panicked at shutdown: {err:?}");
    }
    crate::progress::finish_bar();
    Ok(())
}

fn register_hotkey(
    manager: &GlobalHotKeyManager,
    bus: &EventBus,
    key: HotKey,
    combo: &'static str,
) {
    if let Err(err) = manager.register(key) {
        warn!("failed to register {combo}: {err}");
        bus.publish(&Event::HotkeyRegistrationFailed {
            combo,
            error: err.to_string(),
        });
    }
}

#[allow(clippy::expect_used)]
fn spawn_status_watchdog(status: Arc<EngineStatus>, bar: indicatif::ProgressBar) {
    std::thread::Builder::new()
        .name("id4pii-watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let counters = status.stats();
                let descriptor = match status.snapshot() {
                    Some((kind, step, elapsed)) => {
                        format!("busy {kind:?} step={step} {}ms", elapsed.as_millis())
                    }
                    None => match counters.last_complete {
                        Some(t) => format!("idle (last {}s ago)", t.elapsed().as_secs()),
                        None => "idle".to_string(),
                    },
                };
                bar.set_message(format!(
                    "{descriptor} │ recv={} a={} r={} u={} nc={} fail={} dropped={}",
                    counters.received,
                    counters.anonymized,
                    counters.restored,
                    counters.undone,
                    counters.no_change,
                    counters.failed,
                    counters.dropped
                ));
            }
        })
        .expect("failed to spawn watchdog");
}

#[allow(clippy::expect_used)]
fn spawn_stats_recorder(rx: Receiver<Event>, status: Arc<EngineStatus>) {
    std::thread::Builder::new()
        .name("id4pii-stats".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    Event::OperationCompleted { kind, .. } => status.record_completed(kind),
                    Event::OperationNoChange { .. } => status.record_no_change(),
                    Event::OperationFailed { .. } => status.record_failed(),
                    Event::BackpressureDropped { .. } => status.record_dropped(),
                    Event::Shutdown => break,
                    _ => {}
                }
            }
        })
        .expect("failed to spawn stats recorder");
}

#[allow(clippy::expect_used)]
fn spawn_feedback_adapter(rx: Receiver<Event>) {
    std::thread::Builder::new()
        .name("id4pii-feedback-adapter".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::OperationCompleted {
                    kind,
                    source: Source::Hotkey { cursor },
                    ..
                } = event
                {
                    let feedback_kind = match kind {
                        OpKind::Anonymize => feedback::Kind::Anonymize,
                        OpKind::Restore | OpKind::Undo => feedback::Kind::Restore,
                    };
                    feedback::show(feedback_kind, cursor.0 - 70, cursor.1 - 50);
                }
            }
        })
        .expect("failed to spawn feedback adapter");
}

#[allow(clippy::expect_used)]
fn tray_icon() -> Icon {
    static ICON_PNG: &[u8] = include_bytes!("../../../../assets/icon-32.png");
    match tiny_skia::Pixmap::decode_png(ICON_PNG) {
        Ok(pixmap) => {
            let width = pixmap.width();
            let height = pixmap.height();
            Icon::from_rgba(pixmap.data().to_vec(), width, height)
                .unwrap_or_else(|_| fallback_icon())
        }
        Err(_) => fallback_icon(),
    }
}

fn open_with_shell(path: &std::path::Path) {
    if let Err(err) = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn()
    {
        warn!("could not open {}: {err}", path.display());
    }
}

fn fresh_req_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mixed = t.wrapping_add(c.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    format!("{:08x}", mixed as u32)
}

#[allow(clippy::expect_used)]
fn fallback_icon() -> Icon {
    let size = 32_u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..size * size {
        rgba.extend_from_slice(&[0x2E, 0xB8, 0x8A, 0xFF]);
    }
    Icon::from_rgba(rgba, size, size).expect("32x32 RGBA icon is valid")
}
