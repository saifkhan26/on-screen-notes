//! Two render paths for canvas content:
//!
//! 1. **`paint_canvas_egui`** — paints directly via `egui::Painter`. egui
//!    pushes the geometry as GPU triangles through `wgpu`. There is **no
//!    CPU rasterisation and no per-frame texture upload**, which is what
//!    keeps the live drawing surface low-latency. This is the path the
//!    overlay uses every frame.
//!
//! 2. **`render_canvas`** — rasterises the canvas into a `tiny_skia::Pixmap`.
//!    Used only by the screenshot pipeline (we need RGBA pixels to write a
//!    PNG and to alpha-blend onto a screen capture).
//!
//! ## Why the split
//!
//! Our previous version rasterised every frame and re-uploaded a 1200×800
//! texture (~3.8 MB) to the GPU at 60 fps. As stroke count grew, CPU work
//! and bandwidth pegged → laggy pen. Reference: [Rnote](https://github.com/flxzt/rnote)
//! solves the same problem by caching finished strokes and rendering only
//! the live one each frame. We take the simpler "all-GPU" route which has
//! the same end effect because GPU stroke triangles are essentially free
//! to redraw.

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{BorderStyle, Shape};
use crate::canvas::stroke::{Stroke, StrokeStyle};
use std::sync::atomic::{AtomicBool, AtomicU32};
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, PixmapMut, Stroke as TsStroke, StrokeDash, Transform,
};

// ---- Fan-corner globals -----------------------------------------------
//
// The ribbon mesh builder consults these atomics each time it paints a
// stroke. App reads `config.smoothing` once per frame and pushes the
// flag + step (in radians) here so render code does not need to thread
// an extra parameter through every paint helper. Atomic = lock-free
// read on the hot path.
static FAN_CORNERS_ENABLED:   AtomicBool = AtomicBool::new(false);
static FAN_CORNERS_STEP_BITS: AtomicU32  = AtomicU32::new(0x3E4CCCCD); // ≈ 0.20 rad

