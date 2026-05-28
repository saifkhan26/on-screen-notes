//! "Click-through": when the user holds Alt, mouse clicks pass *through* our
//! transparent window to whatever is behind it.
//!
//! egui exposes this as `ViewportCommand::MousePassthrough(bool)`, which
//! eframe forwards to winit, which on each platform translates to:
//!   * Windows: toggling the `WS_EX_TRANSPARENT` extended window style.
//!   * macOS  : `setIgnoresMouseEvents:`.
//!   * Wayland: setting an empty input region (compositor support varies).
//!   * X11    : XShape input mask.
//!
//! Important caveats:
//!   * Some Linux Wayland compositors don't honour empty input regions; if
//!     the user's compositor lacks support, click-through silently no-ops.
//!     We log a one-time hint at startup.
//!   * On macOS the next mouse-up after un-passthrough may be delayed by
//!     one frame. Not a real bug, just a quirk.

use crate::input::modifiers::ModifierTracker;
use egui::{Context, ViewportCommand};

/// Reads the current Alt state from `ctx`, and on a state change sends the
/// matching `MousePassthrough` command to the host.
pub fn update(ctx: &Context, tracker: &mut ModifierTracker) -> bool {
    // egui's modifier state for the *current* frame.
    let alt_now = ctx.input(|i| i.modifiers.alt);
    if let Some(new_state) = tracker.alt_edge(alt_now) {
        // `true` means clicks should pass through (i.e. ignore them in our window).
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(new_state));
    }
    tracker.alt_held
}
