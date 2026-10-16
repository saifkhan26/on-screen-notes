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

use crate::canvas::CanvasManager;
use crate::tools::{ActiveTool, ToolKind};

/// Canvas zoom range: 0.1 % to 100 000 %, wide enough not to be in the
/// way (a big PureRef board fits at a fraction of a percent). Zoom is
/// still bounded: at 0 the maths divides by zero, and far past these
/// the f32 canvas coordinates run out of precision and ink jitters.
/// Other code already treats 0.001 as the smallest zoom.
pub const MIN_ZOOM: f32 = 0.001;
pub const MAX_ZOOM: f32 = 1000.0;

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
    canvases: &mut CanvasManager,
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
        // A real edit, so this one goes through `active_mut` and is saved.
        let canvas = canvases.active_mut();
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
        // View only: the slide is not flagged for the autosave.
        let (pan, zoom) = canvases.active_view_mut();
        zoom_about_cursor(pan, zoom, raw_delta_y, cursor_screen);
    }
}

/// Live zoom around the cursor. Uses the raw wheel delta sum so
/// smooth-scroll wheels and trackpad pinches drive continuous,
/// per-frame zoom — there is no "land on the next notch" pause between
/// updates.
fn zoom_about_cursor(pan: &mut [f32; 2], zoom: &mut f32, raw_delta_y: f32, cursor_screen: [f32; 2]) {
    if raw_delta_y.abs() < 0.001 { return; }
    // 1.10× per raw unit gives a snappy but smooth feel: a single notch
    // (delta ≈ ±1) ≈ ±10 % zoom; sub-notch events from trackpads /
    // hi-res wheels apply proportionally.
    let factor: f32 = 1.10_f32.powf(raw_delta_y);
    let z_old = *zoom;
    let z_new = (z_old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    // Relative, so a step still registers at tiny zoom levels.
    if (z_new / z_old - 1.0).abs() < 1e-4 { return; }
    let cx = (cursor_screen[0] - pan[0]) / z_old;
    let cy = (cursor_screen[1] - pan[1]) / z_old;
    *zoom = z_new;
    pan[0] = cursor_screen[0] - cx * z_new;
    pan[1] = cursor_screen[1] - cy * z_new;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::canvas::Canvas;

    fn wheel(steps: i32, delta: f32, canvas: &mut Canvas) {
        for _ in 0..steps {
            zoom_about_cursor(&mut canvas.pan, &mut canvas.zoom, delta, [100.0, 100.0]);
        }
    }

    #[test]
    fn wheel_zoom_is_not_an_edit_but_layer_opacity_is() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = ToolKind::Select;
        let mut canvas = Canvas::default();
        canvas.dirty = false;
        let mut m = CanvasManager::for_tests(vec![canvas]);
        let mut bg = 0.5;
        handle_scroll(1, 1.0, [100.0, 100.0], false, false, &mut tool, &mut m, &mut bg);
        assert!(m.active().zoom > 1.0);
        assert!(!m.active().dirty, "zooming must not trigger a slide save");
        handle_scroll(1, 1.0, [100.0, 100.0], false, true, &mut tool, &mut m, &mut bg);
        assert!(m.active().dirty, "a layer-opacity change must be saved");
    }

    #[test]
    fn zoom_reaches_far_past_the_old_limits_and_stops_at_the_new_ones() {
        let mut canvas = Canvas::default();
        wheel(60, 1.0, &mut canvas);
        assert!(canvas.zoom > 100.0, "well past the old 16x: {}", canvas.zoom);
        wheel(200, 1.0, &mut canvas);
        assert_eq!(canvas.zoom, MAX_ZOOM);
        // Out again, through the tiny levels where an absolute "too
        // small a step" test used to stall.
        wheel(400, -1.0, &mut canvas);
        assert_eq!(canvas.zoom, MIN_ZOOM);
        // The point under the cursor stays put.
        let under = [(100.0 - canvas.pan[0]) / canvas.zoom, (100.0 - canvas.pan[1]) / canvas.zoom];
        wheel(5, 1.0, &mut canvas);
        let after = [(100.0 - canvas.pan[0]) / canvas.zoom, (100.0 - canvas.pan[1]) / canvas.zoom];
        assert!((under[0] - after[0]).abs() < 1e-2 * under[0].abs().max(1.0), "{under:?} vs {after:?}");
    }
}
