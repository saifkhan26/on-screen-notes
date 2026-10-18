//! Win32-specific tweaks for the overlay window.
//!
//! ## Why this module exists
//!
//! Window queries eframe / winit do not expose, plus:
//!
//! * Foreground reclaim — after a click-through, the app *behind* OSN
//!   activates (that is OS behaviour we cannot suppress without an
//!   intrusive low-level mouse hook). We call `SetForegroundWindow`
//!   on the next pen event so OSN's keyboard shortcuts (P, B, T…)
//!   keep working. Win10 normally restricts `SetForegroundWindow`
//!   to processes that already own the foreground, so we use the
//!   well-known `AttachThreadInput` hack to bypass it.
//!
//! The window is an ordinary activating window. It used to carry
//! `WS_EX_NOACTIVATE`, which made it open unfocused and ignore
//! activation from clicks on its toolbar and popups.
//!
//! The helpers are best-effort: any error from the Win32 API is
//! logged and swallowed so a temporary failure does not crash the
//! drawing app.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    SetForegroundWindow, SetWindowDisplayAffinity, WindowFromPoint, GA_ROOT, WDA_EXCLUDEFROMCAPTURE,
    WDA_NONE,
};

/// Force `hwnd_raw` (an HWND cast to `isize`, as raw-window-handle
/// reports it) to the foreground using the `AttachThreadInput`
/// bypass. Windows allows `SetForegroundWindow` to succeed when the
/// caller's input is attached to the current-foreground thread's
/// input queue, so we attach for the duration of the call and detach
/// immediately after.
///
/// Cheap to call (a handful of small syscalls) but skip when OSN is
/// already in the foreground.
pub fn reclaim_foreground(hwnd_raw: isize) {
    if hwnd_raw == 0 { return; }
    // SAFETY: the integer is the HWND raw-window-handle gave us for our
    // own window. Each call below takes it by value and never
    // dereferences it on the Rust side; the kernel handles the object.
    unsafe {
        let hwnd = HWND(hwnd_raw);
        let fg = GetForegroundWindow();
        if fg.0 == hwnd_raw {
            return; // already focused
        }

        // Bypass the Win10 foreground restriction:
        //   1. Find the thread that owns the current foreground window.
        //   2. AttachThreadInput so our input queue is married to theirs.
        //   3. SetForegroundWindow is now permitted.
        //   4. Detach so we don't leak the attachment.
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let our_thread = GetWindowThreadProcessId(hwnd, None);
        if fg_thread == 0 || our_thread == 0 || fg_thread == our_thread {
            // Nothing to attach to — just attempt the call.
            let _ = SetForegroundWindow(hwnd);
            return;
        }
        let attached = AttachThreadInput(our_thread, fg_thread, true).as_bool();
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(our_thread, fg_thread, false);
        }
    }
}

