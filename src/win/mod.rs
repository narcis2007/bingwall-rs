//! Everything that talks to Windows. All `unsafe` code of the program lives in
//! this module tree; the rest of the crate is safe Rust.

pub mod http;
pub mod shell;

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::IntoRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize};
use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER};

/// UTF-16 with a terminating NUL, for Win32 calls. Non-ASCII paths pass through intact.
pub(crate) fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Set the desktop wallpaper. The fit and style settings are not touched.
pub fn set_wallpaper(path: &Path) -> Result<(), String> {
    let w = wide(path);
    // SAFETY: `w` is a NUL-terminated UTF-16 buffer that lives until the call
    // returns. For SPI_SETDESKWALLPAPER the system only reads from `pvParam`,
    // although the parameter is declared as a mutable pointer.
    let ok = unsafe {
        SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, w.as_ptr() as *mut c_void, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE)
    };
    if ok != 0 {
        Ok(())
    } else {
        // SAFETY: GetLastError has no preconditions.
        Err(format!("could not set the wallpaper (Windows error {})", unsafe { GetLastError() }))
    }
}

/// Hand the pages this process is not using right now back to Windows. They
/// come back on demand. The tray calls this when it goes idle, so the resident
/// size shown in Task Manager stays near the real private memory.
pub fn trim_working_set() {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
    // Two values of -1 ask Windows to trim the working set of that process.
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

/// Owns a kernel handle and closes it on drop.
pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from CreateMutexW and is closed only here.
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub enum Instance {
    /// This process is the only tray instance. Keep the guard alive.
    First(Guard),
    /// Another tray instance of this user session runs already.
    Running,
}

/// Take the named mutex that marks the one running tray instance.
pub fn single_instance() -> Instance {
    let name = wide("Local\\bingwall-tray");
    // SAFETY: `name` is NUL-terminated and outlives the call; a null
    // security-attributes pointer is allowed.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: GetLastError has no preconditions. It is read right after
    // CreateMutexW, before any other call can change it.
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        if !handle.is_null() {
            // SAFETY: `handle` is a valid handle that we own.
            unsafe { CloseHandle(handle) };
        }
        Instance::Running
    } else {
        // A null handle means the mutex could not be made. Then we run anyway.
        Instance::First(Guard(handle))
    }
}

/// The program has the GUI subsystem, so it has no console of its own. Attach
/// to the console of the parent shell and, if standard output or error is not
/// redirected, point it at that console. Redirected output (`> file`, pipes) is left alone.
pub fn attach_console() {
    // SAFETY: AttachConsole has no pointer arguments. It fails when the parent
    // has no console; then there is nothing to print to.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    redirect_to_console(STD_OUTPUT_HANDLE);
    redirect_to_console(STD_ERROR_HANDLE);
}

fn redirect_to_console(which: STD_HANDLE) {
    // SAFETY: GetStdHandle only reads the process's standard handle table.
    let current = unsafe { GetStdHandle(which) };
    if !(current.is_null() || current == INVALID_HANDLE_VALUE) {
        return;
    }
    if let Ok(console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        // The handle is leaked on purpose: it is the standard handle until the process exits.
        let raw = console.into_raw_handle();
        // SAFETY: `raw` is a valid, open handle that nothing else owns.
        unsafe { SetStdHandle(which, raw) };
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "bingwall";

/// Start `command` when the user logs in: sets `HKCU\...\Run\bingwall`.
pub fn autostart_set(command: &OsStr) -> Result<(), String> {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let data = wide(command);
    let mut hkey: HKEY = std::ptr::null_mut();
    // SAFETY: all strings are NUL-terminated and outlive the calls; `hkey` is a
    // valid out-pointer; optional pointers may be null.
    unsafe {
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        );
        if rc != ERROR_SUCCESS {
            return Err(format!("could not open the Run key (Windows error {rc})"));
        }
        // `cbData` counts bytes, including the terminating NUL.
        let rc = RegSetValueExW(hkey, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32);
        RegCloseKey(hkey);
        if rc != ERROR_SUCCESS {
            return Err(format!("could not write the Run value (Windows error {rc})"));
        }
    }
    Ok(())
}

/// Remove the Run value. Returns `false` if it was not there.
pub fn autostart_remove() -> Result<bool, String> {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    match rc {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
        _ => Err(format!("could not remove the Run value (Windows error {rc})")),
    }
}

/// The command line stored by `autostart on`: the quoted exe path, then the market if it is not the default.
pub fn autostart_command(exe: &Path, market: &str, default_market: &str) -> OsString {
    let mut cmd = OsString::from("\"");
    cmd.push(exe);
    cmd.push("\"");
    if market != default_market {
        cmd.push(" --market ");
        cmd.push(market);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_nul_terminated_and_keeps_non_ascii() {
        let w = wide("C:\\Users\\Jos\u{e9} \u{65e5}\u{672c}\\a.jpg");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(String::from_utf16(&w[..w.len() - 1]).unwrap(), "C:\\Users\\Jos\u{e9} \u{65e5}\u{672c}\\a.jpg");
        assert_eq!(wide(""), vec![0]);
    }

    #[test]
    fn autostart_command_quotes_the_path() {
        let exe = Path::new("C:\\Program Files\\bw\\bingwall.exe");
        assert_eq!(autostart_command(exe, "en-US", "en-US"), OsString::from("\"C:\\Program Files\\bw\\bingwall.exe\""));
        assert_eq!(
            autostart_command(exe, "de-DE", "en-US"),
            OsString::from("\"C:\\Program Files\\bw\\bingwall.exe\" --market de-DE")
        );
    }
}
