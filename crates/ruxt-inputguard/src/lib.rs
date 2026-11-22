//! Win32 calls that the Ruxt binary cannot make itself (`#![forbid(unsafe_code)]`).
//! The API here is safe. On other platforms the functions no-op.
//!
//! * Agent guard: drops keyboard / mouse input that a program injected (`SendInput`,
//!   `keybd_event`, `mouse_event`) while it is aimed at a window of this process.
//! * Caption: paints the Windows title bar so the minimize / maximize / close glyphs show.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Set by the hooks when they dropped injected input; read with [`take_tripped`].
static TRIPPED: AtomicBool = AtomicBool::new(false);
/// Called (from the hook thread) after input was dropped, e.g. to wake the UI.
static ON_TRIP: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

#[cfg_attr(not(windows), allow(dead_code))]
fn trip() {
    TRIPPED.store(true, Ordering::SeqCst);
    if let Some(f) = ON_TRIP.get() {
        f();
    }
}

/// True once after the hooks dropped injected input.
pub fn take_tripped() -> bool {
    TRIPPED.swap(false, Ordering::SeqCst)
}

/// Register the callback run after input was dropped. Only the first registration counts.
pub fn set_on_trip(f: impl Fn() + Send + Sync + 'static) {
    let _ = ON_TRIP.set(Box::new(f));
}

/// Paint an opaque Windows title bar and visible minimize / maximize / close glyphs.
///
/// `hwnd` is a Win32 `HWND` (`0` is ignored). `dark` asks for light glyphs.
/// Colours are sRGB `[r, g, b]`. No-op off Windows.
pub fn set_caption_colors(hwnd: isize, dark: bool, caption: [u8; 3], text: [u8; 3], border: [u8; 3]) {
    #[cfg(windows)]
    win::set_caption_colors(hwnd, dark, caption, text, border);
    #[cfg(not(windows))]
    {
        let _ = (hwnd, dark, caption, text, border);
    }
}

/// Installed low-level hooks; dropping the value removes them.
pub struct Hooks {
    // Only held for its Drop (which removes the hooks).
    #[cfg(windows)]
    _inner: win::HookThread,
}

impl Hooks {
    /// Install the hooks. `None` when the platform has none or installing failed.
    pub fn start() -> Option<Self> {
        #[cfg(windows)]
        {
            win::HookThread::start().map(|inner| Self { _inner: inner })
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

#[cfg(windows)]
mod win {
    use super::trip;
    use std::ptr::null_mut;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::Controls::SetWindowTheme;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn is_ours(hwnd: HWND) -> bool {
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        // SAFETY: plain Win32 queries; a stale handle just yields pid 0.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid == unsafe { GetCurrentProcessId() }
    }

    unsafe extern "system" fn keyboard_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lParam points to a KBDLLHOOKSTRUCT.
            let k = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
            if k.flags & LLKHF_INJECTED != 0 && is_ours(unsafe { GetForegroundWindow() }) {
                trip();
                return 1;
            }
        }
        unsafe { CallNextHookEx(null_mut(), code, w, l) }
    }

    unsafe extern "system" fn mouse_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for WH_MOUSE_LL with HC_ACTION, lParam points to an MSLLHOOKSTRUCT.
            let m = unsafe { &*(l as *const MSLLHOOKSTRUCT) };
            // Plain moves pass (cursor jigglers inject those everywhere); clicks and wheel
            // aimed at one of our windows are dropped.
            if m.flags & LLMHF_INJECTED != 0 && w as u32 != WM_MOUSEMOVE && is_ours(unsafe { WindowFromPoint(m.pt) }) {
                trip();
                return 1;
            }
        }
        unsafe { CallNextHookEx(null_mut(), code, w, l) }
    }

    /// A thread that owns the low-level hooks and pumps the message loop they need.
    pub struct HookThread {
        thread_id: u32,
        join: Option<JoinHandle<()>>,
    }

    impl HookThread {
        pub fn start() -> Option<Self> {
            let (tx, rx) = mpsc::channel();
            let join = std::thread::Builder::new()
                .name("agent-guard".into())
                .spawn(move || unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    // Create the thread's message queue before reporting its id, so the
                    // WM_QUIT posted by Drop cannot be lost.
                    PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
                    let module = GetModuleHandleW(std::ptr::null());
                    let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
                    let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
                    let ok = !kb.is_null() && !ms.is_null();
                    let _ = tx.send(ok.then(|| GetCurrentThreadId()));
                    if ok {
                        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
                    }
                    if !kb.is_null() {
                        UnhookWindowsHookEx(kb);
                    }
                    if !ms.is_null() {
                        UnhookWindowsHookEx(ms);
                    }
                })
                .ok()?;
            match rx.recv() {
                Ok(Some(thread_id)) => Some(Self {
                    thread_id,
                    join: Some(join),
                }),
                _ => {
                    let _ = join.join();
                    None
                }
            }
        }
    }

    /// `DarkMode_Explorer` on the top-level window (what winit applies for a dark theme) leaves
    /// the caption buttons with nothing painted on Windows 11. Clear that theme, then ask DWM
    /// for immersive dark or light glyphs and an opaque caption.
    pub(super) fn set_caption_colors(hwnd: isize, dark: bool, caption: [u8; 3], text: [u8; 3], border: [u8; 3]) {
        let hwnd = hwnd as HWND;
        if hwnd.is_null() {
            return;
        }
        let dark_mode: i32 = i32::from(dark);
        let caption = colorref(caption);
        let text = colorref(text);
        let border = colorref(border);
        // SAFETY: `hwnd` is a window of this process (the caller passes the eframe HWND).
        // A stale handle makes the calls fail and we ignore the HRESULT.
        unsafe {
            let _ = SetWindowTheme(hwnd, std::ptr::null(), std::ptr::null());
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&raw const dark_mode).cast(),
                size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR as u32,
                (&raw const caption).cast(),
                size_of::<u32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TEXT_COLOR as u32,
                (&raw const text).cast(),
                size_of::<u32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&raw const border).cast(),
                size_of::<u32>() as u32,
            );
            let _ = SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    fn colorref(rgb: [u8; 3]) -> u32 {
        u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16)
    }

    impl Drop for HookThread {
        fn drop(&mut self) {
            // SAFETY: posting to a thread id we own; harmless if it already exited.
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn caption_ignores_null_hwnd() {
        super::set_caption_colors(0, true, [0x20, 0x2a, 0x1e], [0xd9, 0xe3, 0xcf], [0x3a, 0x4a, 0x36]);
    }

    #[cfg(windows)]
    #[test]
    fn hooks_install_and_remove() {
        let h = super::Hooks::start().expect("low-level hooks install");
        drop(h); // must not hang
        assert!(!super::take_tripped());
    }
}
