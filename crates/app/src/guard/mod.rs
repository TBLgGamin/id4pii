mod automation;
mod session;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tracing::{error, info};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use self::session::Session;

#[derive(Args)]
pub(crate) struct GuardArgs {
    #[arg(long, env = "ID4PII_MODEL", default_value = "model")]
    model: PathBuf,
    #[arg(long, default_value = "onnx/model_q4.onnx")]
    model_file: String,
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Clone, Copy)]
enum Mode {
    Toggle,
    Restore,
}

pub(crate) fn run(args: &GuardArgs) -> Result<()> {
    let session = Session::load(&args.model, &args.model_file, args.threads)?;
    let session = Arc::new(Mutex::new(session));

    let event_loop = EventLoopBuilder::new().build();

    let manager = GlobalHotKeyManager::new().context("failed to create the hotkey manager")?;
    let toggle_key = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyA);
    let restore_key = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyD);
    manager
        .register(toggle_key)
        .context("failed to register Ctrl+Shift+A")?;
    manager
        .register(restore_key)
        .context("failed to register Ctrl+Shift+D")?;
    let _manager = manager;

    let menu = Menu::new();
    let quit_item = MenuItem::new("Quit id4pii guard", true, None);
    menu.append(&quit_item)
        .context("failed to build the tray menu")?;
    let _tray = TrayIconBuilder::new()
        .with_tooltip("id4pii guard — Ctrl+Shift+A anonymize/restore, Ctrl+Shift+D restore")
        .with_menu(Box::new(menu))
        .with_icon(tray_icon())
        .build()
        .context("failed to create the tray icon")?;

    let hotkey_events = GlobalHotKeyEvent::receiver();
    let menu_events = MenuEvent::receiver();

    info!(
        "id4pii guard running — Ctrl+Shift+A anonymizes the focused field (press again to restore), Ctrl+Shift+D always restores"
    );

    event_loop.run(move |_event, _target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(120));

        while let Ok(event) = hotkey_events.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if event.id == toggle_key.id() {
                spawn_worker(Arc::clone(&session), Mode::Toggle);
            } else if event.id == restore_key.id() {
                spawn_worker(Arc::clone(&session), Mode::Restore);
            }
        }
        while let Ok(event) = menu_events.try_recv() {
            if event.id == quit_item.id() {
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}

fn spawn_worker(session: Arc<Mutex<Session>>, mode: Mode) {
    std::thread::spawn(move || {
        let text = match automation::read_focused() {
            Ok(text) => text,
            Err(error) => {
                error!("{error}");
                return;
            }
        };
        if text.trim().is_empty() {
            return;
        }

        let (output, action) = {
            let mut guard = session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let restore = matches!(mode, Mode::Restore) || guard.contains_surrogates(&text);
            if restore {
                (guard.deanonymize(&text), "restored real values in")
            } else {
                match guard.anonymize(&text) {
                    Ok((anonymized, _)) => (anonymized, "anonymized PII in"),
                    Err(error) => {
                        error!("anonymize: {error}");
                        return;
                    }
                }
            }
        };

        if output == text {
            info!("nothing to change in the focused field");
            return;
        }
        if let Err(error) = automation::write_focused(&output) {
            error!("write-back failed: {error}");
        } else {
            info!("{action} the focused field");
        }
    });
}

#[allow(clippy::expect_used)]
fn tray_icon() -> Icon {
    let size = 32_u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..size * size {
        rgba.extend_from_slice(&[0x2E, 0xB8, 0x8A, 0xFF]);
    }
    Icon::from_rgba(rgba, size, size).expect("32x32 RGBA icon is valid")
}
