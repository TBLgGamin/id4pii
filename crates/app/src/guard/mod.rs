mod automation;
mod popup;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use tracing::{error, info};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use wry::{WebView, WebViewBuilder};

use self::session::Session;

enum UserEvent {
    ShowPopup { text: String, x: i32, y: i32 },
    ClosePopup(WindowId),
}

#[derive(Args)]
pub(crate) struct GuardArgs {
    #[arg(long, env = "ID4PII_MODEL", default_value = "model")]
    model: PathBuf,
    #[arg(long, default_value = "onnx/model_q4.onnx")]
    model_file: String,
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

pub(crate) fn run(args: &GuardArgs) -> Result<()> {
    let session = Session::load(&args.model, &args.model_file, args.threads)?;
    let session = Arc::new(Mutex::new(session));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

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
        .with_tooltip(
            "id4pii guard — Ctrl+Shift+A anonymize/restore field, Ctrl+Shift+D restore selection",
        )
        .with_menu(Box::new(menu))
        .with_icon(tray_icon())
        .build()
        .context("failed to create the tray icon")?;

    let hotkey_events = GlobalHotKeyEvent::receiver();
    let menu_events = MenuEvent::receiver();
    let mut popups: HashMap<WindowId, (Window, WebView)> = HashMap::new();

    info!(
        "id4pii guard running — Ctrl+Shift+A anonymizes/restores the focused field, Ctrl+Shift+D restores the selection in a popup"
    );

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(120));

        match event {
            Event::UserEvent(UserEvent::ShowPopup { text, x, y }) => {
                match build_popup(target, &proxy, &text, x, y) {
                    Ok((window, webview)) => {
                        popups.insert(window.id(), (window, webview));
                    }
                    Err(error) => error!("failed to show popup: {error}"),
                }
            }
            Event::UserEvent(UserEvent::ClosePopup(id)) => {
                popups.remove(&id);
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                popups.remove(&window_id);
            }
            _ => {}
        }

        while let Ok(event) = hotkey_events.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if event.id == toggle_key.id() {
                spawn_toggle(Arc::clone(&session));
            } else if event.id == restore_key.id() {
                spawn_restore_selection(Arc::clone(&session), proxy.clone());
            }
        }
        while let Ok(event) = menu_events.try_recv() {
            if event.id == quit_item.id() {
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}

fn spawn_toggle(session: Arc<Mutex<Session>>) {
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
            if guard.contains_surrogates(&text) {
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

fn spawn_restore_selection(session: Arc<Mutex<Session>>, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let text = match automation::read_selection() {
            Ok(text) => text,
            Err(error) => {
                error!("restore: {error}");
                return;
            }
        };
        let restored = {
            let guard = session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.deanonymize(&text)
        };
        let (x, y) = automation::cursor_position();
        if proxy
            .send_event(UserEvent::ShowPopup {
                text: restored,
                x,
                y,
            })
            .is_err()
        {
            error!("restore: the event loop is closed");
        }
    });
}

fn build_popup(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: &EventLoopProxy<UserEvent>,
    text: &str,
    x: i32,
    y: i32,
) -> Result<(Window, WebView)> {
    let window = WindowBuilder::new()
        .with_title("id4pii")
        .with_inner_size(LogicalSize::new(460.0, 250.0))
        .with_position(PhysicalPosition::new(x + 12, y + 12))
        .with_decorations(false)
        .with_always_on_top(true)
        .with_resizable(false)
        .build(target)
        .context("failed to create the popup window")?;

    let id = window.id();
    let close_proxy = proxy.clone();
    let copy_text = text.to_string();
    let webview = WebViewBuilder::new()
        .with_html(popup::page(text))
        .with_ipc_handler(move |request| match request.body().as_str() {
            "close" => {
                let _ = close_proxy.send_event(UserEvent::ClosePopup(id));
            }
            "copy" => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(copy_text.clone());
                }
            }
            _ => {}
        })
        .build(&window)
        .context("failed to create the popup webview")?;

    Ok((window, webview))
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
