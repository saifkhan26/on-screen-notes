//! Win32-specific tweaks for the overlay window.
//!
//! ## Why this module exists
//!
//! eframe / winit hand us a window with a basic extended style set. For
//! the "click-through but stay focused" UX we want, we need two extra
//! extended-style bits that eframe does not expose:
//!
//! * `WS_EX_NOACTIVATE` — when set, clicks **on** this window do not
//!   activate it. Combined with the existing `WS_EX_TRANSPARENT`
//!   toggle (driven by `ViewportCommand::MousePassthrough`), this
//!   keeps the OSN window from grabbing focus when the user lets go
//!   of Alt and clicks back into it. It also helps the floating
//!   activator button stay out of the way of normal alt-tabbing.
//!
//! * Foreground reclaim — after a click-through, the app *behind* OSN
//!   activates (that is OS behaviour we cannot suppress without an
//!   intrusive low-level mouse hook). We call `SetForegroundWindow`
//!   on the next pen event so OSN's keyboard shortcuts (P, B, T…)
//!   keep working. Win10 normally restricts `SetForegroundWindow`
//!   to processes that already own the foreground, so we use the
//!   well-known `AttachThreadInput` hack to bypass it.
//!
//! Both helpers are best-effort: any error from the Win32 API is
//! logged and swallowed so a temporary failure does not crash the
//! drawing app.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, SetForegroundWindow, SetWindowDisplayAffinity, SetWindowLongPtrW,
    WindowFromPoint, GA_ROOT, GWL_EXSTYLE, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WS_EX_NOACTIVATE,
};

/// Set `WS_EX_NOACTIVATE` on the window identified by `hwnd_raw` (an
/// HWND cast to `isize`, which is how raw-window-handle reports it).
///
/// Idempotent: if the bit is already set we leave it.
pub fn set_no_activate(hwnd_raw: isize) {
    if hwnd_raw == 0 {
        log::warn!("set_no_activate called with null HWND");
        return;
    }
    // SAFETY: we cast the integer back to an HWND we received from
    // raw-window-handle, which is the OS's authoritative pointer for
    // our own window. The two Win32 calls below only read/write
    // window state, never deref the pointer.
    unsafe {
        let hwnd = HWND(hwnd_raw);
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = cur | (WS_EX_NOACTIVATE.0 as isize);
        if cur == want { return; }
        let res = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        if res == 0 {
            // SetWindowLongPtrW returns 0 on success when the prior
            // value was 0; check GetLastError to distinguish.
            // Practically we just log — a failure here is non-fatal.
            log::debug!("SetWindowLongPtrW(GWL_EXSTYLE | WS_EX_NOACTIVATE) returned 0");
        }
    }
}

/// Force `hwnd_raw` to the foreground using the `AttachThreadInput`
/// bypass. Windows allows `SetForegroundWindow` to succeed when the
/// caller's input is attached to the current-foreground thread's
/// input queue, so we attach for the duration of the call and detach
/// immediately after.
///
/// Cheap to call (a handful of small syscalls) but skip when OSN is
/// already in the foreground.
pub fn reclaim_foreground(hwnd_raw: isize) {
    if hwnd_raw == 0 { return; }
    // SAFETY: same justification as `set_no_activate`. Each call below
    // takes an HWND pointer and never dereferences it on the Rust
    // side; the kernel handles the underlying object.
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
