#![allow(unsafe_code)]

use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

use include_dir::{Dir, include_dir};
use tiny_skia::Pixmap;
use tracing::error;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
    ReleaseDC, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, ULW_ALPHA,
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

static FRAMES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../assets/lock_frames");

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
        Kind::Anonymize => CLOSE.get_or_init(|| load("close_")),
        Kind::Restore => OPEN.get_or_init(|| load("open_")),
    }
}

fn load(prefix: &str) -> Vec<Pixmap> {
    let mut files: Vec<&include_dir::File> = FRAMES
        .files()
        .filter(|f| {
            f.path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix))
        })
        .collect();
    files.sort_by(|a, b| a.path().cmp(b.path()));
    files
        .into_iter()
        .filter_map(|f| Pixmap::decode_png(f.contents()).ok())
        .collect()
}

fn apply_alpha(src: &Pixmap, dst: &mut Pixmap, alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let src_data = src.data();
    let dst_data = dst.data_mut();
    for i in 0..src_data.len() {
        dst_data[i] = (src_data[i] as f32 * alpha) as u8;
    }
}

fn pump_messages(hwnd: HWND) {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn create_window(x: i32, y: i32) -> windows::core::Result<HWND> {
    register_class();
    let module = unsafe { GetModuleHandleW(PCWSTR::null())? };
    let instance = HINSTANCE(module.0);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE,
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
            RegisterClassW(&wc);
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
        let bmp = CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());
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
            Some(&size),
            Some(mem_dc),
            Some(&src_pt),
            COLORREF(0),
            Some(&blend),
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