/// Push the user's fan-corner setting into render-side globals. Called
/// once per frame from the app loop before any stroke is painted.
pub fn set_fan_corners(enabled: bool, step_rad: f32) {
    FAN_CORNERS_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    FAN_CORNERS_STEP_BITS.store(step_rad.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// Marching-ants dash speed in canvas-local pixels per second.
const DASH_SPEED_PX_PER_SEC: f32 = 30.0;
/// Dash pattern for animated borders: 12 px on, 6 px off.
// Dash visible length and gap, in canvas-local pixels. Round caps
// (added by `paint_round_line`) extend each dash by half the stroke
// width on both ends, so the *visible* gap is `DASH_OFF − stroke_width`.
// Picked so a typical 6 px stroke still shows a ~12 px gap.
const DASH_ON:  f32 = 10.0;
const DASH_OFF: f32 = 18.0;

/// Current dash-phase offset for marching-ants borders. Derived from a
/// once-initialised "app boot" Instant so all animated borders march
/// in sync, and so the function is cheap to call (no allocations).
fn dash_phase() -> f32 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BOOT: OnceLock<Instant> = OnceLock::new();
    let boot = BOOT.get_or_init(Instant::now);
    let secs = boot.elapsed().as_secs_f32();
    let period = DASH_ON + DASH_OFF;
    (secs * DASH_SPEED_PX_PER_SEC) % period
}

// =============================================================================
// 1) GPU-accelerated path: egui Painter
// =============================================================================

/// Paint the entire canvas (shapes + strokes + previews) directly via the
/// egui `Painter`. Each line segment becomes two triangles on the GPU.
///
/// `origin` is where the canvas's `(0, 0)` lives on screen, in egui logical
/// coordinates. The caller passes in `canvas_rect.min`. We add the canvas's
/// own `pan` on top of that and scale by `zoom`.
pub fn paint_canvas_egui(
    painter: &egui::Painter,
    origin: egui::Pos2,
    canvas: &Canvas,
    preview_shape: Option<&Shape>,
    preview_stroke: Option<&Stroke>,
    // Position-smoothing passes for the live preview path (committed
    // strokes already carry a baked cache built with this same value).
    position_passes: usize,
    // Live laser-pointer strokes painted on top of the canvas with a
    // time-decaying alpha. `(Stroke, pen_up_instant)` — renderer
    // computes `1 - elapsed / LASER_FADE_SECS` per entry and passes
    // that through `paint_stroke` as the layer opacity. The app loop
    // prunes faded entries each frame.
    laser_strokes: &[(Stroke, std::time::Instant)],
    // GPU texture handle for each layer's raster image (frozen
    // frame). Indexed by layer position; `None` = no image on that
    // layer. Built by the overlay's texture cache to avoid uploading
    // PNG bytes every frame.
    layer_image_textures: &[Option<egui::TextureId>],
    // Per-shape texture handles for `Shape::Raster` sticker fills.
    // Indexed by `[layer_index][shape_index]`. Same cache pattern
    // as `layer_image_textures` — `None` for non-raster shapes.
    shape_textures: &[Vec<Option<egui::TextureId>>],
    // Optional pan/zoom override. When `None` the canvas's own
    // `pan` and `zoom` drive the screen transform (normal overlay
    // render). When `Some` the renderer uses the supplied values
    // instead — letting callers re-render the canvas magnified
    // into a clipped region (e.g. the live loupe overlay).
    transform_override: Option<([f32; 2], f32)>,
    // In-progress Shift+Space layer gesture: `(layer index, scale,
    // canvas-local offset)`, i.e. the similarity `p → a · p + b` the
    // named layer previews under. A drag only fills `b`; the wheel adds
    // scale about the cursor. Nothing is written back to the canvas
    // until the gesture ends, at which point `Canvas::transform_layer`
    // bakes the same similarity into the layer's geometry. `None`
    // outside a gesture.
    layer_xform: Option<(usize, f32, [f32; 2])>,
) {
    // Pre-compute the pan-zoom transform as a closure so we don't
    // recompute the same maths inside every loop.
    let (view_pan, zoom) = match transform_override {
        Some((p, z)) => (p, z),
        None => (canvas.pan, canvas.zoom),
    };
    let pan_x = origin.x + view_pan[0];
    let pan_y = origin.y + view_pan[1];
    let to_screen = |p: [f32; 2]| egui::pos2(pan_x + p[0] * zoom, pan_y + p[1] * zoom);

    // Paint each layer in stack order: index 0 paints first (bottom),
    // last layer paints last (top). Within a layer, shapes first then
    // strokes, mirroring the historical flat-canvas ordering. Hidden
    // layers are skipped entirely. Per-layer opacity is threaded into
    // every paint call so the renderer multiplies each colour's alpha
    // by the layer's setting (Shift+wheel adjusts this live).
    for (li, layer) in canvas.layers.iter().enumerate() {
        if !layer.visible { continue; }
        let op = layer.opacity.clamp(0.0, 1.0);
        if op <= 0.0 { continue; }
        // Shadow the canvas-wide `to_screen` with one that folds in this
        // layer's in-progress gesture transform (identity for every
        // layer but the one being dragged / zoomed). Everything the
        // layer owns — raster plate, shapes, strokes — goes through it,
        // so the layer moves and scales as a rigid unit.
        let (lscale, off) = match layer_xform {
            Some((di, a, b)) if di == li => (a, b),
            _ => (1.0, [0.0, 0.0]),
        };
        let to_screen = |p: [f32; 2]| {
            egui::pos2(
                pan_x + (lscale * p[0] + off[0]) * zoom,
                pan_y + (lscale * p[1] + off[1]) * zoom,
            )
        };
        // Widths, font sizes and dash patterns are scaled by the `zoom`
        // the paint helpers receive rather than by `to_screen`, so the
        // previewed layer needs its gesture scale folded in here too —
        // otherwise a layer shrinking under the wheel would keep
        // full-thickness ink until the gesture committed.
        let zoom = zoom * lscale;
        // Raster image (frozen frame) goes UNDER the layer's strokes
        // and shapes — it is the "background plate" the user is
        // annotating over.
        if let (Some(img), Some(Some(tex_id))) =
            (layer.image.as_ref(), layer_image_textures.get(li))
        {
            let (mut w, mut h) = (img.logical_size[0], img.logical_size[1]);
            if w <= 0.0 || h <= 0.0 {
                // Older save without logical_size: fall back to the
                // physical pixel dimensions. Mismatched DPI will skew
                // placement but the image still renders.
                w = img.size[0] as f32;
                h = img.size[1] as f32;
            }
            // Anchor at the recorded canvas origin (zero for older
            // saves) so a freeze captured under a panned/zoomed
            // canvas lands at the visible client area, not at the
            // canvas-local origin.
            let o = img.canvas_origin;
            let p0 = to_screen(o);
            let p1 = to_screen([o[0] + w, o[1] + h]);
            let rect = egui::Rect::from_two_pos(p0, p1);
            let tint = egui::Color32::from_rgba_unmultiplied(255, 255, 255, (op * 255.0) as u8);
            painter.image(*tex_id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
        }
        let shape_tex_for_layer = shape_textures.get(li);
        for (si, shape) in layer.shapes.iter().enumerate() {
            let tex = shape_tex_for_layer
                .and_then(|v| v.get(si))
                .copied()
                .flatten();
            paint_shape(painter, &to_screen, shape, zoom, op, tex);
        }
        for stroke in &layer.strokes {
            paint_stroke(painter, &to_screen, stroke, zoom, position_passes, op);
        }
    }
    // Previews always paint on top of every layer regardless of which
    // is active — the user expects to see their in-progress mark even
    // if the layer they are drawing into sits below others. We use the
    // active layer's opacity so the preview matches what the committed
    // stroke will look like.
    let active_op = canvas
        .layers
        .get(canvas.active_layer)
        .map(|l| l.opacity.clamp(0.0, 1.0))
        .unwrap_or(1.0);
    if let Some(s) = preview_shape {
        paint_shape(painter, &to_screen, s, zoom, active_op, None);
    }
    if let Some(s) = preview_stroke {
        paint_stroke(painter, &to_screen, s, zoom, position_passes, active_op);
    }

    // Laser strokes — painted on top of every layer with a time-based
    // alpha so the trail fades out over `LASER_FADE_SECS`. We multiply
    // the alpha into `layer_opacity` (which `paint_stroke` already
    // honours), so no extra paint plumbing is required.
    if !laser_strokes.is_empty() {
        let now = std::time::Instant::now();
        for (stroke, up_at) in laser_strokes {
            let elapsed = now.duration_since(*up_at).as_secs_f32();
            let alpha = (1.0 - elapsed / LASER_FADE_SECS).clamp(0.0, 1.0);
            if alpha <= 0.0 { continue; }
            paint_stroke(painter, &to_screen, stroke, zoom, position_passes, alpha);
        }
    }
}

/// Number of seconds a laser-pointer stroke takes to fade from full
/// opacity to zero after the user lifts the pen. Tuned for "trail
/// visible long enough to point at a thing across the room, gone
/// before the next sentence starts" — Sticky Notes / Krita /
/// PowerPoint use a similar 0.8–1.5 s window.
pub const LASER_FADE_SECS: f32 = 1.0;

/// Build an `egui::Color32` from raw RGBA bytes, scaling the alpha
/// channel by `layer_opacity` (clamped 0..=1). Every paint helper goes
/// through this function so layer-opacity application is centralised.
fn col(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    let op = layer_opacity.clamp(0.0, 1.0);
    let a = ((c[3] as f32) * op).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

fn paint_shape(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    shape: &Shape,
    zoom: f32,
    layer_opacity: f32,
    raster_tex: Option<egui::TextureId>,
) {
    match shape {
        Shape::Rect { a, b, color, stroke_width, border } => {
            let r = egui::Rect::from_two_pos(to_screen(*a), to_screen(*b));
            // Corner radius scales gently with stroke width, capped at
            // ~half the shorter side so a tiny rectangle stays a
            // rectangle. `f32::clamp` would panic when the preview
            // rect is near-zero (drag just started, width=0): compute
            // the max first and use plain `min` so we degrade
            // gracefully to a sharp corner on tiny boxes.
            let min_side    = r.width().min(r.height()).abs();
            let want_corner = (*stroke_width * 2.5 * zoom).max(0.0);
            let max_corner  = (min_side * 0.45).max(0.0);
            let corner      = want_corner.min(max_corner);
            let stroke = egui::Stroke::new(*stroke_width * zoom, col(*color, layer_opacity));
            match border {
                BorderStyle::Solid => {
                    painter.rect_stroke(r, egui::Rounding::same(corner), stroke);
                }
                BorderStyle::DashedAnimated => {
                    // egui has no rounded-rect dashed stroke; emit four
                    // dashed sides instead. Corner rounding is dropped
                    // for the dashed variant — animated dashes look odd
                    // wrapping a curved corner, so sharp corners are
                    // cleaner and standard for marching-ants outlines.
                    paint_dashed_rect(painter, r, stroke, zoom);
                }
            }
        }
        Shape::Ellipse { a, b, color, stroke_width, border } => {
            // egui has no native ellipse stroke; approximate with a
            // 64-segment polyline.
            let cx = (a[0] + b[0]) * 0.5;
            let cy = (a[1] + b[1]) * 0.5;
            let rx = ((b[0] - a[0]) * 0.5).abs();
            let ry = ((b[1] - a[1]) * 0.5).abs();
            const N: usize = 64;
            let pts: Vec<egui::Pos2> = (0..=N)
                .map(|i| {
                    let t = (i as f32 / N as f32) * std::f32::consts::TAU;
                    to_screen([cx + rx * t.cos(), cy + ry * t.sin()])
                })
                .collect();
            let stroke = egui::Stroke::new(*stroke_width * zoom, col(*color, layer_opacity));
            match border {
                BorderStyle::Solid => {
                    painter.add(egui::Shape::line(pts, stroke));
                }
                BorderStyle::DashedAnimated => {
                    paint_dashed_polyline(painter, &pts, stroke, zoom);
                }
            }
        }
        Shape::Line { a, b, color, stroke_width, border } => {
            let sa = to_screen(*a);
            let sb = to_screen(*b);
            let c = col(*color, layer_opacity);
            match border {
                BorderStyle::Solid => {
                    paint_round_line(painter, sa, sb, *stroke_width * zoom, c);
                }
                BorderStyle::DashedAnimated => {
                    let stroke = egui::Stroke::new(*stroke_width * zoom, c);
                    paint_dashed_polyline(painter, &[sa, sb], stroke, zoom);
                }
            }
        }
        Shape::Arrow { a, b, color, stroke_width, border } => {
            let w = *stroke_width * zoom;
            let c = col(*color, layer_opacity);
            let sa = to_screen(*a);
            let sb = to_screen(*b);

            // Shaft: solid round-capped, or dashed-animated.
            match border {
                BorderStyle::Solid => {
                    paint_round_line(painter, sa, sb, w, c);
                }
                BorderStyle::DashedAnimated => {
                    let stroke = egui::Stroke::new(w, c);
                    paint_dashed_polyline(painter, &[sa, sb], stroke, zoom);
                }
            }

            // Two head lines fan out from `b`, ±25° from the shaft, length
            // proportional to stroke width so the arrowhead always looks
            // in scale with the line. Both head lines use round caps and
            // the joint at `b` is covered by the shaft's end disc.
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let ux = dx / len;
            let uy = dy / len;
            let head_len = (stroke_width * 4.5).max(10.0);
            let theta: f32 = 25.0_f32.to_radians();
            let (s1, c1) = (theta.sin(), theta.cos());
            let h1 = [b[0] - (ux * c1 - uy * s1) * head_len,
                     b[1] - (ux * s1 + uy * c1) * head_len];
            let h2 = [b[0] - (ux * c1 + uy * s1) * head_len,
                     b[1] - (-ux * s1 + uy * c1) * head_len];
            paint_round_line(painter, sb, to_screen(h1), w, c);
            paint_round_line(painter, sb, to_screen(h2), w, c);
        }
        Shape::Raster { pos, size, .. } => {
            if let Some(tex_id) = raster_tex {
                let p0 = to_screen(*pos);
                let p1 = to_screen([pos[0] + size[0], pos[1] + size[1]]);
                let rect = egui::Rect::from_two_pos(p0, p1);
                let tint = egui::Color32::from_rgba_unmultiplied(
                    255, 255, 255,
                    (layer_opacity * 255.0).clamp(0.0, 255.0) as u8,
                );
                painter.image(
                    tex_id,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    tint,
                );
            }
        }
        Shape::Text { pos, content, font_size, color } => {
            let c = col(*color, layer_opacity);
            // The shape's `font_size` is in canvas-local logical px;
            // multiply by zoom so the on-screen height honours the
            // canvas's pan/zoom transform like every other shape.
            let size = (*font_size * zoom).max(2.0);
            // Prefer a "handwritten" family if the app registered one
            // at startup; egui silently falls back to Proportional
            // when the family is unknown, so this is safe regardless
            // of whether a font file is bundled.
            let font_id = egui::FontId::new(size, egui::FontFamily::Name("handwritten".into()));
            // 1.25 × font_size is the standard "comfortable reading"
            // line height; matches what most browsers default to.
            let line_h = size * 1.25;
            let top_left = to_screen(*pos);
            for (i, line) in content.split('\n').enumerate() {
                let lp = egui::pos2(top_left.x, top_left.y + i as f32 * line_h);
                painter.text(lp, egui::Align2::LEFT_TOP, line, font_id.clone(), c);
            }
        }
    }
}

/// Walk `pts` and emit dashed line segments using the global
/// `dash_phase()` so consecutive frames march in lock-step. `zoom`
/// scales the dash pattern so dashes stay readable at low canvas
/// zoom and don't turn into one long block at high zoom.
pub(crate) fn paint_dashed_polyline(
    painter: &egui::Painter,
    pts: &[egui::Pos2],
    stroke: egui::Stroke,
    zoom: f32,
) {
    if pts.len() < 2 { return; }
    // Scale the dash pattern by zoom so it reads at a consistent size
    // in screen space. Floor at 2 px so dashes are never sub-pixel.
    let on  = (DASH_ON  * zoom).max(2.0);
    let off = (DASH_OFF * zoom).max(1.0);
    let period = on + off;
    let phase = dash_phase() * zoom;

    // We walk the polyline in arc-length, alternating "draw" and "skip"
    // chunks. `cursor` tracks distance along the polyline modulo
    // `period`; if we're inside `[0, on)` we paint, otherwise we skip.
    // To make this concrete: shift cursor by `-phase` so animation
    // advances the pattern forward in time (positive phase = dashes
    // appear to move along the path).
    let mut cursor = -phase;
    // Normalise into [0, period) for cleaner arithmetic below.
    while cursor < 0.0 { cursor += period; }

    for win in pts.windows(2) {
        let p0 = win[0];
        let p1 = win[1];
        let dx = p1.x - p0.x;
        let dy = p1.y - p0.y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1e-4 { continue; }
        let mut walked = 0.0;
        while walked < seg_len {
            // Distance from cursor to next phase boundary.
            let in_on = cursor < on;
            let remaining_in_phase = if in_on { on - cursor } else { period - cursor };
            let chunk = remaining_in_phase.min(seg_len - walked);
            if in_on {
                let t0 = walked / seg_len;
                let t1 = (walked + chunk) / seg_len;
                let a = egui::pos2(p0.x + dx * t0, p0.y + dy * t0);
                let b = egui::pos2(p0.x + dx * t1, p0.y + dy * t1);
                paint_dash_pill(painter, a, b, stroke.width, stroke.color);
            }
            walked += chunk;
            cursor += chunk;
            if cursor >= period { cursor -= period; }
        }
    }
}

/// Specialise the polyline dash walker for an axis-aligned rectangle.
/// We walk the four sides in clockwise order so dashes flow around
/// the perimeter rather than restarting on each side.
fn paint_dashed_rect(
    painter: &egui::Painter,
    r: egui::Rect,
    stroke: egui::Stroke,
    zoom: f32,
) {
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    paint_dashed_polyline(painter, &pts, stroke, zoom);
}

/// Paint a thick line with **round caps** by stroking the line and
/// stamping a filled disc of the same radius at both endpoints.
/// epaint 0.29 has no round-cap option on `Stroke`; this is the
/// cheapest portable round-cap.
fn paint_round_line(
    painter: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    width: f32,
    color: egui::Color32,
) {
    painter.line_segment([a, b], egui::Stroke::new(width, color));
    let r = width * 0.5;
    if r >= 0.5 {
        painter.circle_filled(a, r, color);
        painter.circle_filled(b, r, color);
    }
}

/// Round-capped dash. Tighter caps than `paint_round_line` because
/// `circle_filled` adds ~1 px of AA halo that makes a half-stroke
/// disc visibly larger than the line's feathered ends — at short
/// dash lengths the caps end up reading as "dots with a thread
/// between" rather than a pill. Scaling the cap radius by 0.42
/// matches the line's apparent width while keeping the dash's
/// rounded silhouette.
fn paint_dash_pill(
    painter: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    width: f32,
    color: egui::Color32,
) {
    painter.line_segment([a, b], egui::Stroke::new(width, color));
    let r = width * 0.42;
    if r >= 0.5 {
        painter.circle_filled(a, r, color);
        painter.circle_filled(b, r, color);
    }
}

/// Paint a single stroke. Two paths:
///
/// 1. **Committed strokes** carry a pre-built `cache` (set by
///    `Stroke::build_cache` in `Canvas::add_stroke`). We just walk the
///    cached polyline and emit `line_segment`s — no Catmull-Rom math,
///    no allocation. This is the fast path that runs every frame.
///
/// 2. **Live in-progress strokes** lack a cache (their geometry changes
///    every pen sample). We do the Catmull-Rom subdivision inline.
///    The live stroke is one stroke at most, so this stays cheap.
fn paint_stroke(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    stroke: &Stroke,
    zoom: f32,
    position_passes: usize,
    layer_opacity: f32,
) {
    if stroke.samples.is_empty() {
        return;
    }
    let color = col(stroke.color, layer_opacity);

    // Pencil look: drop the colour alpha to ~55 % so the stroke reads as
    // translucent graphite. Width jitter (added inside the mesh build)
    // contributes the grainy edge.
    let paint_color = match stroke.style {
        StrokeStyle::Default => color,
        StrokeStyle::Pencil  => pencil_color(stroke.color, layer_opacity),
        StrokeStyle::Marker  => marker_color(stroke.color, layer_opacity),
        StrokeStyle::Airbrush => airbrush_color(stroke.color, layer_opacity),
    };

    if stroke.samples.len() == 1 {
        let s = stroke.samples[0];
        let pressure = if matches!(stroke.style, StrokeStyle::Marker) { 1.0 } else { s.pressure.max(0.1) };
        let r = (stroke.base_width * 0.5 * pressure).max(0.4) * zoom;
        painter.circle_filled(to_screen(s.pos), r, paint_color);
        return;
    }

    if let Some(cache) = &stroke.cache {
        // Fill goes UNDER the outline so the user-drawn line still
        // reads as the boundary on top of the colour blob.
        if stroke.filled {
            paint_fill_from_polyline(
                painter, to_screen, &cache.points,
                fill_color(stroke.color, layer_opacity),
            );
        }
        // Marker = constant-width ribbon (no pressure modulation).
        // Airbrush = soft-disc stamp walk (handled inside the
        // grouped polyline helper via the style discriminant).
        let const_widths: Vec<f32>;
        let widths_for_paint: &[f32] = if matches!(stroke.style, StrokeStyle::Marker) {
            const_widths = vec![stroke.base_width; cache.widths.len()];
            &const_widths
        } else {
            &cache.widths
        };
        paint_polyline_grouped(
            painter, to_screen, &cache.points, widths_for_paint,
            paint_color, zoom, stroke.style,
        );
        return;
    }

    // Slow path: live stroke (no cache yet) — recompute Catmull-Rom inline.
    paint_stroke_live(painter, to_screen, stroke, zoom, position_passes, layer_opacity);
}

/// Paint a polyline whose width varies along its length as smoothly-joined
/// **groups** of constant-width line segments. This is the trick that
/// gives us anti-aliased, smoothly-joined strokes while keeping the
/// pressure-driven width variation:
///
///   * `egui::Shape::line(points, Stroke)` is rendered as one *single* mesh
///     with proper miter joins and feathered (anti-aliased) edges.
///   * Pure single-Stroke means the *whole* polyline shares one width.
///     Pressure variation needs different widths.
///
/// We split the cached polyline into runs where consecutive widths are
/// within 25 % of the running average, emit each run as its own
/// `Shape::line`, and overlap the boundary point so adjacent runs tuck
/// into each other without visible gaps.
/// Paint a variable-width centerline as a **single triangle-strip
/// ribbon mesh** plus round caps at each endpoint.
///
/// Why a custom mesh instead of `egui::Shape::line` + disc stamps:
///
///   * `Shape::line` cannot vary width along its length.
///   * Disc stamping (our previous approach) paints N overlapping
///     anti-aliased circles. Egui feathers every disc edge (1.5 px),
///     and at high disc density the feathered fringes accumulate into
///     a visible scale-mail pattern *inside* the stroke — exactly the
///     "circle edges visible" artifact the user saw.
///
/// One mesh = one feathered outline. The interior is a flat triangle
/// fill with no internal seams, so no fringe stacking is possible.
fn paint_polyline_grouped(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    points: &[[f32; 2]],
    widths: &[f32],
    color: egui::Color32,
    zoom: f32,
    style: StrokeStyle,
) {
    let n = points.len();
    if n == 0 { return; }
    if n == 1 {
        let r = widths[0] * 0.5 * zoom;
        if r >= 0.4 {
            painter.circle_filled(to_screen(points[0]), r, color);
        }
        return;
    }

    // Pencil = disc-stamp rendering.
    //
    // The default ribbon-mesh strip suffers from "bowtie" self-
    // overlap on tight curves: at a sharp inner turn, two adjacent
    // strip quads fold over each other on the inner side. With a
    // fully opaque colour this is invisible (same colour stacks =
    // same colour), but a translucent pencil colour stacks alpha →
    // visible dark scaly bands, exactly the artefact in the report.
    //
    // Stamping a circle at constant arc-length intervals dodges the
    // problem entirely — there is no per-segment geometry that can
    // fold, only a sequence of overlapping discs. Density variation
    // from the per-stamp width jitter is what gives the graphite
    // texture.
    if let StrokeStyle::Pencil = style {
        paint_pencil_stamps(painter, to_screen, points, widths, color, zoom);
        return;
    }
    if let StrokeStyle::Airbrush = style {
        paint_airbrush_stamps(painter, to_screen, points, widths, color, zoom);
        return;
    }

    // Emit one mesh with a built-in feather band along the left and
    // right edges. For each polyline point we generate 4 vertices,
    // outer→inner→inner→outer, where the *outer* vertices carry
    // alpha=0. Linear vertex-colour interpolation gives a soft 1 px
    // fringe along both edges → proper anti-aliasing for raw meshes
    // (egui only auto-feathers `Shape::line` / `circle_filled`, not
    // user-built meshes).
    const FEATHER: f32 = 1.0;
    let trans = egui::Color32::from_rgba_premultiplied(0, 0, 0, 0);

    // ---- Round caps painted BEFORE the ribbon mesh -------------
    // Painting discs first then the ribbon on top ensures the ribbon's
    // anti-aliased fringe overdraws the disc's auto-AA halo cleanly.
    // The disc's center half is invisible under the ribbon's solid
    // interior; only its backward hemisphere remains visible — which
    // is exactly the round-cap silhouette we want.
    //
    // We also shrink each disc by `FEATHER` so its halo lands INSIDE
    // the ribbon's solid interior instead of stacking with the
    // ribbon's outer feather strip. Without that shrink, the user
    // sees a darker bead at every endpoint because the two AA
    // gradients overlay in the same perpendicular pixel band.
    let (j0, jn) = match style {
        StrokeStyle::Default | StrokeStyle::Marker => (1.0_f32, 1.0_f32),
        StrokeStyle::Pencil  => (
            0.78 + 0.32 * hash01(0),
            0.78 + 0.32 * hash01((n - 1) as u32),
        ),
        StrokeStyle::Airbrush => (1.0_f32, 1.0_f32),
    };
    let r0 = (widths[0]     * 0.5 * j0 * zoom - FEATHER * 0.5).max(0.4);
    let rn = (widths[n - 1] * 0.5 * jn * zoom - FEATHER * 0.5).max(0.4);
    if r0 >= 0.4 {
        painter.circle_filled(to_screen(points[0]), r0, color);
    }
    if rn >= 0.4 {
        painter.circle_filled(to_screen(points[n - 1]), rn, color);
    }

    let mut mesh = egui::Mesh::default();
    mesh.vertices.reserve(n * 4);
    mesh.indices.reserve((n - 1) * 18);

    for i in 0..n {
        let dir = if i == 0 {
            [points[1][0] - points[0][0], points[1][1] - points[0][1]]
        } else if i == n - 1 {
            [points[n - 1][0] - points[n - 2][0], points[n - 1][1] - points[n - 2][1]]
        } else {
            [points[i + 1][0] - points[i - 1][0], points[i + 1][1] - points[i - 1][1]]
        };
        let len = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt().max(1.0e-6);
        // Unit normal: rotate the tangent 90° CCW.
        let nx = -dir[1] / len;
        let ny =  dir[0] / len;
        // Pencil style: jitter each polyline point's half-width with a
        // deterministic hash so the ribbon edge gains a graphite-like
        // graininess instead of reading as a perfect ribbon.
        let jitter = match style {
            StrokeStyle::Default | StrokeStyle::Marker => 1.0,
            StrokeStyle::Pencil  => 0.78 + 0.32 * hash01(i as u32),
            // Airbrush never reaches the ribbon path — stamp walk
            // bailed out above. Keep a sane default for completeness.
            StrokeStyle::Airbrush => 1.0,
        };
        let half = (widths[i] * 0.5 * jitter * zoom).max(0.4);
        let p = to_screen(points[i]);
        // Four vertices per point: outer_left, inner_left,
        // inner_right, outer_right.
        let outer_l = egui::pos2(p.x + nx * (half + FEATHER), p.y + ny * (half + FEATHER));
        let inner_l = egui::pos2(p.x + nx * half,             p.y + ny * half);
        let inner_r = egui::pos2(p.x - nx * half,             p.y - ny * half);
        let outer_r = egui::pos2(p.x - nx * (half + FEATHER), p.y - ny * (half + FEATHER));
        mesh.colored_vertex(outer_l, trans);
        mesh.colored_vertex(inner_l, color);
        mesh.colored_vertex(inner_r, color);
        mesh.colored_vertex(outer_r, trans);
    }
    for i in 0..(n - 1) {
        let base = (i * 4)       as u32;
        let next = ((i + 1) * 4) as u32;
        // Left feather strip (outer_l ↔ inner_l).
        mesh.add_triangle(base + 0, base + 1, next + 0);
        mesh.add_triangle(base + 1, next + 1, next + 0);
        // Interior (inner_l ↔ inner_r).
        mesh.add_triangle(base + 1, base + 2, next + 1);
        mesh.add_triangle(base + 2, next + 2, next + 1);
        // Right feather strip (inner_r ↔ outer_r).
        mesh.add_triangle(base + 2, base + 3, next + 2);
        mesh.add_triangle(base + 3, next + 3, next + 2);
    }
    painter.add(egui::Shape::mesh(mesh));

    // Fan-corner discs — Krita's `paintFan` analogue. At each
    // interior polyline vertex whose tangent rotates by more than
    // `fan_corners_step` radians, stamp a filled disc the size of
    // the local stroke width. The disc fills the outer wedge that
    // the miter joint would otherwise leave behind and covers the
    // inner-side bowtie self-overlap. Cheaper than synthesising
    // angular triangle fans and visually identical to Krita's
    // own implementation. Only fires when the caller opted in.
    //
    // Same `FEATHER` shrink as the endpoint caps: keeps the disc's
    // AA halo inside the ribbon body so the user doesn't see a bead
    // at every painted corner.
    if FAN_CORNERS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        let step = f32::from_bits(
            FAN_CORNERS_STEP_BITS.load(std::sync::atomic::Ordering::Relaxed),
        );
        for i in 1..(n - 1) {
            let pa = points[i - 1];
            let pb = points[i];
            let pc = points[i + 1];
            let d1 = [pb[0] - pa[0], pb[1] - pa[1]];
            let d2 = [pc[0] - pb[0], pc[1] - pb[1]];
            let l1 = (d1[0] * d1[0] + d1[1] * d1[1]).sqrt().max(1.0e-6);
            let l2 = (d2[0] * d2[0] + d2[1] * d2[1]).sqrt().max(1.0e-6);
            let cos_t = (d1[0] * d2[0] + d1[1] * d2[1]) / (l1 * l2);
            let theta = cos_t.clamp(-1.0, 1.0).acos();
            if theta > step {
                let r = (widths[i] * 0.5 * zoom - FEATHER * 0.5).max(0.4);
                painter.circle_filled(to_screen(points[i]), r, color);
            }
        }
    }
}

/// Variant used for the live in-progress stroke (no cache, recomputed each
/// frame). We build a temporary points/widths Vec via Catmull-Rom and feed
/// it to `paint_polyline_grouped` — same anti-aliased look as committed
/// strokes, no cache.
fn paint_stroke_live(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    stroke: &Stroke,
    zoom: f32,
    position_passes: usize,
    layer_opacity: f32,
) {
    let color = col(stroke.color, layer_opacity);

    // Same geometry as the committed cache: the shared ribbon-spine builder
    // (centripetal Catmull-Rom + adaptive arc-length / turn-angle
    // decimation). Rebuilding it fresh each frame is cheap for a single
    // in-progress stroke and guarantees the live preview matches the
    // committed stroke exactly (WYSIWYG on pen-up).
    let position_passes = stroke.smoothing.cache_passes(position_passes);
    let cache =
        crate::canvas::stroke::build_spine(&stroke.samples, stroke.base_width, position_passes);
    let points = cache.points;
    let widths = cache.widths;

    let paint_color = match stroke.style {
        StrokeStyle::Default => color,
        StrokeStyle::Pencil  => pencil_color(stroke.color, layer_opacity),
        StrokeStyle::Marker  => marker_color(stroke.color, layer_opacity),
        StrokeStyle::Airbrush => airbrush_color(stroke.color, layer_opacity),
    };
    // Fill underlay for the live (in-progress) Fill-tool stroke. Grows
    // every frame as the user drags. lyon's sweep-line tessellator
    // handles self-intersection cleanly so the live preview never
    // flashes holes when the user's polyline briefly crosses itself.
    if stroke.filled {
        paint_fill_from_polyline(
            painter, to_screen, &points,
            fill_color(stroke.color, layer_opacity),
        );
    }
    // Marker live preview also uses constant-width ribbon — pressure
    // does not modulate width for highlighter-style strokes.
    let const_widths: Vec<f32>;
    let widths_for_paint: &[f32] = if matches!(stroke.style, StrokeStyle::Marker) {
        const_widths = vec![stroke.base_width; widths.len()];
        &const_widths
    } else {
        &widths
    };
    paint_polyline_grouped(
        painter, to_screen, &points, widths_for_paint,
        paint_color, zoom, stroke.style,
    );
}

/// Fill colour for the interior of a Fill-tool stroke. The auto-closing
/// edge is invisible regardless — only the user-drawn outline is
/// stroked — so we pass the stroke colour through with its own alpha
/// (typical RGBA = 0xFF → opaque fill), pre-scaled by the layer's
/// opacity.
fn fill_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    col(c, layer_opacity)
}

/// Triangulate the closed polygon `[points[0], ..., points[n-1], points[0]]`
/// and paint it as a filled mesh.
///
/// Why lyon? egui's built-in fill is a centroid-fan that only handles
/// convex polygons. Ear-clipping (earcutr, our previous tessellator)
/// handles non-convex polygons but its behaviour is **undefined on
/// self-intersecting input** — when the user's Fill stroke crosses
/// itself (a loop-inside-a-loop, a figure-8, an accidental cross-over
/// while tracing) ear-clipping skips regions and leaves visible holes
/// inside the fill. Lyon's `FillTessellator` uses a sweep-line
/// algorithm with proper edge-crossing detection and honours the
/// **non-zero winding rule**, so self-intersection regions fill
/// correctly — no holes.
fn paint_fill_from_polyline(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    points: &[[f32; 2]],
    color: egui::Color32,
) {
    use lyon_path::Path;
    use lyon_path::math::point;
    use lyon_tessellation::{
        BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex,
        VertexBuffers,
    };

    let n = points.len();
    if n < 3 { return; }

    // Build a closed lyon path in screen space so the resulting
    // vertices drop straight into an egui Mesh with no further
    // transform.
    let mut pb = Path::builder();
    let p0 = to_screen(points[0]);
    pb.begin(point(p0.x, p0.y));
    for p in points.iter().skip(1) {
        let s = to_screen(*p);
        pb.line_to(point(s.x, s.y));
    }
    pb.end(true); // close: lyon auto-joins last vertex → first
    let path = pb.build();

    // Output buffers: per-vertex screen position + triangle index list.
    let mut buffers: VertexBuffers<egui::Pos2, u32> = VertexBuffers::new();
    let mut tess = FillTessellator::new();
    // Non-zero winding is what most paint apps default to and matches
    // user intuition: an inner loop drawn in the same direction fills
    // as one connected blob; only opposite-winding inner loops cut
    // holes (rare in freehand pen input).
    let options = FillOptions::default()
        .with_fill_rule(FillRule::NonZero);
    let result = tess.tessellate_path(
        &path,
        &options,
        &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| {
            let p = v.position();
            egui::pos2(p.x, p.y)
        }),
    );
    if result.is_err() || buffers.indices.is_empty() {
        return;
    }

    let mut mesh = egui::Mesh::default();
    mesh.vertices.reserve(buffers.vertices.len());
    mesh.indices.reserve(buffers.indices.len());
    for p in buffers.vertices {
        mesh.colored_vertex(p, color);
    }
    for tri in buffers.indices.chunks_exact(3) {
        mesh.add_triangle(tri[0], tri[1], tri[2]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// RGBA bytes → translucent egui colour for pencil rendering.
///
/// Lower alpha than for the ribbon mesh because the stamp path covers
/// each pixel multiple times (dense disc stamps with ~30 % spacing
/// overlap each pixel ~2-3 times). A per-stamp alpha around 0.30
/// composites to ≈0.55 on the painted line — same on-screen weight as
/// before, but without the bowtie self-overlap that the mesh produced.
fn pencil_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    // Per-stamp alpha is low because the stamp path covers each pixel
    // ~5× (dense spacing at ~20 % of stroke diameter). Composite
    // weight = 1 - (1-a)^5 ≈ 0.67 at a = 0.20 — pencil-pale, uniform.
    //
    // We *cannot* use `Color32::from_rgba_unmultiplied` here: that
    // helper premultiplies in linear (gamma-correct) space, which at
    // low alpha collapses src.rgb to a tiny value. Egui's shader then
    // blends in sRGB (gamma-encoded) space — the tiny src.rgb gets
    // overwhelmed by `dst.rgb * (1 - a)` and a mid-grey pencil over a
    // white desktop washes out to near-white.
    //
    // Premultiplying in display space (plain `src.rgb * a`) keeps the
    // colour contribution intact through the sRGB-space blend, so a
    // grey pencil reads as grey regardless of what is behind the
    // overlay.
    let af = (c[3] as f32 / 255.0) * 0.20 * layer_opacity.clamp(0.0, 1.0);
    let r = (c[0] as f32 * af) as u8;
    let g = (c[1] as f32 * af) as u8;
    let b = (c[2] as f32 * af) as u8;
    let a = (af * 255.0) as u8;
    egui::Color32::from_rgba_premultiplied(r, g, b, a)
}

/// Highlighter-style colour: roughly 45 % alpha so a single pass
/// reads as translucent ink and a second pass darkens visibly. We
/// stay in `from_rgba_unmultiplied` (not the gamma-trick path the
/// pencil uses) because at 45 % alpha there is no risk of the
/// premultiplied RGB collapsing to near-zero under sRGB blending.
fn marker_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    let a = ((c[3] as f32 / 255.0) * 0.45 * layer_opacity.clamp(0.0, 1.0) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

/// Airbrush per-stamp colour: very low alpha (~7 %) so that
/// overlapping stamps composite into a soft cloud. We need the
/// premultiplied-in-display-space treatment for the same reason the
/// pencil does: at this alpha, the gamma-correct premultiply path
/// would wash the colour out completely.
fn airbrush_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    let af = (c[3] as f32 / 255.0) * 0.07 * layer_opacity.clamp(0.0, 1.0);
    let r = (c[0] as f32 * af) as u8;
    let g = (c[1] as f32 * af) as u8;
    let b = (c[2] as f32 * af) as u8;
    let a = (af * 255.0) as u8;
    egui::Color32::from_rgba_premultiplied(r, g, b, a)
}

/// Airbrush renderer: dense disc-stamp walk along the polyline with
/// uniform per-stamp radius (no jitter — the soft cloud effect
/// comes from low per-stamp alpha + tight spacing, not radius
/// variation). Spacing is ~10 % of the average stamp diameter so a
/// hold-still pen accumulates many overlapping stamps in the same
/// place and the user sees the colour build toward solid.
fn paint_airbrush_stamps(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    points: &[[f32; 2]],
    widths: &[f32],
    color: egui::Color32,
    zoom: f32,
) {
    let n = points.len();
    if n == 0 { return; }

    // First stamp anchors the head of the stroke at the pen-down
    // position so the user always sees ink land where they pressed.
    let r0 = widths[0] * 0.5 * zoom;
    if r0 >= 0.4 {
        painter.circle_filled(to_screen(points[0]), r0, color);
    }
    if n == 1 { return; }

    let mut residual = 0.0_f32;
    for i in 0..(n - 1) {
        let p0 = points[i];
        let p1 = points[i + 1];
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1.0e-4 { continue; }
        let avg_w = 0.5 * (widths[i] + widths[i + 1]);
        // 10 % of diameter → ~10 stamps stack on each pixel. With
        // 7 % per-stamp alpha that composites to ≈ 1 - 0.93^10 ≈
        // 52 % cover on a single pass; doubling back over the same
        // path takes it toward solid.
        let spacing = (avg_w * 0.10).max(0.5);
        let mut d = spacing - residual;
        while d < seg_len {
            let t   = d / seg_len;
            let pos = [p0[0] + dx * t, p0[1] + dy * t];
            let w   = widths[i] + (widths[i + 1] - widths[i]) * t;
            let r   = w * 0.5 * zoom;
            if r >= 0.4 {
                painter.circle_filled(to_screen(pos), r, color);
            }
            d += spacing;
        }
        residual = (seg_len - (d - spacing)).max(0.0);
    }
}

/// Pencil renderer: walk the polyline at constant arc-length intervals
/// and stamp a filled disc at each step. Disc radius is per-stamp
/// jittered for graphite-like edge irregularity.
fn paint_pencil_stamps(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    points: &[[f32; 2]],
    widths: &[f32],
    color: egui::Color32,
    zoom: f32,
) {
    let n = points.len();
    if n == 0 { return; }
    let mut stamp_id: u32 = 0;

    // First stamp at the start point so the head of the stroke is
    // anchored exactly where the pen landed.
    let mut last_pos = points[0];
    {
        let j = 0.78 + 0.32 * hash01(stamp_id);
        let r = widths[0] * 0.5 * j * zoom;
        if r >= 0.4 {
            painter.circle_filled(to_screen(points[0]), r, color);
        }
        stamp_id += 1;
    }

    // Carry-over of leftover arc length from the previous segment so
    // stamp spacing is continuous across joins (no doubled stamp at
    // shared vertices).
    //
    // **Slow-pen wobble guard.** Arc-length stepping alone is not
    // enough: at slow pen speeds the smoothed polyline meanders inside
    // a sub-pixel region, doubling back on itself in tiny zig-zags.
    // Arc-length stepping faithfully traces every micro-wobble and
    // ends up stamping discs on top of each other → a splotchy,
    // ink-bleed look instead of a continuous pencil line.
    //
    // The cure is a second gate: candidate stamps must also sit at
    // least 85 % of the nominal spacing away from the **previously
    // placed stamp** in Euclidean (straight-line) distance. A backtrack
    // along the polyline accumulates arc-length but not Euclidean
    // distance from the last stamp, so the rejection naturally fires
    // on wobble and never on a clean forward-moving line.
    let mut residual = 0.0_f32;
    for i in 0..(n - 1) {
        let p0 = points[i];
        let p1 = points[i + 1];
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1.0e-4 { continue; }
        let w0 = widths[i];
        let w1 = widths[i + 1];
        let avg_w = 0.5 * (w0 + w1);
        // Spacing ≈ 20 % of the diameter (≈ 40 % of the radius) → each
        // pixel is hit by ~5 stamps, giving a continuous line with
        // visible per-stamp jitter at the edges. Floor at 0.5 px so a
        // hairline stroke still emits enough stamps to overlap.
        let spacing = (avg_w * 0.20).max(0.5);
        // 85 % of nominal spacing is the rejection threshold. Lower
        // than the spacing itself so jitter between consecutive stamps
        // does not accidentally trip the guard on a perfectly straight
        // segment.
        let min_euc = spacing * 0.85;
        let mut d = spacing - residual;
        while d < seg_len {
            let t   = d / seg_len;
            let pos = [p0[0] + dx * t, p0[1] + dy * t];

            // Reject candidate if too close to the last stamp.
            let edx = pos[0] - last_pos[0];
            let edy = pos[1] - last_pos[1];
            let euc = (edx * edx + edy * edy).sqrt();
            if euc >= min_euc {
                let w   = w0 + (w1 - w0) * t;
                let j   = 0.78 + 0.32 * hash01(stamp_id);
                let r   = w * 0.5 * j * zoom;
                stamp_id += 1;
                if r >= 0.4 {
                    painter.circle_filled(to_screen(pos), r, color);
                }
                last_pos = pos;
            }
            d += spacing;
        }
        residual = seg_len - (d - spacing);
    }
}

/// Deterministic [0, 1) hash from a u32 index. Used by the pencil
/// renderer to jitter the per-point width — same index always returns
/// the same value, so a stroke does not shimmer between frames.
#[inline]
fn hash01(i: u32) -> f32 {
    let mut x = i.wrapping_mul(2654435761);
    x ^= x >> 16;
    x = x.wrapping_mul(2246822519);
    x ^= x >> 13;
    x = x.wrapping_mul(3266489917);
    x ^= x >> 16;
    (x & 0xFFFF) as f32 / 65535.0
}

// Centripetal Catmull-Rom shared with the committed-stroke path —
// see `crate::canvas::stroke::catmull_rom`.

// =============================================================================
// 2) Tiny-skia path: kept ONLY for the screenshot PNG pipeline.
// =============================================================================

/// Render a single layer into a small RGBA buffer for use as a
/// preview thumbnail.
///
/// Returns straight (un-premultiplied) RGBA so the caller can upload
/// it as an `egui::ColorImage` without further conversion. Returns
/// `None` when the layer has nothing renderable — the caller can
/// then skip the texture upload + show an "empty layer" placeholder
/// instead.
///
/// Bounds are inferred from the union of every stroke / shape / image
/// rect on the layer. The thumbnail is rendered at the largest scale
/// that fits the requested `out_w × out_h` while preserving aspect.
pub fn render_layer_thumbnail(
    layer: &crate::canvas::canvas::Layer,
    out_w: u32,
    out_h: u32,
) -> Option<image::RgbaImage> {
    // 1) Canvas-local bounds of every renderable on the layer.
    let [min_x, min_y, max_x, max_y] = layer.content_bounds()?;
    let bw = (max_x - min_x).max(1.0);
    let bh = (max_y - min_y).max(1.0);

    // 2) Scale to fit out_w × out_h preserving aspect, centred.
    let sx = out_w as f32 / bw;
    let sy = out_h as f32 / bh;
    let scale = sx.min(sy).max(1.0e-3);
    let used_w = bw * scale;
    let used_h = bh * scale;
    let ox = (out_w as f32 - used_w) * 0.5 - min_x * scale;
    let oy = (out_h as f32 - used_h) * 0.5 - min_y * scale;

    let mut pixmap = tiny_skia::Pixmap::new(out_w, out_h)?;
    pixmap.fill(Color::from_rgba8(20, 20, 24, 220));
    let t = Transform::from_translate(ox, oy).post_scale(scale, scale);

    // 3) Reuse the regular draw paths. They are agnostic of the
    // outer canvas's pan/zoom — they operate in whatever transform
    // we hand them.
    if let Some(img) = layer.image.as_ref() {
        if let Ok(decoded) = image::load_from_memory(&img.png) {
            let rgba = decoded.to_rgba8();
            let (iw, ih) = (rgba.width(), rgba.height());
            if let Some(mut src) = tiny_skia::Pixmap::new(iw, ih) {
                for (px, dst) in rgba.pixels().zip(src.pixels_mut()) {
                    let [r, g, b, a] = px.0;
                    let af = a as f32 / 255.0;
                    *dst = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r as f32 * af) as u8,
                        (g as f32 * af) as u8,
                        (b as f32 * af) as u8,
                        a,
                    ).unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0,0,0,0).unwrap());
                }
                let (w, h) = if img.logical_size[0] > 0.0 && img.logical_size[1] > 0.0 {
                    (img.logical_size[0], img.logical_size[1])
                } else {
                    (iw as f32, ih as f32)
                };
                let sx = w / iw as f32;
                let sy = h / ih as f32;
                let draw_t = Transform::from_scale(sx, sy)
                    .post_translate(img.canvas_origin[0], img.canvas_origin[1])
                    .post_concat(t);
                let opts = tiny_skia::PixmapPaint {
                    opacity: 1.0,
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    quality: tiny_skia::FilterQuality::Bilinear,
                };
                pixmap.as_mut().draw_pixmap(0, 0, src.as_ref(), &opts, draw_t, None);
            }
        }
    }
    let mut view = pixmap.as_mut();
    for shape in &layer.shapes {
        draw_shape(&mut view, shape, t);
    }
    for stroke in &layer.strokes {
        draw_stroke(&mut view, stroke, t);
    }

    // 4) tiny-skia outputs premultiplied; unpremultiply to straight
    //    so the egui ColorImage upload doesn't double-darken.
    let raw = pixmap.data().to_vec();
    let mut img = image::RgbaImage::from_raw(out_w, out_h, raw)?;
    for px in img.pixels_mut() {
        let a = px.0[3];
        if a == 0 || a == 255 { continue; }
        let af = a as f32 / 255.0;
        for c in 0..3 {
            px.0[c] = (px.0[c] as f32 / af).min(255.0) as u8;
        }
    }
    Some(img)
}