/// True while the OS-level left mouse button is held down.
///
/// `GetAsyncKeyState`'s high bit is set while the key is pressed.
/// Used by the recording-cursor compositing path so the captured
/// dot turns red on click.
pub fn lmb_down() -> bool {
    // SAFETY: `GetAsyncKeyState` takes a virtual-key code and reads
    // a per-thread keyboard state. No pointers involved.
    unsafe {
        (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0
    }
}

/// True while Ctrl and V are both physically held.
///
/// egui-winit turns Ctrl+V into a `Paste(text)` event and swallows the
/// key press itself — and emits nothing at all when the clipboard holds
/// no text. A copied screenshot is exactly that case, so pasting an
/// image has to notice the chord some other way.
pub fn paste_chord_down() -> bool {
    const VK_V: i32 = 0x56;
    // SAFETY: same as `lmb_down` — plain key-state reads.
    unsafe {
        (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
            && (GetAsyncKeyState(VK_V) as u16 & 0x8000) != 0
    }
}

/// Current cursor position in physical screen pixels, or `None` if
/// the call failed.
pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: `GetCursorPos` writes into the `POINT` we pass; we
    // hand it a stack-allocated value with no extra reference.
    let ok = unsafe { GetCursorPos(&mut p).is_ok() };
    if ok { Some((p.x, p.y)) } else { None }
}

/// Top-level window under `(x, y)` in screen-physical pixels. We
/// chase up to the root via `GetAncestor(GA_ROOT)` so click on a
/// child control (a button inside a dialog, say) still grabs the
/// outermost frame the user thinks of as "that window". Returns the
/// HWND as `isize` so callers stay platform-portable; 0 = no match.
pub fn top_level_window_at(x: i32, y: i32) -> isize {
    // SAFETY: both calls take simple value-type arguments and
    // return HWND pointers we treat as opaque integers.
    unsafe {
        let pt = POINT { x, y };
        let hit = WindowFromPoint(pt);
        if hit.0 == 0 {
            return 0;
        }
        let root = GetAncestor(hit, GA_ROOT);
        if root.0 == 0 { hit.0 } else { root.0 }
    }
}

/// Toggle whether `hwnd_raw` is hidden from desktop-capture APIs
/// (DXGI Desktop Duplication, BitBlt, PrintScreen, OBS, the
/// magnifier loupe's own xcap calls — anything Windows treats as
/// a capture surface). Used by the live loupe so it does not
/// recursively capture its own painted output and feed back into
/// an infinite zoom-into-self mirror.
///
/// `WDA_EXCLUDEFROMCAPTURE` needs Windows 10 build 19041+. Older
/// builds fall back to `WDA_MONITOR` semantics (black-box the
/// window in captures), which is still better than the feedback
/// loop. Calls quietly when affinity setting is unsupported.
pub fn set_window_capture_excluded(hwnd_raw: isize, excluded: bool) {
    if hwnd_raw == 0 { return; }
    // SAFETY: HWND is the kernel handle for our own window; the
    // API just sets a flag on it.
    unsafe {
        let hwnd = HWND(hwnd_raw);
        let affinity = if excluded { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        let _ = SetWindowDisplayAffinity(hwnd, affinity);
    }
}

/// Outer rect of `hwnd_raw` in screen-physical pixels as
/// `(left, top, right, bottom)`. Returns `None` if the call fails
/// or the HWND is 0.
pub fn window_rect(hwnd_raw: isize) -> Option<(i32, i32, i32, i32)> {
    if hwnd_raw == 0 { return None; }
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: `GetWindowRect` writes into the `RECT` we pass and
    // does not store the pointer.
    let ok = unsafe { GetWindowRect(HWND(hwnd_raw), &mut r).is_ok() };
    if ok { Some((r.left, r.top, r.right, r.bottom)) } else { None }
}

/// Client-area rect of `hwnd_raw` in screen-physical pixels as
/// `(left, top, right, bottom)`.
///
/// Distinct from `window_rect` (the OUTER rect, which includes any
/// DWM-managed shadow / resize border even on a borderless window).
/// For the freeze-frame-to-layer feature we want the *content* area
/// the user actually sees and draws into — using the outer rect
/// captured a strip of pixels above-and-left of the visible content,
/// which then rendered shifted up-left from the user's strokes.
///
/// Uses `GetClientRect` (returns client width/height with top-left at
/// (0,0)) plus `ClientToScreen((0,0))` to translate the origin into
/// screen coordinates.
pub fn client_rect(hwnd_raw: isize) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    if hwnd_raw == 0 { return None; }
    let hwnd = HWND(hwnd_raw);
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: both calls write to the structures we pass and do not
    // retain the pointers.
    let ok = unsafe { GetClientRect(hwnd, &mut r).is_ok() };
    if !ok { return None; }
    let mut origin = POINT { x: 0, y: 0 };
    let ok = unsafe { ClientToScreen(hwnd, &mut origin).as_bool() };
    if !ok { return None; }
    Some((origin.x, origin.y, origin.x + r.right, origin.y + r.bottom))
}

/// Refresh rate in Hz of the monitor showing `hwnd_raw`, or `None` when
/// Windows does not say (it reports 0 or 1 for "hardware default").
pub fn refresh_rate_hz(hwnd_raw: isize) -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS,
        MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    };
    // SAFETY: both calls only write into the structs we pass, each with
    // its size filled in, and keep no pointer to them.
    unsafe {
        let monitor = MonitorFromWindow(HWND(hwnd_raw), MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut info.monitorInfo as *mut MONITORINFO).as_bool() {
            return None;
        }
        let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool() {
            return None;
        }
        (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency)
    }
}
