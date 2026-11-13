//! Scroll-wheel handling.
//!
//! Four modes, mutually exclusive per event:
//!   * `Ctrl + wheel`     → adjust overlay background opacity.
//!   * `Shift + wheel`    → adjust active-layer opacity.
//!   * Pen-style tool     → adjust pen size.
//!   * Any other tool     → zoom the canvas around the cursor.
//!
//! Holding Space (the pan key) makes the wheel zoom whatever the tool,
//! so the pen and brushes can pan and zoom without switching tools.
//!
//! A locked slide, or one still opening, takes no edits: there Shift +
//! wheel does nothing and a pen-style tool zooms like any other.
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
    space_held: bool,
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

    let view_only = !canvases.active_editable();

    // Shift + wheel: adjust the *active* layer's opacity. Same 1 % /
    // notch granularity as the background-opacity branch.
    if shift_held {
        if view_only { return; }
        if notches == 0 { return; }
        // A real edit, so this one goes through `active_mut` and is saved.
        let canvas = canvases.active_mut();
        if let Some(layer) = canvas.layers.get_mut(canvas.active_layer) {
            const STEP: f32 = 0.01;
            layer.opacity = (layer.opacity + n_f * STEP).clamp(0.0, 1.0);
        }
        return;
    }

    if space_held {
        // Space pans, so with it held the wheel is view navigation too.
        let (pan, zoom) = canvases.active_view_mut();
        zoom_about_cursor(pan, zoom, raw_delta_y, cursor_screen);
    } else if matches!(tool.kind, ToolKind::Spotlight)
        || !view_only && matches!(tool.kind, ToolKind::Pen | ToolKind::Eraser | ToolKind::FreehandArrow | ToolKind::Fill)
    {
        if notches == 0 { return; }
        nudge_size(tool, n_f);
    } else {
        // View only: the slide is not flagged for the autosave.
        let (pan, zoom) = canvases.active_view_mut();
        zoom_about_cursor(pan, zoom, raw_delta_y, cursor_screen);
    }
}

