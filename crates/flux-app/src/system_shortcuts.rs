//! macOS shortcuts the window server keeps for itself: their keys never reach any app.
//!
//! ⌘Esc (Claude) is the system's "symbolic hot key" No. 73 — Front Row's, an app macOS dropped in
//! 10.7, yet the shortcut is still on in many Macs' keyboard settings (no setting shows it), and with
//! it on the window server swallows ⌘Esc. While Flux runs, that dead shortcut is turned off — again
//! each time a Flux window comes forward, as another Flux that quit turns it back on — and Flux
//! turns it on again when it quits. A shortcut moved to another key is left alone.

use gpui::App;

/// Front Row's shortcut.
#[cfg(target_os = "macos")]
const FRONT_ROW: std::ffi::c_int = 73;
/// ⌘Esc as the window server stores it: Escape's virtual key code and the Command flag.
#[cfg(target_os = "macos")]
const ESCAPE_KEY_CODE: u16 = 53;
#[cfg(target_os = "macos")]
const COMMAND_FLAG: u64 = 1 << 20;

/// Flux turned the shortcut off: it turns it on when it quits.
#[cfg(target_os = "macos")]
static TURNED_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Frees ⌘Esc now and gives it back when Flux quits.
pub fn init(cx: &mut App) {
    claim();
    cx.on_app_quit(|_| {
        release();
        async {}
    })
    .detach();
}

/// Turns Front Row's shortcut off while it holds ⌘Esc; a window coming forward calls it again.
/// Never in a scenario or a test: the shortcut is the user's own system's.
#[cfg(target_os = "macos")]
pub fn claim() {
    use std::sync::atomic::Ordering;
    if cfg!(test) || std::env::var_os("FLUX_SCENARIO").is_some() {
        return;
    }
    let Some(server) = window_server() else {
        return;
    };
    // SAFETY: the window server's functions as SkyLight exports them, with valid out-pointers.
    unsafe {
        if !(server.is_enabled)(FRONT_ROW) {
            return;
        }
        let (mut character, mut key_code, mut modifiers) = (0u16, 0u16, 0u64);
        let read = (server.value)(FRONT_ROW, &mut character, &mut key_code, &mut modifiers);
        if read != 0 || key_code != ESCAPE_KEY_CODE || modifiers != COMMAND_FLAG {
            return;
        }
        if (server.set_enabled)(FRONT_ROW, false) == 0 {
            TURNED_OFF.store(true, Ordering::Relaxed);
            log::info!("macOS shortcut {FRONT_ROW} (Front Row, ⌘Esc) turned off while Flux runs");
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn claim() {}

/// Turns back on what [`claim`] turned off.
#[cfg(target_os = "macos")]
fn release() {
    if !TURNED_OFF.swap(false, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(server) = window_server() {
        // SAFETY: as in `claim`.
        unsafe { (server.set_enabled)(FRONT_ROW, true) };
    }
}

#[cfg(not(target_os = "macos"))]
fn release() {}

/// The window server's functions for its shortcuts (SkyLight, a private framework: looked up at
/// run time, absent ones leave the shortcut as it is).
#[cfg(target_os = "macos")]
struct WindowServer {
    is_enabled: unsafe extern "C" fn(std::ffi::c_int) -> bool,
    set_enabled: unsafe extern "C" fn(std::ffi::c_int, bool) -> std::ffi::c_int,
    value: unsafe extern "C" fn(std::ffi::c_int, *mut u16, *mut u16, *mut u64) -> std::ffi::c_int,
}

#[cfg(target_os = "macos")]
fn window_server() -> Option<&'static WindowServer> {
    static SERVER: std::sync::OnceLock<Option<WindowServer>> = std::sync::OnceLock::new();
    SERVER.get_or_init(load_window_server).as_ref()
}

#[cfg(target_os = "macos")]
fn load_window_server() -> Option<WindowServer> {
    use std::ffi::{CStr, c_void};
    type IsEnabled = unsafe extern "C" fn(std::ffi::c_int) -> bool;
    type SetEnabled = unsafe extern "C" fn(std::ffi::c_int, bool) -> std::ffi::c_int;
    type Value =
        unsafe extern "C" fn(std::ffi::c_int, *mut u16, *mut u16, *mut u64) -> std::ffi::c_int;
    let path = c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";
    // SAFETY: a system framework, loaded once and never unloaded.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
    if handle.is_null() {
        return None;
    }
    let symbol = |name: &CStr| -> Option<*mut c_void> {
        // SAFETY: `handle` is a loaded library, `name` a C string.
        let pointer = unsafe { libc::dlsym(handle, name.as_ptr()) };
        (!pointer.is_null()).then_some(pointer)
    };
    let (is_enabled, set_enabled, value) = (
        symbol(c"CGSIsSymbolicHotKeyEnabled")?,
        symbol(c"CGSSetSymbolicHotKeyEnabled")?,
        symbol(c"CGSGetSymbolicHotKeyValue")?,
    );
    // SAFETY: the symbols are these functions, with these signatures.
    unsafe {
        Some(WindowServer {
            is_enabled: std::mem::transmute::<*mut c_void, IsEnabled>(is_enabled),
            set_enabled: std::mem::transmute::<*mut c_void, SetEnabled>(set_enabled),
            value: std::mem::transmute::<*mut c_void, Value>(value),
        })
    }
}