/// Rasterise a canvas into the given mutable pixmap.
///
/// `pixels_per_point` scales canvas-local logical coordinates into the
/// pixmap's physical pixel coordinates. The pixmap dimensions usually
/// match the monitor's physical resolution (xcap returns physical px),
/// while the canvas itself stores positions in egui logical pixels — at
/// 125/150/200 % DPI the two diverge, so without this scale strokes
/// land in the wrong region of the saved PNG. Pass `1.0` from any
/// caller that operates entirely in logical space.
pub fn render_canvas(
    pixmap: &mut PixmapMut<'_>,
    canvas: &Canvas,
    preview_shape: Option<&Shape>,
    preview_stroke: Option<&Stroke>,
    pixels_per_point: f32,
) {
    // Clear to fully transparent — we composite onto the screen, and any
    // unpainted pixel must let the pixels behind show through.
    pixmap.fill(Color::TRANSPARENT);

    // canvas-local → logical screen px (pan + zoom) → physical pixel (× ppp).
    // `post_scale` applies *after* the prior transform, which is the
    // outer DPI step we want. tiny-skia's stroker tessellates strokes
    // into a filled path in source-space BEFORE the transform applies,
    // so stroke widths scale with this transform automatically — no
    // separate width-scale plumbing needed.
    let ppp = pixels_per_point.max(0.01);
    let t = Transform::from_translate(canvas.pan[0], canvas.pan[1])
        .post_scale(canvas.zoom, canvas.zoom)
        .post_scale(ppp, ppp);

    for layer in &canvas.layers {
        if !layer.visible { continue; }
        // Raster background: decode PNG to a Pixmap and blit it into
        // the same transform stack the strokes use, so a screenshot
        // preserves the frozen frame underneath the user's ink.
        // Decode is O(image) — only happens on save, not per frame.
        if let Some(img) = layer.image.as_ref() {
            if let Ok(decoded) = image::load_from_memory(&img.png) {
                let rgba = decoded.to_rgba8();
                let (iw, ih) = (rgba.width(), rgba.height());
                if let Some(mut src) = tiny_skia::Pixmap::new(iw, ih) {
                    // tiny-skia expects premultiplied RGBA.
                    for (px, dst) in rgba.pixels().zip(src.pixels_mut()) {
                        let [r, g, b, a] = px.0;
                        let af = a as f32 / 255.0;
                        *dst = tiny_skia::PremultipliedColorU8::from_rgba(
                            (r as f32 * af) as u8,
                            (g as f32 * af) as u8,
                            (b as f32 * af) as u8,
                            a,
                        ).unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0,0,0,0).unwrap());
                    }
                    // Source image is in physical pixels; the active
                    // transform `t` operates in canvas-local coords
                    // and post-scales by ppp into physical pixels.
                    // Logical-size width gives the canvas-local rect
                    // the renderer should hit, so divide image
                    // dimensions by `(iw / logical_w)` via a scale
                    // baked into the draw transform.
                    let lw = if img.logical_size[0] > 0.0 { img.logical_size[0] } else { iw as f32 };
                    let lh = if img.logical_size[1] > 0.0 { img.logical_size[1] } else { ih as f32 };
                    let sx = lw / iw as f32;
                    let sy = lh / ih as f32;
                    // `canvas_origin` is the image's canvas-local anchor —
                    // set at capture time and shifted further once the user
                    // pans this layer. Apply it after the physical→logical
                    // scale but before `t`, so it is interpreted in
                    // canvas-local pixels like every other coordinate the
                    // transform stack consumes, and screenshots line up the
                    // same way the live overlay does.
                    let draw_t = Transform::from_scale(sx, sy)
                        .post_translate(img.canvas_origin[0], img.canvas_origin[1])
                        .post_concat(t);
                    let opts = tiny_skia::PixmapPaint {
                        opacity: layer.opacity.clamp(0.0, 1.0),
                        blend_mode: tiny_skia::BlendMode::SourceOver,
                        quality: tiny_skia::FilterQuality::Bilinear,
                    };
                    pixmap.draw_pixmap(0, 0, src.as_ref(), &opts, draw_t, None);
                }
            }
        }
        for shape in &layer.shapes {
            draw_shape(pixmap, shape, t);
        }
        for stroke in &layer.strokes {
            draw_stroke(pixmap, stroke, t);
        }
    }
    if let Some(s) = preview_shape {
        draw_shape(pixmap, s, t);
    }
    if let Some(s) = preview_stroke {
        draw_stroke(pixmap, s, t);
    }
}

