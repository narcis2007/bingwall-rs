//! GDI+ through its flat API, used only to decode and scale one picture for the
//! preview popup.
//!
//! `gdiplus.dll` is loaded with `LoadLibraryExW` when a session starts and
//! freed when it ends. It is not linked at build time (the `windows-sys`
//! declarations would put it in the import table, so Windows would load it when
//! the program starts). While the tray is idle, GDI+ is not in the process.

use std::ffi::c_void;
use std::path::Path;

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject, FillRect, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows_sys::Win32::Graphics::GdiPlus::{
    GdiplusStartupInput, GdiplusStartupOutput, GpGraphics, GpImage, InterpolationModeHighQualityBicubic, Ok as STATUS_OK,
    PixelOffsetModeHalf, Status,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

use super::wide;
use crate::preview::fit_rect;

type Startup = unsafe extern "system" fn(*mut usize, *const GdiplusStartupInput, *mut GdiplusStartupOutput) -> Status;
type Shutdown = unsafe extern "system" fn(usize);
type LoadImage = unsafe extern "system" fn(*const u16, *mut *mut GpImage) -> Status;
type DisposeImage = unsafe extern "system" fn(*mut GpImage) -> Status;
type ImageSize = unsafe extern "system" fn(*mut GpImage, *mut u32) -> Status;
type CreateGraphics = unsafe extern "system" fn(HDC, *mut *mut GpGraphics) -> Status;
type DeleteGraphics = unsafe extern "system" fn(*mut GpGraphics) -> Status;
type SetMode = unsafe extern "system" fn(*mut GpGraphics, i32) -> Status;
type DrawImage = unsafe extern "system" fn(*mut GpGraphics, *mut GpImage, i32, i32, i32, i32) -> Status;

struct Fns {
    shutdown: Shutdown,
    load_image: LoadImage,
    dispose_image: DisposeImage,
    width: ImageSize,
    height: ImageSize,
    create_graphics: CreateGraphics,
    delete_graphics: DeleteGraphics,
    set_interpolation: SetMode,
    set_pixel_offset: SetMode,
    draw_image: DrawImage,
}

/// Look up the export `name` (which must end in a NUL byte) as a function of type `T`.
///
/// # Safety
/// `T` must be the function pointer type that the export really has.
unsafe fn sym<T: Copy>(lib: HMODULE, name: &'static str) -> Result<T, String> {
    const { assert!(std::mem::size_of::<T>() == std::mem::size_of::<usize>()) };
    debug_assert!(name.ends_with('\0'));
    match GetProcAddress(lib, name.as_ptr()) {
        // SAFETY: by the contract of this function, `T` is a function pointer
        // type, which has the same size as the `FARPROC` that came back.
        Some(p) => Ok(std::mem::transmute_copy::<unsafe extern "system" fn() -> isize, T>(&p)),
        None => Err(format!("GDI+ has no {}", name.trim_end_matches('\0'))),
    }
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        sym($lib, concat!($name, "\0"))?
    };
}

/// A running GDI+ session. Dropping it shuts GDI+ down and unloads the DLL.
pub struct Gdip {
    lib: HMODULE,
    token: usize,
    f: Fns,
}

// SAFETY: the module handle, the token and the function pointers are plain
// process-wide values. The GDI+ flat API may be called from any thread as long
// as one image or graphics object is used by one thread at a time, which
// `render` guarantees: it makes and destroys all its objects itself.
unsafe impl Send for Gdip {}
// SAFETY: see above; `Gdip` has no interior state that a call could change.
unsafe impl Sync for Gdip {}

impl Gdip {
    pub fn start() -> Result<Gdip, String> {
        let name = wide("gdiplus.dll");
        // SAFETY: `name` is NUL-terminated and outlives the call. Only the
        // system folder is searched, so no other program's DLL can be picked up.
        let lib = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if lib.is_null() {
            return Err("GDI+ is not available".to_string());
        }
        // SAFETY: `lib` is the module just loaded.
        let started = unsafe { Gdip::init(lib) };
        if started.is_err() {
            // SAFETY: `lib` is a valid module handle that nothing else uses.
            unsafe { FreeLibrary(lib) };
        }
        started
    }

    /// # Safety
    /// `lib` must be a loaded `gdiplus.dll`. On `Err` the caller still owns `lib`.
    unsafe fn init(lib: HMODULE) -> Result<Gdip, String> {
        let startup: Startup = sym!(lib, "GdiplusStartup");
        let f = Fns {
            shutdown: sym!(lib, "GdiplusShutdown"),
            load_image: sym!(lib, "GdipLoadImageFromFile"),
            dispose_image: sym!(lib, "GdipDisposeImage"),
            width: sym!(lib, "GdipGetImageWidth"),
            height: sym!(lib, "GdipGetImageHeight"),
            create_graphics: sym!(lib, "GdipCreateFromHDC"),
            delete_graphics: sym!(lib, "GdipDeleteGraphics"),
            set_interpolation: sym!(lib, "GdipSetInterpolationMode"),
            set_pixel_offset: sym!(lib, "GdipSetPixelOffsetMode"),
            draw_image: sym!(lib, "GdipDrawImageRectI"),
        };
        let input = GdiplusStartupInput { GdiplusVersion: 1, ..Default::default() };
        let mut output = GdiplusStartupOutput::default();
        let mut token = 0usize;
        // SAFETY: valid in and out pointers; GDI+ version 1 is the one asked for.
        if startup(&mut token, &input, &mut output) != STATUS_OK {
            return Err("GDI+ did not start".to_string());
        }
        Ok(Gdip { lib, token, f })
    }

