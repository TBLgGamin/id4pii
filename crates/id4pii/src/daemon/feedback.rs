#![allow(unsafe_code)]
#![allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]

use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

use resvg::usvg;
use tiny_skia::Pixmap;
use tracing::error;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Anonymize,
    Restore,
}

const W: i32 = 200;
const H: i32 = 160;
const FRAME_MS: u64 = 16;
const HOLD_MS: u64 = 120;
const FADE_MS: u64 = 200;
const FRAME_COUNT: usize = 28;
const OPEN_ANGLE: f32 = -46.0;
const ACCENT: &str = "#F5A524";
const HOLE: &str = "#7A4E06";

pub(crate) fn show(kind: Kind, x: i32, y: i32) {
    std::thread::spawn(move || {
        if let Err(err) = run(kind, x, y) {
            error!("feedback render failed: {err:?}");
        }
    });
}

fn run(kind: Kind, x: i32, y: i32) -> windows::core::Result<()> {
    let frames = frames_for(kind);
    let hwnd = create_window(x, y)?;

    let start = Instant::now();
    for (i, frame) in frames.iter().enumerate() {
        pump_messages(hwnd);
        let _ = push(hwnd, frame);
        let target = (i as u64 + 1) * FRAME_MS;
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed < target {
            std::thread::sleep(Duration::from_millis(target - elapsed));
        }
    }

    pump_messages(hwnd);
    std::thread::sleep(Duration::from_millis(HOLD_MS));

    if let Some(last) = frames.last() {
        let mut buf = last.clone();
        let fade_start = Instant::now();
        loop {
            pump_messages(hwnd);
            let elapsed = fade_start.elapsed().as_millis() as u64;
            if elapsed >= FADE_MS {
                break;
            }
            let alpha = 1.0 - (elapsed as f32 / FADE_MS as f32);
            apply_alpha(last, &mut buf, alpha);
            let _ = push(hwnd, &buf);
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

fn frames_for(kind: Kind) -> &'static [Pixmap] {
    static CLOSE: OnceLock<Vec<Pixmap>> = OnceLock::new();
    static OPEN: OnceLock<Vec<Pixmap>> = OnceLock::new();
    match kind {
        Kind::Anonymize => CLOSE.get_or_init(|| render_frames(true)),
        Kind::Restore => OPEN.get_or_init(|| render_frames(false)),
    }
}

fn render_frames(closing: bool) -> Vec<Pixmap> {
    (0..FRAME_COUNT)
        .filter_map(|i| {
            let t = if FRAME_COUNT <= 1 {
                1.0
            } else {
                i as f32 / (FRAME_COUNT as f32 - 1.0)
            };
            let scale = 0.55 + 0.45 * ease_out_back(t);
            let angle = if closing {
                OPEN_ANGLE + (-OPEN_ANGLE) * ease_out_back(t)
            } else {
                OPEN_ANGLE * ease_out_cubic(t)
            };
            render_svg(angle, scale)
        })
        .collect()
}

fn render_svg(angle: f32, scale: f32) -> Option<Pixmap> {
    let svg = lock_svg(angle, scale);
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = Pixmap::new(W as u32, H as u32)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Some(pixmap)
}

fn lock_svg(angle: f32, scale: f32) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="-8 -4 40 32"><g transform="translate(12 12) scale({scale}) translate(-12 -12)"><g transform="rotate({angle} 16.5 11)"><path d="M7.5 11 V8 a4.5 4.5 0 0 1 9 0 V11" fill="none" stroke="{ACCENT}" stroke-width="2.4" stroke-linecap="round"/></g><rect x="4.5" y="10.5" width="15" height="11" rx="2.6" fill="{ACCENT}"/><circle cx="12" cy="15" r="1.7" fill="{HOLE}"/><rect x="11.2" y="15" width="1.6" height="3.4" rx="0.8" fill="{HOLE}"/></g></svg>"#
    )
}

fn ease_out_back(t: f32) -> f32 {
    let c1 = 1.9_f32;
    let c3 = c1 + 1.0;
    let u = t - 1.0;
    1.0 + c3 * u * u * u + c1 * u * u
}

fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}

fn apply_alpha(src: &Pixmap, dst: &mut Pixmap, alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let src_data = src.data();
    let dst_data = dst.data_mut();
    for i in 0..src_data.len() {
        dst_data[i] = (f32::from(src_data[i]) * alpha) as u8;
    }
}

fn pump_messages(hwnd: HWND) {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&raw mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

fn create_window(x: i32, y: i32) -> windows::core::Result<HWND> {
    register_class();
    let module = unsafe { GetModuleHandleW(PCWSTR::null())? };
    let instance = HINSTANCE(module.0);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("id4pii_feedback"),
            PCWSTR::null(),
            WS_POPUP,
            x,
            y,
            W,
            H,
            None,
            None,
            Some(instance),
            None,
        )?
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(hwnd)
}

static REGISTER: Once = Once::new();

fn register_class() {
    REGISTER.call_once(|| {
        let module = unsafe { GetModuleHandleW(PCWSTR::null()).unwrap_or_default() };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: w!("id4pii_feedback"),
            ..Default::default()
        };
        unsafe {
            RegisterClassW(&raw const wc);
        }
    });
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn push(hwnd: HWND, pixmap: &Pixmap) -> windows::core::Result<()> {
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let pixels = pixmap.data();

    let mut bgra: Vec<u8> = Vec::with_capacity(pixels.len());
    for chunk in pixels.chunks_exact(4) {
        bgra.push(chunk[2]);
        bgra.push(chunk[1]);
        bgra.push(chunk[0]);
        bgra.push(chunk[3]);
    }

    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(
            Some(mem_dc),
            &raw const bmi,
            DIB_RGB_COLORS,
            &raw mut bits,
            None,
            0,
        )?;
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), bgra.len());
        }
        let old = SelectObject(mem_dc, bmp.into());

        let size = windows::Win32::Foundation::SIZE {
            cx: width,
            cy: height,
        };
        let src_pt = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let result = UpdateLayeredWindow(
            hwnd,
            Some(screen_dc),
            None,
            Some(&raw const size),
            Some(mem_dc),
            Some(&raw const src_pt),
            COLORREF(0),
            Some(&raw const blend),
            ULW_ALPHA,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        result?;
    }
    Ok(())
}
