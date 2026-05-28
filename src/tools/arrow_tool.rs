//! Arrow-tool helpers.
//!
//! Two arrow modes:
//!   * `ToolKind::Arrow`         — straight arrow from drag-start to drag-end.
//!   * `ToolKind::FreehandArrow` — freehand stroke + an arrowhead shape at
//!     the end. The arrowhead lives as a separate `Shape::Arrow` so the
//!     tapered ink from the freehand stroke and the crisp arrowhead from
//!     the shape both render correctly.

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{BorderStyle, Shape};

/// Build an arrowhead `Shape::Arrow` whose direction matches the average
/// motion across the *last few* samples of the canvas's most recently
/// added stroke.
///
/// We average the direction over up to `LOOKBACK` trailing samples instead
/// of using just the last two. With only two samples the arrow's heading
/// jumps wildly when the user wiggles their hand on lift-off; averaging
/// produces a stable direction matching the user's overall motion.
pub fn arrowhead_for_freehand_end(
    canvas: &Canvas,
    color: [u8; 4],
    stroke_width: f32,
) -> Option<Shape> {
    const LOOKBACK: usize = 8;

    // The freehand stroke was committed to the active layer one tick
    // ago — fetch it from there for the arrowhead direction.
    let stroke = canvas.layers.get(canvas.active_layer)?.strokes.last()?;
    let n = stroke.samples.len();
    if n < 2 {
        return None;
    }
    // Use up to LOOKBACK trailing samples (or all of them, if shorter).
    let start = n.saturating_sub(LOOKBACK);
    let trailing = &stroke.samples[start..];
    // The arrowhead's tip is the very last sample.
    let tip = stroke.samples[n - 1].pos;
    // Anchor for direction = first sample in the trailing window.
    let anchor = trailing[0].pos;
    // If anchor and tip happen to coincide (rare — user lifted in place),
    // fall back to the previous sample.
    let dx = tip[0] - anchor[0];
    let dy = tip[1] - anchor[1];
    let len_sq = dx * dx + dy * dy;
    let (a, b) = if len_sq < 1.0 {
        (stroke.samples[n - 2].pos, tip)
    } else {
        (anchor, tip)
    };
    Some(Shape::Arrow { a, b, color, stroke_width, border: BorderStyle::Solid })
}
