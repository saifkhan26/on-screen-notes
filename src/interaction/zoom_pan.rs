//! Scroll-wheel handling.
//!
//! Four modes, mutually exclusive per event:
//!   * `Ctrl + wheel`     → adjust overlay background opacity.
//!   * `Shift + wheel`    → adjust active-layer opacity.
//!   * Pen-style tool     → adjust pen size.
//!   * Any other tool     → zoom the canvas around the cursor.
//!
//! `Shift + Space + wheel` (zoom the active *layer*) never reaches this
//! function — the app intercepts it while Space is held, because the
//! gesture accumulates in app-side state until the user lets go of
//! Space rather than mutating the canvas per notch.
//!
//! All branches are driven by a **notch count** (signed integer: +1 per
//! wheel-up notch, -1 per wheel-down). The caller is responsible for
//! collapsing the platform-specific raw delta into this notch count so
//! every wheel click produces exactly one gradual step here.
//!
//! Zoom-around-cursor maths (for the curious):
//!   `pan' = pan + (cursor - pan) * (1 - z' / z)`
//! where z is old zoom, z' is new zoom. This keeps the cursor's
//! canvas-space position fixed across the zoom change so the world
//! appears to scale "around" the cursor.

use crate::canvas::canvas::Canvas;
use crate::tools::{ActiveTool, ToolKind};

/// Apply `notches` wheel steps (signed integer — positive = wheel up).
/// Step sizes are tuned for gradual, fine-grained control: one notch
/// nudges the value just enough to feel responsive without overshooting.
pub fn handle_scroll(
    notches: i32,
    raw_delta_y: f32,
    cursor_screen: [f32; 2],
    ctrl_held: bool,
    shift_held: bool,
    tool: &mut ActiveTool,
    canvas: &mut Canvas,
    bg_opacity: &mut f32,
) {
    // Stepped controls (opacity / pen size / spotlight size) read
    // notches so each wheel click is a discrete bump. Canvas zoom
    // reads the raw delta sum so smooth-scroll wheels and trackpad
    // pinches produce continuous, lag-free zoom updates each
    // frame.
    let n_f = notches as f32;

    // Ctrl + wheel: adjust background opacity. ~1 % per notch → ~50
    // clicks to span the full transparent→opaque range. Plenty
    // gradual.
    if ctrl_held {
        if notches == 0 { return; }
        const STEP: f32 = 0.01;
        *bg_opacity = (*bg_opacity + n_f * STEP).clamp(0.0, 1.0);
        return;
    }

    // Shift + wheel: adjust the *active* layer's opacity. Same 1 % /
    // notch granularity as the background-opacity branch.
    if shift_held {
        if notches == 0 { return; }
        if let Some(layer) = canvas.layers.get_mut(canvas.active_layer) {
            const STEP: f32 = 0.01;
            layer.opacity = (layer.opacity + n_f * STEP).clamp(0.0, 1.0);
        }
        return;
    }

    if matches!(tool.kind, ToolKind::Spotlight) {
        if notches == 0 { return; }
        const STEP: f32 = 5.0;
        tool.spotlight_size = (tool.spotlight_size + n_f * STEP).clamp(20.0, 800.0);
    } else if matches!(tool.kind, ToolKind::Pen | ToolKind::Eraser | ToolKind::FreehandArrow | ToolKind::Fill) {
        if notches == 0 { return; }
        const SIZE_MIN: f32 = 1.0;
        const SIZE_MAX: f32 = 80.0;
        tool.size = (tool.size + n_f).clamp(SIZE_MIN, SIZE_MAX);
    } else {
        // Live zoom around the cursor. Uses the raw wheel delta
        // sum so smooth-scroll wheels and trackpad pinches drive
        // continuous, per-frame zoom — there is no "land on the
        // next notch" pause between updates.
        if raw_delta_y.abs() < 0.001 { return; }
        // 1.10× per raw unit gives a snappy but smooth feel: a
        // single notch (delta ≈ ±1) ≈ ±10 % zoom; sub-notch
        // events from trackpads / hi-res wheels apply
        // proportionally.
        let factor: f32 = 1.10_f32.powf(raw_delta_y);
        let z_old = canvas.zoom;
        let z_new = (z_old * factor).clamp(0.1, 16.0);
        if (z_new - z_old).abs() < 1e-4 { return; }
        let cx = (cursor_screen[0] - canvas.pan[0]) / z_old;
        let cy = (cursor_screen[1] - canvas.pan[1]) / z_old;
        canvas.zoom = z_new;
        canvas.pan[0] = cursor_screen[0] - cx * z_new;
        canvas.pan[1] = cursor_screen[1] - cy * z_new;
    }
}
