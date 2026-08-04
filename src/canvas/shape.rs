//! Geometric shapes the user can place on the canvas.
//!
//! Unlike `Stroke` (a list of arbitrary samples), a `Shape` is a *closed*
//! geometric primitive whose appearance is computed from a few control
//! points. This makes shapes cheap to store and easy to manipulate.

use serde::{Deserialize, Serialize};

/// How the outline of a Rect or Ellipse is rendered.
///
/// `DashedAnimated` produces a marching-ants stroke: the dash phase
/// advances every frame from a wall-clock reading, which makes the
/// dashes appear to crawl along the outline. Useful for highlighting
/// "this is the area I'm talking about" during live demos. The
/// pattern is stateless (no per-shape phase stored), so the same
/// outline picked up from disk picks up animation the moment it is
/// rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BorderStyle {
    /// Regular continuous outline (existing look).
    #[default]
    Solid,
    /// Marching-ants animated dashes.
    DashedAnimated,
}

/// All shape variants live in one enum so a Canvas can store them in a single
/// `Vec<Shape>` without trait objects (which complicate serde).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Shape {
    /// Axis-aligned rectangle defined by two opposite corners.
    Rect {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        /// Outline style. `#[serde(default)]` keeps RON files written
        /// before this field existed loading as plain `Solid`.
        #[serde(default)]
        border: BorderStyle,
    },
    /// Ellipse defined by its bounding rectangle (corners `a` and `b`).
    Ellipse {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        /// Outline style. `#[serde(default)]` keeps old files loading
        /// as solid-outlined ellipses.
        #[serde(default)]
        border: BorderStyle,
    },
    /// Straight line from `a` to `b`.
    Line {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        /// Outline style. `#[serde(default)]` keeps RON files written
        /// before this field existed loading as plain `Solid`. The
        /// only producer today is the Line shape tool; ruler-mode
        /// snap still commits a `Stroke` so its line is unaffected.
        #[serde(default)]
        border: BorderStyle,
    },
    /// Arrow from `a` to `b`. Arrowhead is drawn at `b`. Only the
    /// shaft honours `border`; the head stays solid so the
    /// direction reads cleanly even when the shaft is dashed.
    Arrow {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        #[serde(default)]
        border: BorderStyle,
    },
    /// Free-floating text label anchored at `pos` (top-left of the
    /// first line's baseline box). Lines are split on `\n`. Rendered
    /// in egui's default proportional font at `font_size` (canvas-local
    /// logical pixels). Screenshots of canvases containing Text shapes
    /// do not yet render the text glyphs (tiny-skia has no font
    /// rasteriser); the on-screen render is the source of truth until
    /// we bundle a font + fontdue.
    Text {
        pos: [f32; 2],
        content: String,
        font_size: f32,
        color: [u8; 4],
    },
    /// Raster sticker — a PNG-backed image dropped at `pos` with
    /// canvas-local `size`. Produced by the flood-fill tool, which
    /// rasterises the click region and stores it as a sticker on
    /// the active layer.
    Raster {
        /// Top-left canvas-local position.
        pos: [f32; 2],
        /// Logical canvas-local width / height.
        size: [f32; 2],
        /// PNG-encoded bytes. Transparent outside the filled region.
        png: Vec<u8>,
        /// Physical-pixel dimensions of the PNG image.
        px_size: [u32; 2],
        /// Cached colour for tinting helpers / hit-testing. The
        /// PNG itself already carries pixel colour; this is only
        /// the "primary" fill colour so the eraser / hit-test can
        /// reason about it without decoding.
        color: [u8; 4],
    },
}

impl Shape {
    /// Color accessor — useful for "highlight selected shape" rendering.
    #[allow(dead_code)]
    pub fn color(&self) -> [u8; 4] {
        match self {
            Shape::Rect    { color, .. } => *color,
            Shape::Ellipse { color, .. } => *color,
            Shape::Line    { color, .. } => *color,
            Shape::Arrow   { color, .. } => *color,
            Shape::Text    { color, .. } => *color,
            Shape::Raster  { color, .. } => *color,
        }
    }

    /// Shift this shape by `d` canvas-local pixels. Control points move;
    /// sizes, colours and stroke widths do not.
    pub fn translate(&mut self, d: [f32; 2]) {
        let shift = |p: &mut [f32; 2]| {
            p[0] += d[0];
            p[1] += d[1];
        };
        match self {
            Shape::Rect    { a, b, .. }
            | Shape::Ellipse { a, b, .. }
            | Shape::Line    { a, b, .. }
            | Shape::Arrow   { a, b, .. } => {
                shift(a);
                shift(b);
            }
            Shape::Text   { pos, .. } => shift(pos),
            Shape::Raster { pos, .. } => shift(pos),
        }
    }
}
