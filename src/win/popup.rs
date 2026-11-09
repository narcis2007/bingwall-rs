//! The preview popup: a small borderless window that shows the cached pictures
//! one at a time, so the user can pick one before it becomes the wallpaper.
//!
//! The popup lives on its own thread, with its own message loop, and the thread
//! ends when the popup closes. The picture is decoded by GDI+ on a worker thread
//! (a 4K JPEG takes longer than a click should wait); the popup thread only
//! draws with plain GDI. Nothing of this exists while the popup is closed.
//!
//! All sizes and positions come from `crate::preview`, where they are tested.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, FillRect, FrameRect, GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, MonitorFromRect,
    SelectObject, SetBkMode, SetTextColor, UpdateWindow, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DT_CENTER, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW, GetSystemMetrics,
    LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow, ShowWindow, TranslateMessage,
    UnregisterClassW, CS_DBLCLKS, CS_DROPSHADOW, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WA_INACTIVE, WM_ACTIVATE,
    WM_APP, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_PAINT, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::gdip::{Bitmap, Gdip};
use super::wide;
use crate::preview::{self, Hit, Item, Layout, Rect};

/// Everything the popup needs to open.
pub struct Request {
    pub items: Vec<Item>,
    /// The row to show first.
    pub start: usize,
    /// Where the tray icon (or, failing that, the cursor) is, in screen pixels.
    pub anchor: Rect,
    /// Called with the id of the picture when the user chooses "Set as wallpaper".
    pub on_set: fn(&str),
}

const WM_LOADED: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const POPUP_STACK: usize = 512 * 1024;
/// GDI+ and the JPEG decoder need more stack than a message loop does.
const WORKER_STACK: usize = 1024 * 1024;

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const BACKGROUND: u32 = rgb(0x20, 0x20, 0x20);
const PANEL: u32 = rgb(0x2B, 0x2B, 0x2B);
const BORDER: u32 = rgb(0x55, 0x55, 0x55);
const TEXT: u32 = rgb(0xF2, 0xF2, 0xF2);
const MUTED: u32 = rgb(0xA6, 0xA6, 0xA6);
const BUTTON: u32 = rgb(0x3A, 0x3A, 0x3A);
const BUTTON_HOT: u32 = rgb(0x4E, 0x4E, 0x4E);
const ACCENT: u32 = rgb(0x00, 0x78, 0xD4);
const ACCENT_HOT: u32 = rgb(0x24, 0x8F, 0xE6);

// ---- what the tray thread sees ----

/// A popup exists (its thread is running).
static OPEN: AtomicBool = AtomicBool::new(false);
/// Its window, or 0 while it is being made.
static WINDOW: AtomicUsize = AtomicUsize::new(0);
/// When the popup last closed because it lost the focus.
static FOCUS_CLOSED: Mutex<Option<Instant>> = Mutex::new(None);

/// Open the popup, or close it if it is open. Call it on the tray's thread.
/// `make` reads the pictures; it is not called when the popup is only closed.
pub fn toggle(make: impl FnOnce() -> Option<Request>) {
    let focus_closed = *FOCUS_CLOSED.lock().unwrap_or_else(PoisonError::into_inner);
    if preview::ignore_tray_click(focus_closed.map(|t| t.elapsed())) {
        return; // this click is the one that took the focus from the popup
    }
    if OPEN.load(Ordering::Acquire) {
        let hwnd = WINDOW.load(Ordering::Acquire);
        if hwnd != 0 {
            // SAFETY: posting a message to a window handle is allowed from any
            // thread. If the window is already gone the call just fails.
            unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
        }
        return;
    }
    let Some(request) = make() else { return };
    OPEN.store(true, Ordering::Release);
    let spawned = thread::Builder::new().stack_size(POPUP_STACK).spawn(move || {
        run(request);
        WINDOW.store(0, Ordering::Release);
        OPEN.store(false, Ordering::Release);
    });
    if spawned.is_err() {
        OPEN.store(false, Ordering::Release);
    }
}

// ---- the popup thread ----

/// A decoded picture on its way from a worker to the popup.
struct Done {
    index: usize,
    /// `None` if the picture could not be decoded.
    bitmap: Option<Bitmap>,
}

/// Shared by the popup and its workers. The worker leaves the result here and
/// posts `WM_LOADED`; a result nobody picks up is freed with the last owner.
struct Shared {
    hwnd: AtomicUsize,
    done: Mutex<Option<Done>>,
}

