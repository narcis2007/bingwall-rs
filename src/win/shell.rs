//! The tray icon: a hidden window, a notification-area icon drawn at run time,
//! a context menu and an hourly timer. The window procedure only turns clicks
//! and timers into `Action` values and calls the handler given to `run`.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, NOTIFYICONIDENTIFIER,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage,
    ICONINFO, MF_SEPARATOR, MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC, SM_CXSMICON, TPM_BOTTOMALIGN, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_POWERBROADCAST,
    WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_OVERLAPPED,
};

use super::wide;
use crate::preview::Rect;

/// What the user or a timer asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Check Bing for new images (timer, resume from sleep, start-up).
    Sync,
    /// Open the preview popup, or close it if it is open (left-click, menu).
    Preview,
    Next,
    Previous,
    Today,
}

const WM_TRAY: u32 = WM_APP + 1;
const WM_TIP: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
const TIMER_SYNC: usize = 1;
const TIMER_RESUME: usize = 2;
const SYNC_EVERY_MS: u32 = 60 * 60 * 1000;
/// After wake-up, wait a little so the network is back before the check.
const RESUME_DELAY_MS: u32 = 30 * 1000;
const TIP_MAX_UNITS: usize = 127;

const ID_NEXT: usize = 1;
const ID_PREVIOUS: usize = 2;
const ID_TODAY: usize = 3;
const ID_QUIT: usize = 4;
const ID_PREVIEW: usize = 5;

// The window procedure is a plain function, so its state is kept in statics.
static HANDLER: OnceLock<fn(Action)> = OnceLock::new();
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static ICON: AtomicUsize = AtomicUsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static TIP: Mutex<String> = Mutex::new(String::new());

fn dispatch(a: Action) {
    if let Some(h) = HANDLER.get() {
        h(a);
    }
}

/// Change the tooltip. Safe to call from any thread.
pub fn set_tooltip(text: &str) {
    *TIP.lock().unwrap_or_else(PoisonError::into_inner) = text.to_string();
    let hwnd = WINDOW.load(Ordering::Acquire);
    if hwnd != 0 {
        // SAFETY: posting a message to a window handle is allowed from any
        // thread. If the window is already gone the call just fails.
        unsafe { PostMessageW(hwnd as HWND, WM_TIP, 0, 0) };
    }
}

/// Make the tray window and run the message loop until Quit. Returns the exit code.
pub fn run(initial_tip: &str, handler: fn(Action)) -> i32 {
    let _ = HANDLER.set(handler);
    *TIP.lock().unwrap_or_else(PoisonError::into_inner) = initial_tip.to_string();
    // SAFETY: plain Win32 start-up calls. Every pointer passed points to a local
    // that outlives the call; the class name buffer stays alive for the whole function.
    unsafe {
        // Sharp icon and menu on scaled displays. Fails harmlessly if already set.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("bingwall.tray");
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        if RegisterClassExW(&wc) == 0 {
            return 1;
        }
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);
        let size = match GetSystemMetrics(SM_CXSMICON) {
            s if s > 0 => s,
            _ => 16,
        };
        ICON.store(make_icon(size) as usize, Ordering::Relaxed);
        // A top-level window that is never shown. Unlike a message-only window,
        // it gets "TaskbarCreated" and power broadcasts.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("bingwall").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return 1;
        }
        WINDOW.store(hwnd as usize, Ordering::Release);
        notify(hwnd, NIM_ADD, NIF_ICON | NIF_MESSAGE | NIF_TIP);
        SetTimer(hwnd, TIMER_SYNC, SYNC_EVERY_MS, None);
    }
    dispatch(Action::Sync); // the first check, right at start
    // SAFETY: standard message loop. `msg` is a valid MSG; GetMessageW returns
    // 0 on WM_QUIT and -1 on error, and both end the loop.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        msg.wParam as i32
    }
}

/// Where to put a window that belongs to the tray icon, in screen pixels: the
/// icon's own rectangle, or the cursor if Windows cannot say where the icon is.
pub fn anchor() -> Rect {
    let hwnd = WINDOW.load(Ordering::Acquire) as HWND;
    // SAFETY: `id` is fully set and `rc` is a valid out-pointer; the call only
    // reads `id`. An all-zero GUID means "no GUID": the icon is found by window and id.
    let icon = unsafe {
        let id = NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            guidItem: std::mem::zeroed(),
        };
        let mut rc: RECT = std::mem::zeroed();
        let found = !hwnd.is_null() && Shell_NotifyIconGetRect(&id, &mut rc) >= 0;
        (found && rc.right > rc.left && rc.bottom > rc.top).then_some(Rect::new(rc.left, rc.top, rc.right, rc.bottom))
    };
    icon.unwrap_or_else(|| {
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: `pt` is a valid out-pointer.
        unsafe { GetCursorPos(&mut pt) };
        Rect::new(pt.x, pt.y, pt.x + 1, pt.y + 1)
    })
}

/// Add, change or remove the notification-area icon.
fn notify(hwnd: HWND, op: u32, flags: u32) {
    // SAFETY: an all-zero NOTIFYICONDATAW is a valid value (integers and null
    // pointers only). The fields are filled in before the call, and
    // Shell_NotifyIconW only reads the structure.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = ICON_ID;
        nid.uFlags = flags;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = ICON.load(Ordering::Relaxed) as _;
        let tip = TIP.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let units = utf16_prefix(&tip, TIP_MAX_UNITS);
        nid.szTip[..units.len()].copy_from_slice(&units); // the rest stays 0
        Shell_NotifyIconW(op, &nid);
    }
}

