//! Two render paths for canvas content:
//!
//! 1. **`paint_canvas_egui`** — paints directly via `egui::Painter`. egui
//!    pushes the geometry as GPU triangles through `wgpu`. There is **no
//!    CPU rasterisation and no per-frame texture upload**, which is what
//!    keeps the live drawing surface low-latency. This is the path the
//!    overlay uses every frame. Committed strokes skip egui when they
//!    can: `canvas::ink_gpu` keeps them in GPU memory and draws them
//!    with the same look, so a frame's cost no longer grows with the
//!    stroke count. Everything else (the live stroke, laser trails,
//!    shapes) is built here each frame, and ink that falls back to this
//!    path is culled to the view, simplified when zoomed out and capped
//!    by `MAX_INK_VERTICES`.
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

use crate::canvas::affine::Affine;
use crate::canvas::canvas::Canvas;
use crate::canvas::hit::{ObjRef, Selection};
use crate::canvas::ink_gpu;
use crate::canvas::pins::PinMember;
use crate::canvas::shape::{BorderStyle, Shape};
use crate::canvas::stroke::{Stroke, StrokeStyle};
use std::sync::atomic::{AtomicBool, AtomicU32};

use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, PixmapMut, Stroke as TsStroke, StrokeDash, Transform,
};

// Each part in a file of its own; every path into `render` still works.
mod ink;
mod paint;
mod raster;
mod shapes;

pub(crate) use ink::*;
pub use paint::*;
pub use raster::*;
pub use shapes::*;

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

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use crate::canvas::shape::RasterKind;
    use std::sync::Arc;

    /// The screenshot path turns shapes with tiny-skia; the overlay and
    /// the hit-tests turn them with `Obb`. If the two disagreed on which
    /// way is clockwise, a saved screenshot would show every rotated
    /// object mirrored. A two-pixel image (red | blue) turned a quarter
    /// turn must come out red above blue in both conventions.
    #[test]
    fn screenshot_path_turns_the_same_way_as_the_overlay() {
        let mut src = image::RgbaImage::new(2, 1);
        src.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        src.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
        let mut png = Vec::new();
        src.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();

        let shape = Shape::Raster {
            pos: [30.0, 40.0], size: [40.0, 20.0], png: Arc::new(png), px_size: [2, 1],
            color: [0; 4], rotation: std::f32::consts::FRAC_PI_2, kind: RasterKind::Image, media: None,
            picture: None,
            pin: 0,
            flip: false,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
        };
        // Where `Obb` says the red (left) and blue (right) halves went.
        let o = shape.obb();
        let red_at = o.to_canvas([-10.0, 0.0]);
        let blue_at = o.to_canvas([10.0, 0.0]);
        assert!(red_at[1] < blue_at[1], "a clockwise quarter turn puts the left half on top");

        let mut pixmap = tiny_skia::Pixmap::new(100, 100).unwrap();
        draw_shape(&mut pixmap.as_mut(), &shape, Transform::identity(), 1.0, None);
        let at = |p: [f32; 2]| pixmap.pixel(p[0] as u32, p[1] as u32).unwrap();
        let (r, b) = (at(red_at), at(blue_at));
        assert!(r.red() > 200 && r.blue() < 60, "expected red at {red_at:?}, got {r:?}");
        assert!(b.blue() > 200 && b.red() < 60, "expected blue at {blue_at:?}, got {b:?}");
        // Nothing left behind where the unrotated image used to reach.
        assert_eq!(at([33.0, 50.0]).alpha(), 0);
    }
}

#[cfg(test)]
mod ink_tests {
    use super::*;
    use crate::canvas::stroke::PenSample;

    fn stroke(style: StrokeStyle, width: f32) -> Stroke {
        let mut s = Stroke::with_style([90, 200, 255, 255], width, style);
        for i in 0..60 {
            let t = i as f32;
            let pressure = 0.2 + 0.8 * (t * 0.1).sin().abs();
            s.samples.push(PenSample { pos: [t * 4.0, (t * 0.2).sin() * 30.0], pressure, tilt: [0.0; 2] });
        }
        s.build_cache();
        s
    }

    /// Vertices egui ends up with for `s` painted at `zoom`.
    fn tessellated_vertices(s: &Stroke, zoom: f32) -> usize {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let to_screen = |p: [f32; 2]| egui::pos2(100.0 + p[0] * zoom, 100.0 + p[1] * zoom);
            paint_stroke(&painter, &to_screen, s, zoom, 3, 1.0);
        });
        ctx.tessellate(out.shapes, out.pixels_per_point)
            .iter()
            .map(|p| match &p.primitive {
                egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                _ => 0,
            })
            .sum()
    }

    /// The egui path's budget is what keeps a frame's buffers under
    /// wgpu's limit, so its estimate must never fall short.
    #[test]
    fn vertex_estimate_bounds_what_egui_builds() {
        for style in [StrokeStyle::Default, StrokeStyle::Pencil, StrokeStyle::Marker, StrokeStyle::Airbrush] {
            for width in [1.0, 4.0, 12.0, 40.0] {
                for zoom in [0.05, 0.3, 1.0, 4.0] {
                    let s = stroke(style, width);
                    let estimate = stroke_vertex_cost(&s, zoom);
                    let built = tessellated_vertices(&s, zoom);
                    assert!(built <= estimate, "{style:?} width {width} zoom {zoom}: built {built} > estimate {estimate}");
                }
            }
        }
    }

    #[test]
    fn decimate_keeps_ends_and_spacing() {
        let points: Vec<[f32; 2]> = (0..=100).map(|i| [i as f32 * 0.3, 0.0]).collect();
        let widths: Vec<f32> = (0..=100).map(|i| i as f32).collect();
        let (p, w) = decimate(&points, &widths, 2.0);
        assert_eq!(p[0], points[0]);
        assert_eq!(*p.last().unwrap(), points[100]);
        assert_eq!(*w.last().unwrap(), 100.0);
        assert_eq!(p.len(), w.len());
        for pair in p.windows(2) {
            assert!(pair[1][0] - pair[0][0] >= 2.0 - 1e-4, "{pair:?}");
        }
        // Nothing to merge at a gap below the node spacing.
        assert_eq!(decimate(&points, &widths, 0.1).0.len(), points.len());
    }

    #[test]
    fn disc_rules() {
        assert_eq!(DiscRule::Cap.radius(2.0, 1.0), Some(1.5));
        assert_eq!(DiscRule::Cap.radius(0.1, 1.0), Some(0.4));
        assert_eq!(DiscRule::Stamp.radius(0.3, 1.0), None);
        assert_eq!(DiscRule::Stamp.radius(0.3, 2.0), Some(0.6));
        assert_eq!(DiscRule::Plain.radius(0.3, 1.0), Some(0.3));
    }
}