enum Shown {
    Nothing,
    Loaded(usize, Bitmap),
    Failed(usize),
}

impl Shown {
    fn index(&self) -> Option<usize> {
        match self {
            Shown::Nothing => None,
            Shown::Loaded(i, _) | Shown::Failed(i) => Some(*i),
        }
    }
}

struct Font(HFONT);

impl Font {
    fn new(face: &str, size: i32, weight: u32, dpi: u32) -> Font {
        // SAFETY: the face name is NUL-terminated and outlives the call. A
        // failed call gives a null handle, which draws with the default font.
        Font(unsafe {
            CreateFontW(
                -preview::scale(size, dpi),
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide(face).as_ptr(),
            )
        })
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the font is owned by this value and not selected into a live device context.
            unsafe { DeleteObject(self.0) };
        }
    }
}

struct Fonts {
    title: Font,
    small: Font,
    button: Font,
    arrows: Font,
}

struct Popup {
    layout: Layout,
    items: Vec<Item>,
    /// The row on show. It is only a position in `items`: browsing changes
    /// nothing on disk, and the list is a snapshot from when the popup opened.
    pos: usize,
    on_set: fn(&str),
    fonts: Fonts,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tracking: bool,
    closing: bool,
    session: Option<Arc<Gdip>>,
    shared: Arc<Shared>,
    /// A worker is decoding. At most one runs at a time.
    busy: bool,
    shown: Shown,
}