fn rgba_paint(color: [u8; 4]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(color[0], color[1], color[2], color[3]);
    // Anti-alias is on by default; spell it out for clarity.
    p.anti_alias = true;
    p
}

fn draw_shape(pixmap: &mut PixmapMut<'_>, shape: &Shape, t: Transform) {
    match shape {
        Shape::Rect { a, b, color, stroke_width, border } => {
            let mut pb = PathBuilder::new();
            // Normalise corners so width/height are positive.
            let x0 = a[0].min(b[0]);
            let y0 = a[1].min(b[1]);
            let x1 = a[0].max(b[0]);
            let y1 = a[1].max(b[1]);
            pb.move_to(x0, y0);
            pb.line_to(x1, y0);
            pb.line_to(x1, y1);
            pb.line_to(x0, y1);
            pb.close();
            if let Some(path) = pb.finish() {
                let paint = rgba_paint(*color);
                let mut sk = TsStroke::default();
                sk.width = *stroke_width;
                if let BorderStyle::DashedAnimated = border {
                    // Same on/off pattern + phase the egui side uses,
                    // so a screenshot taken at frame N matches the
                    // live overlay at frame N.
                    if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                        sk.dash = Some(d);
                    }
                }
                pixmap.stroke_path(&path, &paint, &sk, t, None);
            }
        }
        Shape::Ellipse { a, b, color, stroke_width, border } => {
            let cx = (a[0] + b[0]) * 0.5;
            let cy = (a[1] + b[1]) * 0.5;
            let rx = ((b[0] - a[0]) * 0.5).abs().max(1.0);
            let ry = ((b[1] - a[1]) * 0.5).abs().max(1.0);
            // tiny_skia's PathBuilder has `push_oval` from a Rect.
            if let Some(rect) = tiny_skia::Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0) {
                let mut pb = PathBuilder::new();
                pb.push_oval(rect);
                if let Some(path) = pb.finish() {
                    let paint = rgba_paint(*color);
                    let mut sk = TsStroke::default();
                    sk.width = *stroke_width;
                    if let BorderStyle::DashedAnimated = border {
                        if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                            sk.dash = Some(d);
                        }
                    }
                    pixmap.stroke_path(&path, &paint, &sk, t, None);
                }
            }
        }
        Shape::Line { a, b, color, stroke_width, border } => {
            match border {
                BorderStyle::Solid => {
                    draw_line(pixmap, *a, *b, *color, *stroke_width, t);
                }
                BorderStyle::DashedAnimated => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(a[0], a[1]);
                    pb.line_to(b[0], b[1]);
                    if let Some(path) = pb.finish() {
                        let paint = rgba_paint(*color);
                        let mut sk = TsStroke::default();
                        sk.width = *stroke_width;
                        if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                            sk.dash = Some(d);
                        }
                        pixmap.stroke_path(&path, &paint, &sk, t, None);
                    }
                }
            }
        }
        Shape::Arrow { a, b, color, stroke_width, border } => {
            // 1) The shaft (dashed or solid).
            match border {
                BorderStyle::Solid => {
                    draw_line(pixmap, *a, *b, *color, *stroke_width, t);
                }
                BorderStyle::DashedAnimated => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(a[0], a[1]);
                    pb.line_to(b[0], b[1]);
                    if let Some(path) = pb.finish() {
                        let paint = rgba_paint(*color);
                        let mut sk = TsStroke::default();
                        sk.width = *stroke_width;
                        if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                            sk.dash = Some(d);
                        }
                        pixmap.stroke_path(&path, &paint, &sk, t, None);
                    }
                }
            }
            // 2) Two arrowhead lines, each rotated ±25° from the shaft direction.
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let ux = dx / len;
            let uy = dy / len;
            let head_len = (stroke_width * 4.0).max(8.0);
            // Rotate (ux,uy) by ±150° (i.e. 180° back, then ±30° splay).
            let theta: f32 = 25.0_f32.to_radians();
            let (s1, c1) = (theta.sin(), theta.cos());
            // head1: rotate by +(180-25)°
            let h1x = -(ux * c1 - uy * s1) * head_len;
            let h1y = -(ux * s1 + uy * c1) * head_len;
            // head2: rotate by -(180-25)°
            let h2x = -(ux * c1 + uy * s1) * head_len;
            let h2y = -(-ux * s1 + uy * c1) * head_len;
            draw_line(pixmap, *b, [b[0] + h1x, b[1] + h1y], *color, *stroke_width, t);
            draw_line(pixmap, *b, [b[0] + h2x, b[1] + h2y], *color, *stroke_width, t);
        }
        Shape::Raster { pos, size, png, .. } => {
            // Decode and blit the PNG inline. Slow per call but the
            // screenshot path only fires once per save, not per frame.
            if let Ok(decoded) = image::load_from_memory(png) {
                let rgba = decoded.to_rgba8();
                let (iw, ih) = (rgba.width(), rgba.height());
                if let Some(mut src) = tiny_skia::Pixmap::new(iw, ih) {
                    for (px, dst) in rgba.pixels().zip(src.pixels_mut()) {
                        let [r, g, b, a] = px.0;
                        let af = a as f32 / 255.0;
                        *dst = tiny_skia::PremultipliedColorU8::from_rgba(
                            (r as f32 * af) as u8,
                            (g as f32 * af) as u8,
                            (b as f32 * af) as u8,
                            a,
                        )
                        .unwrap_or_else(|| {
                            tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap()
                        });
                    }
                    let sx = size[0] / iw as f32;
                    let sy = size[1] / ih as f32;
                    let draw_t = Transform::from_scale(sx, sy)
                        .post_translate(pos[0], pos[1])
                        .post_concat(t);
                    let opts = tiny_skia::PixmapPaint {
                        opacity: 1.0,
                        blend_mode: tiny_skia::BlendMode::SourceOver,
                        quality: tiny_skia::FilterQuality::Bilinear,
                    };
                    pixmap.draw_pixmap(0, 0, src.as_ref(), &opts, draw_t, None);
                }
            }
        }
        Shape::Text { pos, content, font_size, color } => {
            // tiny-skia has no built-in font rasteriser, so the
            // screenshot pipeline can't show glyphs yet. We paint a
            // coloured underline at the top-left of each line and a
            // semi-transparent fill so the user sees *something*
            // where their text sits — a placeholder until we bundle
            // a font + fontdue. Once that lands, replace this arm
            // with a real glyph blit.
            let lines: Vec<&str> = content.split('\n').collect();
            let line_h = font_size * 1.25;
            let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
            let w = (font_size * 0.55 * max_chars).max(*font_size);
            let h = line_h * lines.len() as f32;
            let mut fill = rgba_paint(*color);
            // Light underlay so the placeholder reads as "text was
            // here" without pretending to be the glyphs themselves.
            fill.set_color_rgba8(color[0], color[1], color[2], color[3] / 3);
            if let Some(rect) = tiny_skia::Rect::from_xywh(pos[0], pos[1], w, h) {
                let mut pb = PathBuilder::new();
                pb.push_rect(rect);
                if let Some(path) = pb.finish() {
                    pixmap.fill_path(&path, &fill, FillRule::Winding, t, None);
                }
            }
        }
    }
}

