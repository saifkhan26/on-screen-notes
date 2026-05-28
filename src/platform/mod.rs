//! OS-specific glue that does not fit into a single subsystem.
//!
//! Currently holds the Win32 click-through helpers; macOS / Linux stubs
//! re-export no-op versions so the call sites can stay portable.

#[cfg(target_os = "windows")]
pub mod win32;

#[cfg(target_os = "windows")]
pub use win32::{cursor_pos, lmb_down, reclaim_foreground, set_no_activate, set_window_capture_excluded, top_level_window_at, window_rect};

#[cfg(not(target_os = "windows"))]
pub fn top_level_window_at(_x: i32, _y: i32) -> isize { 0 }

#[cfg(not(target_os = "windows"))]
pub fn set_window_capture_excluded(_hwnd: isize, _excluded: bool) {}

// ---- No-op stand-ins for non-Windows targets -----------------------------
// `app.rs` calls these unconditionally; they degrade to predictable
// "feature disabled" return values when we are not on Windows.

#[cfg(not(target_os = "windows"))]
pub fn set_no_activate(_hwnd: isize) {}

#[cfg(not(target_os = "windows"))]
pub fn reclaim_foreground(_hwnd: isize) {}

#[cfg(not(target_os = "windows"))]
pub fn window_rect(_hwnd: isize) -> Option<(i32, i32, i32, i32)> { None }

#[cfg(not(target_os = "windows"))]
pub fn cursor_pos() -> Option<(i32, i32)> { None }

#[cfg(not(target_os = "windows"))]
pub fn lmb_down() -> bool { false }