thread_local! {
    // The window procedure is a plain function, so the state is kept here (the
    // popup has one thread). Use `with_popup`, which never holds a borrow open
    // across a call that can re-enter the window procedure.
    static POPUP: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

fn with_popup<R>(f: impl FnOnce(&mut Popup) -> R) -> Option<R> {
    POPUP.with(|p| p.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn to_rect(r: Rect) -> RECT {
    RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// The work area and DPI of the monitor that has the anchor.
fn monitor_of(anchor: Rect) -> (Rect, u32) {
    let rc = to_rect(anchor);
    // SAFETY: `rc` and `info` are valid for the calls; `info.cbSize` is set as
    // the API requires; the DPI out-pointers are valid.
    unsafe {
        let monitor = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let work = if GetMonitorInfoW(monitor, &mut info) != 0 {
            let w = info.rcWork;
            Rect::new(w.left, w.top, w.right, w.bottom)
        } else {
            Rect::new(0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
        };
        let (mut dx, mut dy) = (0u32, 0u32);
        let dpi = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) >= 0 && dx > 0 {
            dx
        } else {
            preview::BASE_DPI
        };
        (work, dpi)
    }
}

fn run(request: Request) {
    let (work, dpi) = monitor_of(request.anchor);
    let layout = preview::layout(dpi);
    let (x, y) = preview::place_for_dpi(request.anchor, work, &layout, dpi);
    let shared = Arc::new(Shared { hwnd: AtomicUsize::new(0), done: Mutex::new(None) });
    let start = request.start.min(request.items.len().saturating_sub(1));
    let popup = Popup {
        layout,
        items: request.items,
        pos: start,
        on_set: request.on_set,
        fonts: Fonts {
            title: Font::new("Segoe UI", 15, FW_NORMAL, dpi),
            small: Font::new("Segoe UI", 13, FW_NORMAL, dpi),
            button: Font::new("Segoe UI", 14, FW_SEMIBOLD, dpi),
            arrows: Font::new("Segoe MDL2 Assets", 14, FW_NORMAL, dpi),
        },
        hover: None,
        pressed: None,
        tracking: false,
        closing: false,
        session: Gdip::start().ok().map(Arc::new),
        shared: Arc::clone(&shared),
        busy: false,
        shown: Shown::Nothing,
    };
    POPUP.with(|p| *p.borrow_mut() = Some(popup));

    // SAFETY: plain Win32 calls on this thread. The class name outlives every
    // use; the window is made, shown and destroyed here; the message loop ends
    // when WM_DESTROY posts WM_QUIT.
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("bingwall.popup");
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.style = CS_DROPSHADOW | CS_DBLCLKS;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = class.as_ptr();
        if RegisterClassExW(&wc) != 0 {
            let (w, h) = layout.window;
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST, // no taskbar button
                class.as_ptr(),
                wide("bingwall preview").as_ptr(),
                WS_POPUP,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if !hwnd.is_null() {
                shared.hwnd.store(hwnd as usize, Ordering::Release);
                WINDOW.store(hwnd as usize, Ordering::Release);
                with_popup(|p| p.load());
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
                UpdateWindow(hwnd);
                if GetForegroundWindow() != hwnd {
                    // Windows did not give us the focus. A popup that never had it
                    // would never lose it, and could not be closed with a key.
                    DestroyWindow(hwnd);
                }
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            UnregisterClassW(class.as_ptr(), hinst);
        }
    }
    // Free the fonts, the picture and our hold on GDI+ (the DLL is unloaded
    // here unless a worker is still finishing), then give the pages back.
    let popup = POPUP.with(|p| p.borrow_mut().take());
    drop(popup);
    super::trim_working_set();
}

impl Popup {
    /// Start decoding the picture on show, unless it is ready or a worker is busy.
    /// When the worker finishes, `loaded` looks again, so a quick browser that
    /// has moved on gets the picture it stopped at.
    fn load(&mut self) {
        if self.busy || self.shown.index() == Some(self.pos) {
            return;
        }
        let Some(item) = self.items.get(self.pos) else { return };
        let Some(session) = self.session.clone() else {
            self.shown = Shown::Failed(self.pos);
            return;
        };
        let (index, path) = (self.pos, item.path.clone());
        let (w, h) = (self.layout.image.width(), self.layout.image.height());
        let shared = Arc::clone(&self.shared);
        let job = move || {
            let bitmap = session.render(&path, w, h, PANEL).ok();
            *shared.done.lock().unwrap_or_else(PoisonError::into_inner) = Some(Done { index, bitmap });
            let hwnd = shared.hwnd.load(Ordering::Acquire);
            // SAFETY: posting to a window handle is allowed from any thread; a
            // window that is gone just makes the call fail.
            unsafe { PostMessageW(hwnd as HWND, WM_LOADED, 0, 0) };
            // `session` ends here. If the popup is gone, this was the last hold on GDI+.
        };
        self.busy = thread::Builder::new().stack_size(WORKER_STACK).spawn(job).is_ok();
        if !self.busy {
            self.shown = Shown::Failed(self.pos);
        }
    }

    /// A worker has left a result. Keep it if it is for the picture on show.
    fn loaded(&mut self) {
        let done = self.shared.done.lock().unwrap_or_else(PoisonError::into_inner).take();
        self.busy = false;
        if let Some(Done { index, bitmap }) = done {
            if index == self.pos {
                self.shown = match bitmap {
                    Some(b) => Shown::Loaded(index, b),
                    None => Shown::Failed(index),
                };
            }
        }
        self.load();
    }

    /// Show another row (see `preview::browse`).
    fn go(&mut self, delta: isize) {
        self.pos = preview::browse(self.items.len(), self.pos, delta);
        if self.shown.index() != Some(self.pos) {
            self.shown = Shown::Nothing; // "Loading..." until the new picture is ready
        }
        self.load();
    }

    /// Draw everything into `dc`, which must be a valid device context of the window's size.
    fn draw(&self, dc: HDC) {
        let l = &self.layout;
        let (w, h) = l.window;
        fill(dc, Rect::new(0, 0, w, h), BACKGROUND);
        frame(dc, Rect::new(0, 0, w, h), BORDER);

        match &self.shown {
            Shown::Loaded(i, bitmap) if *i == self.pos => blit(dc, l.image, bitmap),
            Shown::Failed(i) if *i == self.pos => placeholder(dc, l.image, "Image not available", &self.fonts.small),
            _ => placeholder(dc, l.image, "Loading...", &self.fonts.small),
        }

        if let Some(item) = self.items.get(self.pos) {
            let room = l.title.width();
            let title = preview::ellipsize(&item.title, |s| text_width(dc, &self.fonts.title, s) <= room);
            text(dc, &title, l.title, DT_LEFT, &self.fonts.title, TEXT);
            text(dc, &item.date, l.date, DT_LEFT, &self.fonts.small, MUTED);
        }
        let label = preview::position_label(self.pos, self.items.len());
        text(dc, &label, l.position, DT_RIGHT, &self.fonts.small, MUTED);

        let hot = |hit| self.hover == Some(hit);
        let arrow = |hit, r, glyph| {
            let fill_color = if hot(hit) { BUTTON_HOT } else { BUTTON };
            fill(dc, r, fill_color);
            text(dc, glyph, r, DT_CENTER, &self.fonts.arrows, TEXT);
        };
        arrow(Hit::Previous, l.previous, "\u{E76B}"); // ChevronLeft
        arrow(Hit::Next, l.next, "\u{E76C}"); // ChevronRight
        fill(dc, l.set, if hot(Hit::Set) { ACCENT_HOT } else { ACCENT });
        text(dc, "Set as wallpaper", l.set, DT_CENTER, &self.fonts.button, TEXT);
    }
}

// ---- drawing helpers. `dc` is the device context of the paint in progress. ----

fn fill(dc: HDC, r: Rect, color: u32) {
    let rc = to_rect(r);
    // SAFETY: `dc` is valid; the brush is made and deleted here.
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, &rc, brush);
        DeleteObject(brush);
    }
}

fn frame(dc: HDC, r: Rect, color: u32) {
    let rc = to_rect(r);
    // SAFETY: `dc` is valid; the brush is made and deleted here.
    unsafe {
        let brush = CreateSolidBrush(color);
        FrameRect(dc, &rc, brush);
        DeleteObject(brush);
    }
}

/// Copy a bitmap of exactly the size of `r` to `r`.
fn blit(dc: HDC, r: Rect, bitmap: &Bitmap) {
    // SAFETY: `dc` is valid. The bitmap is selected into a private memory
    // device context for the copy and taken out again before that is deleted.
    unsafe {
        let mem = CreateCompatibleDC(dc);
        if mem.is_null() {
            return;
        }
        let old = SelectObject(mem, bitmap.handle());
        BitBlt(dc, r.left, r.top, r.width(), r.height(), mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteDC(mem);
    }
}

fn placeholder(dc: HDC, r: Rect, message: &str, font: &Font) {
    fill(dc, r, PANEL);
    text(dc, message, r, DT_CENTER, font, MUTED);
}

/// One line of text, centred vertically in `r`. `align` is `DT_LEFT`, `DT_CENTER` or `DT_RIGHT`.
fn text(dc: HDC, s: &str, r: Rect, align: u32, font: &Font, color: u32) {
    let w = wide(s);
    let mut rc = to_rect(r);
    // SAFETY: `dc` is valid; `w` holds `w.len() - 1` UTF-16 units plus the NUL
    // and outlives the call; `rc` is a valid in-out rectangle. The old font is
    // selected back before returning.
    unsafe {
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        let old = SelectObject(dc, font.0);
        DrawTextW(dc, w.as_ptr(), (w.len() - 1) as i32, &mut rc, align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old);
    }
}

fn text_width(dc: HDC, font: &Font, s: &str) -> i32 {
    let w = wide(s);
    let mut size = SIZE { cx: 0, cy: 0 };
    // SAFETY: `dc` is valid; `w` outlives the call; `size` is a valid out-pointer.
    unsafe {
        let old = SelectObject(dc, font.0);
        GetTextExtentPoint32W(dc, w.as_ptr(), (w.len() - 1) as i32, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

// ---- the window procedure ----

fn invalidate(hwnd: HWND) {
    // SAFETY: `hwnd` is our window; a null rectangle means all of it, and no
    // erase is asked for (the paint covers every pixel).
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
}

fn close_later(hwnd: HWND) {
    // SAFETY: posting to our own window; if it is gone the call just fails.
    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
}

/// The point of a mouse message, in window pixels.
fn mouse_point(lparam: LPARAM) -> (i32, i32) {
    (i32::from(lparam as i16), i32::from((lparam >> 16) as i16))
}

fn browse(hwnd: HWND, delta: isize) {
    if with_popup(|p| p.go(delta)).is_some() {
        invalidate(hwnd);
    }
}

/// Apply the picture on show, then close. Only this changes the wallpaper.
fn set_wallpaper(hwnd: HWND) {
    let chosen = with_popup(|p| p.items.get(p.pos).map(|i| (p.on_set, i.id.clone()))).flatten();
    if let Some((on_set, id)) = chosen {
        on_set(&id);
        close_later(hwnd);
    }
}

fn on_key(hwnd: HWND, key: u16) {
    match key {
        VK_LEFT => browse(hwnd, preview::PREVIOUS),
        VK_RIGHT => browse(hwnd, preview::NEXT),
        VK_RETURN => set_wallpaper(hwnd),
        VK_ESCAPE => close_later(hwnd),
        _ => {}
    }
}

fn on_button_down(hwnd: HWND, lparam: LPARAM, double: bool) {
    let (x, y) = mouse_point(lparam);
    let hit = with_popup(|p| {
        let hit = p.layout.hit(x, y);
        p.pressed = hit;
        hit
    })
    .flatten();
    if double && hit == Some(Hit::Image) {
        set_wallpaper(hwnd);
    }
}

fn on_button_up(hwnd: HWND, lparam: LPARAM) {
    let (x, y) = mouse_point(lparam);
    let (down, up) = with_popup(|p| (p.pressed.take(), p.layout.hit(x, y))).unwrap_or((None, None));
    if down.is_none() || down != up {
        return; // the press and the release were not on the same button
    }
    match up {
        Some(Hit::Previous) => browse(hwnd, preview::PREVIOUS),
        Some(Hit::Next) => browse(hwnd, preview::NEXT),
        Some(Hit::Set) => set_wallpaper(hwnd),
        _ => {}
    }
}

fn on_mouse_move(hwnd: HWND, lparam: LPARAM) {
    let (x, y) = mouse_point(lparam);
    let state = with_popup(|p| {
        let hit = p.layout.hit(x, y).filter(|h| *h != Hit::Image);
        let changed = p.hover != hit;
        p.hover = hit;
        (changed, !std::mem::replace(&mut p.tracking, true))
    });
    let Some((changed, start_tracking)) = state else { return };
    if start_tracking {
        let mut track = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: `track` is fully set and valid for the call.
        unsafe { TrackMouseEvent(&mut track) };
    }
    if changed {
        invalidate(hwnd);
    }
}

fn on_mouse_leave(hwnd: HWND) {
    let _ = with_popup(|p| {
        p.tracking = false;
        p.hover = None;
    });
    invalidate(hwnd);
}

/// The popup lost the focus: close it. The tray click that caused this (if it
/// did) arrives a moment later; `toggle` ignores it.
fn on_focus_lost(hwnd: HWND) {
    let first = with_popup(|p| !std::mem::replace(&mut p.closing, true)).unwrap_or(false);
    if first {
        *FOCUS_CLOSED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        close_later(hwnd);
    }
}

fn on_paint(hwnd: HWND) {
    // SAFETY: standard paint sequence. `ps` is valid for Begin/EndPaint; the
    // memory device context and bitmap are made and deleted here, and the
    // bitmap is taken out of the context before that.
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        if !dc.is_null() {
            // Draw off screen and copy once, so nothing flickers.
            let size = with_popup(|p| p.layout.window);
            if let Some((w, h)) = size {
                let mem = CreateCompatibleDC(dc);
                let bitmap = CreateCompatibleBitmap(dc, w, h);
                if !mem.is_null() && !bitmap.is_null() {
                    let old = SelectObject(mem, bitmap);
                    with_popup(|p| p.draw(mem));
                    BitBlt(dc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
                    SelectObject(mem, old);
                }
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                if !mem.is_null() {
                    DeleteDC(mem);
                }
            }
        }
        EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            on_paint(hwnd);
            0
        }
        WM_ACTIVATE => {
            if (wparam & 0xFFFF) as u32 == WA_INACTIVE {
                on_focus_lost(hwnd);
            }
            0
        }
        WM_KEYDOWN => {
            on_key(hwnd, wparam as u16);
            0
        }
        WM_LBUTTONDOWN => {
            on_button_down(hwnd, lparam, false);
            0
        }
        WM_LBUTTONDBLCLK => {
            on_button_down(hwnd, lparam, true);
            0
        }
        WM_LBUTTONUP => {
            on_button_up(hwnd, lparam);
            0
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, lparam);
            0
        }
        WM_MOUSELEAVE => {
            on_mouse_leave(hwnd);
            0
        }
        WM_LOADED => {
            if with_popup(|p| p.loaded()).is_some() {
                invalidate(hwnd);
            }
            0
        }
        WM_CLOSE => {
            let _ = with_popup(|p| p.closing = true);
            // SAFETY: `hwnd` is our window and we are on its thread.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: no arguments to check; ends the message loop of this thread.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: forwards the arguments of this very call.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