fn draw_line(
    pixmap: &mut PixmapMut<'_>,
    a: [f32; 2],
    b: [f32; 2],
    color: [u8; 4],
    width: f32,
    t: Transform,
) {
    let mut pb = PathBuilder::new();
    pb.move_to(a[0], a[1]);
    pb.line_to(b[0], b[1]);
    if let Some(path) = pb.finish() {
        let paint = rgba_paint(color);
        let mut sk = TsStroke::default();
        sk.width = width;
        sk.line_cap = tiny_skia::LineCap::Round;
        sk.line_join = tiny_skia::LineJoin::Round;
        pixmap.stroke_path(&path, &paint, &sk, t, None);
    }
}

fn draw_stroke(pixmap: &mut PixmapMut<'_>, stroke: &Stroke, t: Transform) {
    if stroke.samples.is_empty() {
        return;
    }
    let paint = rgba_paint(stroke.color);

    // Prefer the cached subdivided polyline (built once at commit time and
    // re-used by both the live egui paint and the screenshot path). Without
    // this, the screenshot would walk the *raw* filtered samples and draw
    // straight segments between them — visible kinks where the live egui
    // render shows a smooth Catmull-Rom-subdivided curve.
    if let Some(cache) = &stroke.cache {
        if cache.points.is_empty() { return; }
        if cache.points.len() == 1 {
            let r = (cache.widths[0] * 0.5).max(0.4);
            stamp_circle(pixmap, cache.points[0], r, &paint, t);
            return;
        }
        // Stamp filled discs along the dense cache polyline. Cache points
        // are spaced ~40 % of the stroke's half-width apart, which is
        // already dense enough that disc overlap leaves no scallops. Each
        // disc's radius comes from the matching per-point width in the
        // cache, so pressure-tapered strokes taper correctly.
        let pts = &cache.points;
        let ws  = &cache.widths;
        let n   = pts.len();
        let r0 = (ws[0] * 0.5).max(0.4);
        stamp_circle(pixmap, pts[0], r0, &paint, t);
        for i in 1..n {
            let prev = pts[i - 1];
            let cur  = pts[i];
            let dx = cur[0] - prev[0];
            let dy = cur[1] - prev[1];
            let dist = (dx * dx + dy * dy).sqrt();
            // Sub-step only when the cache spacing is larger than half a
            // pixel (rare — cache is already dense). Capped to bound work
            // on degenerate input.
            let steps = (dist * 2.0).ceil() as usize;
            let steps = steps.clamp(1, 16);
            let w0 = ws[i - 1];
            let w1 = ws[i];
            for s in 1..=steps {
                let u = s as f32 / steps as f32;
                let pos = [prev[0] + dx * u, prev[1] + dy * u];
                let w   = w0 + (w1 - w0) * u;
                stamp_circle(pixmap, pos, (w * 0.5).max(0.4), &paint, t);
            }
        }
        return;
    }

    // Fallback path used only when the cache is missing (in-progress live
    // stroke caught mid-frame, or some future caller that hasn't built
    // the cache yet). Walks raw filtered samples with linear interp
    // between them. The screenshot pipeline always sees committed strokes
    // (which carry a cache), so this is exercised rarely.
    let mut prev = stroke.samples[0];
    stamp_circle(pixmap, prev.pos, half_width(prev.pressure, stroke.base_width), &paint, t);
    for &cur in stroke.samples.iter().skip(1) {
        let dx = cur.pos[0] - prev.pos[0];
        let dy = cur.pos[1] - prev.pos[1];
        let dist = (dx * dx + dy * dy).sqrt();
        let steps = ((dist * 2.0).ceil() as usize).clamp(1, 256);
        for i in 1..=steps {
            let t_ = i as f32 / steps as f32;
            let pos  = [prev.pos[0] + dx * t_, prev.pos[1] + dy * t_];
            let pres = prev.pressure + (cur.pressure - prev.pressure) * t_;
            stamp_circle(pixmap, pos, half_width(pres, stroke.base_width), &paint, t);
        }
        prev = cur;
    }
}

#[inline]
fn half_width(pressure: f32, base: f32) -> f32 {
    // Clamp to [0.4, base/2]: even at "zero pressure" we still want a hair-thin
    // mark so the user sees their input. The 0.4 floor avoids invisible dots.
    let p = pressure.clamp(0.0, 1.0);
    (base * 0.5 * p).max(0.4)
}

fn stamp_circle(
    pixmap: &mut PixmapMut<'_>,
    center: [f32; 2],
    radius: f32,
    paint: &Paint<'_>,
    t: Transform,
) {
    if let Some(rect) = tiny_skia::Rect::from_xywh(
        center[0] - radius,
        center[1] - radius,
        radius * 2.0,
        radius * 2.0,
    ) {
        let mut pb = PathBuilder::new();
        pb.push_oval(rect);
        if let Some(path) = pb.finish() {
            pixmap.fill_path(&path, paint, FillRule::Winding, t, None);
        }
    }
}