/// At most `max` UTF-16 units of `s`, never cutting a surrogate pair in half.
fn utf16_prefix(s: &str, max: usize) -> Vec<u16> {
    let mut out = Vec::new();
    for c in s.chars() {
        if out.len() + c.len_utf16() > max {
            break;
        }
        let mut buf = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut buf));
    }
    out
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lparam as u32) & 0xFFFF {
                WM_LBUTTONUP => dispatch(Action::Preview),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_TIP => {
            notify(hwnd, NIM_MODIFY, NIF_TIP);
            // A worker has just finished: the process is idle again.
            super::trim_working_set();
            0
        }
        WM_TIMER => {
            match wparam {
                TIMER_SYNC => dispatch(Action::Sync),
                TIMER_RESUME => {
                    // SAFETY: `hwnd` is the window that owns this timer.
                    unsafe { KillTimer(hwnd, TIMER_RESUME) };
                    dispatch(Action::Sync);
                }
                _ => {}
            }
            0
        }
        WM_POWERBROADCAST => {
            if wparam == PBT_APMRESUMEAUTOMATIC as usize {
                // SAFETY: `hwnd` is our window; no callback is given.
                unsafe { SetTimer(hwnd, TIMER_RESUME, RESUME_DELAY_MS, None) };
            }
            1 // TRUE
        }
        WM_DESTROY => {
            notify(hwnd, NIM_DELETE, 0);
            // SAFETY: no arguments to check; ends the message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted and lost our icon.
            notify(hwnd, NIM_ADD, NIF_ICON | NIF_MESSAGE | NIF_TIP);
            0
        }
        // SAFETY: forwards the arguments of this very call.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_menu(hwnd: HWND) {
    // SAFETY: the menu handle is checked for null, used only on this thread and
    // destroyed before returning; the strings are NUL-terminated and outlive
    // the calls; `pt` is a valid out-pointer.
    let chosen = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (id, text) in [(ID_PREVIEW, "Preview"), (ID_NEXT, "Next"), (ID_PREVIOUS, "Previous"), (ID_TODAY, "Today")] {
            AppendMenuW(menu, MF_STRING, id, wide(text).as_ptr());
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // The menu closes when the user clicks elsewhere only if our window is
        // in the foreground. WM_NULL afterwards is the documented companion.
        SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        id as usize
    };
    super::trim_working_set();
    match chosen {
        ID_PREVIEW => dispatch(Action::Preview),
        ID_NEXT => dispatch(Action::Next),
        ID_PREVIOUS => dispatch(Action::Previous),
        ID_TODAY => dispatch(Action::Today),
        // SAFETY: `hwnd` is our window and we are on its thread.
        ID_QUIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ => {}
    }
}

/// A small landscape: sky, sun and two hills, on a round tile.
/// Pixels are either fully opaque or fully transparent.
fn tile_pixel(x: usize, y: usize, n: usize) -> Option<u32> {
    let u = (x as f32 + 0.5) / n as f32;
    let v = (y as f32 + 0.5) / n as f32;
    let (dx, dy) = (u - 0.5, v - 0.5);
    if dx * dx + dy * dy > 0.25 {
        return None; // outside the round tile
    }
    let (sx, sy) = (u - 0.68, v - 0.34);
    let rgb = if v >= 0.46 + (u - 0.30).abs() * 1.1 {
        0x1B3A6B // near hill
    } else if v >= 0.60 + (u - 0.72).abs() * 1.0 {
        0x3A66A8 // far hill
    } else if sx * sx + sy * sy <= 0.15 * 0.15 {
        0xFFD23F // sun
    } else if v < 0.5 {
        0x4AA8F0 // upper sky
    } else {
        0x9CD3FA // lower sky
    };
    Some(0xFF00_0000 | rgb)
}

fn make_icon(size: i32) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    let n = size.max(1) as usize;
    let mut color = vec![0u32; n * n]; // 0xAARRGGBB, which is BGRA in memory
    let stride = n.div_ceil(16) * 2; // bytes per row of the 1-bit mask (16-bit aligned)
    let mut mask = vec![0xFFu8; stride * n]; // 1 = transparent
    for y in 0..n {
        for x in 0..n {
            if let Some(px) = tile_pixel(x, y, n) {
                color[y * n + x] = px;
                mask[y * stride + x / 8] &= !(0x80u8 >> (x % 8));
            }
        }
    }
    // SAFETY: both buffers are at least as large as the bitmaps they describe
    // and live until after the calls. CreateIconIndirect copies the bitmaps, so
    // they are deleted right after it.
    unsafe {
        let hbm_color = CreateBitmap(size, size, 1, 32, color.as_ptr().cast());
        let hbm_mask = CreateBitmap(size, size, 1, 1, mask.as_ptr().cast());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: hbm_mask, hbmColor: hbm_color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(hbm_color);
        DeleteObject(hbm_mask);
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_is_cut_on_a_character_boundary() {
        assert_eq!(utf16_prefix("abc", 127), "abc".encode_utf16().collect::<Vec<_>>());
        assert_eq!(utf16_prefix("abcdef", 3).len(), 3);
        // U+1F600 takes two UTF-16 units; it must not be cut in half.
        let s = "ab\u{1F600}";
        assert_eq!(utf16_prefix(s, 3).len(), 2);
        assert_eq!(utf16_prefix(s, 4).len(), 4);
        assert!(utf16_prefix("", 5).is_empty());
    }

    #[test]
    fn icon_has_a_round_transparent_corner_and_opaque_centre() {
        for n in [16usize, 24, 32] {
            assert_eq!(tile_pixel(0, 0, n), None);
            assert_eq!(tile_pixel(n - 1, n - 1, n), None);
            assert!(tile_pixel(n / 2, n / 2, n).is_some_and(|p| p >> 24 == 0xFF));
        }
    }
}