/// Size up or down by `notches` wheel notches: the spotlight's square
/// with the Spotlight tool, else the pen's width (what the size slider
/// sets, and so a selection's).
pub fn nudge_size(tool: &mut ActiveTool, notches: f32) {
    if matches!(tool.kind, ToolKind::Spotlight) {
        const STEP: f32 = 5.0;
        tool.spotlight_size = (tool.spotlight_size + notches * STEP).clamp(20.0, 800.0);
    } else {
        const SIZE_MIN: f32 = 1.0;
        const SIZE_MAX: f32 = 80.0;
        tool.size = (tool.size + notches).clamp(SIZE_MIN, SIZE_MAX);
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

/// Most a zoom to fit enlarges the whole slide: a slide holding one
/// small note should not fill the screen with it.
pub const FIT_MAX_ZOOM: f32 = 4.0;
/// Most a zoom to the selection enlarges: one handwritten word comes up
/// to read comfortably, not to a blur.
pub const FIT_SELECTION_MAX_ZOOM: f32 = 8.0;

/// The view (`pan`, `zoom`) that shows the canvas box `[x0, y0, x1, y1]`
/// whole and centred in `area` — `[x, y, width, height]` in the same
/// screen space as `pan` (relative to the canvas origin) — zoomed in no
/// further than `max_zoom`.
pub fn fit_view([x0, y0, x1, y1]: [f32; 4], area: [f32; 4], max_zoom: f32) -> ([f32; 2], f32) {
    let (w, h) = ((x1 - x0).max(1e-3), (y1 - y0).max(1e-3));
    let (aw, ah) = (area[2].max(1.0), area[3].max(1.0));
    let zoom = (aw / w).min(ah / h).min(max_zoom).clamp(MIN_ZOOM, MAX_ZOOM);
    let centre = [area[0] + aw * 0.5, area[1] + ah * 0.5];
    let pan = [centre[0] - (x0 + x1) * 0.5 * zoom, centre[1] - (y0 + y1) * 0.5 * zoom];
    (pan, zoom)
}

/// A view gliding to a new pan and zoom over a few frames (zoom to fit,
/// zoom to the selection), so the eye can follow where it went.
#[derive(Debug, Clone, Copy)]
pub struct ViewGlide {
    /// Slide it belongs to: going to another slide ends it.
    pub slide: usize,
    from: ([f32; 2], f32),
    to: ([f32; 2], f32),
    /// Screen point (relative to the canvas origin) the glide is
    /// measured about: the middle of the area being fitted.
    about: [f32; 2],
    start: std::time::Instant,
    /// The view this glide last set. Anything else found there means the
    /// user panned or zoomed meanwhile, which ends the glide.
    last: ([f32; 2], f32),
}

/// How long a glide takes.
const GLIDE: std::time::Duration = std::time::Duration::from_millis(240);

impl ViewGlide {
    pub fn new(slide: usize, from: ([f32; 2], f32), to: ([f32; 2], f32), about: [f32; 2]) -> Self {
        ViewGlide { slide, from, to, about, start: std::time::Instant::now(), last: from }
    }

    /// Move the view one frame along. Returns `false` once the glide is
    /// over — arrived, or given up because the view was changed by
    /// something else — and the caller should drop it.
    pub fn step(&mut self, pan: &mut [f32; 2], zoom: &mut f32, now: std::time::Instant) -> bool {
        if *pan != self.last.0 || *zoom != self.last.1 {
            return false;
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / GLIDE.as_secs_f32()).min(1.0);
        let (p, z) = glide_view(self.from, self.to, self.about, ease_out(t));
        *pan = p;
        *zoom = z;
        self.last = (p, z);
        t < 1.0
    }
}

/// Fast at first, settling gently.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// The view `k` (0..1) of the way from `from` to `to`. Zoom moves
/// evenly in scale (halving and doubling take the same time), and the
/// canvas point under `about` slides straight from where it was to
/// where it ends up, so the view neither swings away nor overshoots.
fn glide_view(from: ([f32; 2], f32), to: ([f32; 2], f32), about: [f32; 2], k: f32) -> ([f32; 2], f32) {
    if k >= 1.0 {
        return to;
    }
    let (z0, z1) = (from.1.max(MIN_ZOOM), to.1.max(MIN_ZOOM));
    let z = (z0.ln() + (z1.ln() - z0.ln()) * k).exp();
    let q0 = [(about[0] - from.0[0]) / z0, (about[1] - from.0[1]) / z0];
    let q1 = [(about[0] - to.0[0]) / z1, (about[1] - to.0[1]) / z1];
    let q = [q0[0] + (q1[0] - q0[0]) * k, q0[1] + (q1[1] - q0[1]) * k];
    ([about[0] - q[0] * z, about[1] - q[1] * z], z)
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
        handle_scroll(1, 1.0, [100.0, 100.0], false, false, false, &mut tool, &mut m, &mut bg);
        assert!(m.active().zoom > 1.0);
        assert!(!m.active().dirty, "zooming must not trigger a slide save");
        handle_scroll(1, 1.0, [100.0, 100.0], false, true, false, &mut tool, &mut m, &mut bg);
        assert!(m.active().dirty, "a layer-opacity change must be saved");
    }

    #[test]
    fn on_a_locked_slide_the_wheel_only_zooms() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = ToolKind::Pen;
        let size = tool.size;
        let mut canvas = Canvas::default();
        canvas.dirty = false;
        let mut m = CanvasManager::for_tests(vec![canvas]);
        m.set_locked(0, true);
        let mut bg = 0.5;
        handle_scroll(1, 1.0, [100.0, 100.0], false, false, false, &mut tool, &mut m, &mut bg);
        assert!(m.active().zoom > 1.0, "the pen's wheel zooms instead");
        assert_eq!(tool.size, size);
        handle_scroll(1, 1.0, [100.0, 100.0], false, true, false, &mut tool, &mut m, &mut bg);
        assert_eq!(m.active().layers[0].opacity, 1.0);
        assert!(!m.active().dirty);
    }

    /// The wheel sizes the pen, but with Space (the pan key) held it
    /// zooms, so the pen and brushes never need a tool switch to zoom.
    #[test]
    fn space_and_the_wheel_zoom_while_the_pen_is_out() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = ToolKind::Pen;
        let size = tool.size;
        let mut m = CanvasManager::for_tests(vec![Canvas::default()]);
        let mut bg = 0.5;
        handle_scroll(1, 1.0, [100.0, 100.0], false, false, false, &mut tool, &mut m, &mut bg);
        assert_eq!(tool.size, size + 1.0, "without Space the wheel sizes the pen");
        assert_eq!(m.active().zoom, 1.0);
        handle_scroll(1, 1.0, [100.0, 100.0], false, false, true, &mut tool, &mut m, &mut bg);
        assert!(m.active().zoom > 1.0, "with Space it zooms in");
        handle_scroll(-1, -1.0, [100.0, 100.0], false, false, true, &mut tool, &mut m, &mut bg);
        handle_scroll(-1, -1.0, [100.0, 100.0], false, false, true, &mut tool, &mut m, &mut bg);
        assert!(m.active().zoom < 1.0, "and out");
        assert_eq!(tool.size, size + 1.0, "leaving the size alone");
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

    fn on_screen(view: ([f32; 2], f32), p: [f32; 2]) -> [f32; 2] {
        [view.0[0] + p[0] * view.1, view.0[1] + p[1] * view.1]
    }

    #[test]
    fn fitting_shows_the_box_whole_and_centred_in_the_area() {
        // A wide box in a squarish area: width decides the zoom.
        let area = [20.0, 60.0, 400.0, 300.0];
        let view = fit_view([100.0, 100.0, 900.0, 300.0], area, 100.0);
        assert!((view.1 - 0.5).abs() < 1e-6);
        let (a, b) = (on_screen(view, [100.0, 100.0]), on_screen(view, [900.0, 300.0]));
        assert!((a[0] - 20.0).abs() < 1e-3 && (b[0] - 420.0).abs() < 1e-3, "edge to edge across: {a:?} {b:?}");
        let mid = (a[1] + b[1]) * 0.5;
        assert!((mid - 210.0).abs() < 1e-3, "centred down: {mid}");
        // A tiny box stops at the zoom limit, still centred.
        let view = fit_view([0.0, 0.0, 2.0, 2.0], area, FIT_MAX_ZOOM);
        assert_eq!(view.1, FIT_MAX_ZOOM);
        let c = on_screen(view, [1.0, 1.0]);
        assert!((c[0] - 220.0).abs() < 1e-3 && (c[1] - 210.0).abs() < 1e-3);
    }

    #[test]
    fn a_glide_starts_where_it_was_ends_where_it_goes_and_zooms_evenly() {
        let from = ([0.0, 0.0], 1.0);
        let to = ([-300.0, 50.0], 4.0);
        let about = [200.0, 150.0];
        assert_eq!(glide_view(from, to, about, 0.0), from);
        assert_eq!(glide_view(from, to, about, 1.0), to);
        // Halfway in time is halfway in scale: 2×, not 2.5×.
        assert!((glide_view(from, to, about, 0.5).1 - 2.0).abs() < 1e-4);
    }

    #[test]
    fn a_glide_gives_way_to_the_user() {
        let start = std::time::Instant::now();
        let mut g = ViewGlide::new(0, ([0.0, 0.0], 1.0), ([10.0, 10.0], 2.0), [100.0, 100.0]);
        let (mut pan, mut zoom) = ([0.0, 0.0], 1.0);
        assert!(g.step(&mut pan, &mut zoom, start + std::time::Duration::from_millis(60)));
        assert!(zoom > 1.0 && zoom < 2.0);
        // The wheel moved the view: the glide stops and leaves it be.
        zoom *= 1.1;
        let mine = (pan, zoom);
        assert!(!g.step(&mut pan, &mut zoom, start + std::time::Duration::from_millis(120)));
        assert_eq!((pan, zoom), mine);
        // Left alone, one runs to its end.
        let mut g = ViewGlide::new(0, ([0.0, 0.0], 1.0), ([10.0, 10.0], 2.0), [100.0, 100.0]);
        let (mut pan, mut zoom) = ([0.0, 0.0], 1.0);
        assert!(!g.step(&mut pan, &mut zoom, start + std::time::Duration::from_secs(1)));
        assert_eq!((pan, zoom), ([10.0, 10.0], 2.0));
    }
}