    /// Decode the picture at `path` and draw it, scaled to fit and centred, on a
    /// new `w` x `h` bitmap that is first filled with `background` (a COLORREF).
    pub fn render(&self, path: &Path, w: i32, h: i32, background: u32) -> Result<Bitmap, String> {
        if w <= 0 || h <= 0 {
            return Err("empty picture area".to_string());
        }
        // SAFETY: plain GDI calls. `info` is fully set before use; every handle
        // made here is checked for null and released before returning, except
        // the bitmap, which the returned `Bitmap` owns (and which is deleted on
        // the error paths when `bitmap` is dropped).
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err("cannot make a drawing surface".to_string());
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            info.bmiHeader.biHeight = -h; // top-down
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut c_void = std::ptr::null_mut();
            let hbm = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if hbm.is_null() {
                DeleteDC(dc);
                return Err("cannot make a bitmap".to_string());
            }
            let bitmap = Bitmap(hbm);
            let old = SelectObject(dc, hbm);
            let drawn = self.draw(dc, path, w, h, background);
            SelectObject(dc, old);
            DeleteDC(dc);
            drawn.map(|()| bitmap)
        }
    }

    /// # Safety
    /// `dc` must be a valid memory device context with a `w` x `h` bitmap selected.
    unsafe fn draw(&self, dc: HDC, path: &Path, w: i32, h: i32, background: u32) -> Result<(), String> {
        let brush = CreateSolidBrush(background);
        let area = RECT { left: 0, top: 0, right: w, bottom: h };
        FillRect(dc, &area, brush);
        DeleteObject(brush);

        let name = wide(path);
        let mut image: *mut GpImage = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated and outlives the call; `image` is a valid out-pointer.
        if (self.f.load_image)(name.as_ptr(), &mut image) != STATUS_OK || image.is_null() {
            return Err("cannot read the picture".to_string());
        }
        let result = self.draw_image_on(dc, image, w, h);
        // The file is not locked any more once the image is gone.
        (self.f.dispose_image)(image);
        result
    }

    /// # Safety
    /// `dc` must be a valid device context and `image` a valid GDI+ image.
    unsafe fn draw_image_on(&self, dc: HDC, image: *mut GpImage, w: i32, h: i32) -> Result<(), String> {
        let (mut iw, mut ih) = (0u32, 0u32);
        if (self.f.width)(image, &mut iw) != STATUS_OK || (self.f.height)(image, &mut ih) != STATUS_OK {
            return Err("cannot read the picture size".to_string());
        }
        let to = fit_rect(iw, ih, w, h);
        let mut graphics: *mut GpGraphics = std::ptr::null_mut();
        if (self.f.create_graphics)(dc, &mut graphics) != STATUS_OK || graphics.is_null() {
            return Err("cannot draw the picture".to_string());
        }
        // Bicubic scaling with the half-pixel offset gives a clean edge; this
        // is the usual setting for shrinking a photo.
        (self.f.set_interpolation)(graphics, InterpolationModeHighQualityBicubic);
        (self.f.set_pixel_offset)(graphics, PixelOffsetModeHalf);
        let status = (self.f.draw_image)(graphics, image, to.left, to.top, to.width(), to.height());
        (self.f.delete_graphics)(graphics); // also flushes the drawing to the bitmap
        if status == STATUS_OK {
            Ok(())
        } else {
            Err("cannot draw the picture".to_string())
        }
    }
}

impl Drop for Gdip {
    fn drop(&mut self) {
        // SAFETY: `token` came from a successful GdiplusStartup and `lib` from
        // LoadLibraryExW; both are released once, here, in this order.
        unsafe {
            (self.f.shutdown)(self.token);
            FreeLibrary(self.lib);
        }
    }
}

/// A GDI bitmap that is deleted when dropped.
pub struct Bitmap(HBITMAP);

impl Bitmap {
    pub fn handle(&self) -> HBITMAP {
        self.0
    }
}

// SAFETY: a GDI bitmap handle is a process-wide value. It is made on a worker
// thread and then used by one thread only (the popup's), never by two at once.
unsafe impl Send for Bitmap {}

impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and is not selected into a
        // device context when it is dropped.
        unsafe { DeleteObject(self.0) };
    }
}
