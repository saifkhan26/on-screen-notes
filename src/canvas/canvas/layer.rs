//! A layer of a slide (`Layer`), the frozen frame it may hold (`LayerImage`),
//! and the boxes objects take up (`shape_bounds`, turned or not).

use super::*;

/// Raster image payload of a frozen-frame layer. PNG bytes are
/// stored inline so the save file is self-contained — there are no
/// sidecar files to lose, and saving a canvas is atomic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerImage {
    /// PNG-encoded bytes of the captured frame.
    pub png: Vec<u8>,
    /// Width and height of the decoded image in physical pixels.
    /// Cached so the renderer doesn't need to peek the PNG header
    /// for every hit-test or bounds query.
    pub size: [u32; 2],
    /// Width and height of the image in egui *logical* pixels at
    /// the time of capture (physical px ÷ `pixels_per_point`). Used
    /// as the canvas-local rect to paint into so the frozen frame
    /// covers the same logical-screen region the user saw when
    /// they pressed Ctrl+Shift+F. `#[serde(default)]` keeps any
    /// older save loading (the captured physical size is recovered
    /// from the PNG header as a fallback).
    #[serde(default)]
    pub logical_size: [f32; 2],
    /// Canvas-local top-left of the image. Zero for older saves and
    /// for any image captured at default `(pan = 0, zoom = 1)`. When
    /// the user freezes a frame after panning or zooming the canvas,
    /// we record `-pan / zoom` here so the captured pixels land
    /// exactly on the client area the user saw — independent of the
    /// canvas's current pan/zoom. Per-layer panning and zooming move it
    /// further (zoom also scales `logical_size`), so it doubles as the
    /// image's live anchor.
    ///
    /// `alias = "pos"` keeps saves written before this field was
    /// renamed loading with their panned position intact.
    #[serde(default, alias = "pos")]
    pub canvas_origin: [f32; 2],
    /// Zoom factor in effect at the moment of capture. Used together
    /// with `canvas_origin` to keep the image's screen footprint
    /// stable: the canvas-local extent we render into is
    /// `logical_size / capture_zoom`. Defaults to `1.0` so older
    /// saves continue to render at their physical-to-logical scale.
    #[serde(default = "default_capture_zoom")]
    pub capture_zoom: f32,
}

pub(super) fn default_capture_zoom() -> f32 { 1.0 }

impl LayerImage {
    /// Canvas-local `[width, height]` the image is painted into. Older
    /// saves carry no logical size; for those the renderer falls back
    /// to the physical pixel dimensions, and so does this.
    pub fn extent(&self) -> [f32; 2] {
        if self.logical_size[0] > 0.0 && self.logical_size[1] > 0.0 {
            self.logical_size
        } else {
            [self.size[0] as f32, self.size[1] as f32]
        }
    }
}

/// One named layer of ink. Layers stack in the order they appear in
/// `Canvas::layers` — earlier indices paint underneath later ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    /// Display name shown in the layer panel.
    pub name: String,
    /// When `false`, the layer's contents are skipped at render time and
    /// the eraser refuses to touch them.
    pub visible: bool,
    /// Per-layer opacity 0..=1. Renderer multiplies every stroke /
    /// shape alpha by this value so the user can fade a whole layer
    /// independently of the strokes' own colours. `#[serde(default)]`
    /// keeps older save files (without this field) loading as fully
    /// opaque.
    #[serde(default = "default_layer_opacity")]
    pub opacity: f32,
    /// Completed strokes for this layer.
    pub strokes: Vec<Stroke>,
    /// Completed shapes for this layer.
    pub shapes: Vec<Shape>,
    /// Optional raster background — set when the user runs
    /// "freeze frame to layer". Painted before any strokes / shapes,
    /// covering the layer's canvas region. `#[serde(default)]` keeps
    /// older saves loading without an image.
    #[serde(default)]
    pub image: Option<LayerImage>,
    /// Translation part of every per-layer pan / zoom baked into this
    /// layer's geometry — the `b` of the accumulated
    /// `p → scaled_by · p + moved_by` similarity. Pure bookkeeping: the
    /// renderer and the hit-testers never read it, because a layer
    /// transform rewrites the point data rather than living as a
    /// transform. It exists so "reset layer position" knows how far
    /// back to go. `#[serde(default)]` keeps older saves loading as
    /// "never moved".
    #[serde(default)]
    pub moved_by: [f32; 2],
    /// Running product of every per-layer zoom baked into this layer's
    /// geometry — the `a` of the accumulated `p → a · p + moved_by`
    /// similarity that maps the layer's original points to where they
    /// sit now. Same bookkeeping-only role as `moved_by`: nothing reads
    /// it at paint time, it exists so "reset layer zoom" knows what to
    /// divide out. `#[serde(default)]` keeps older saves loading as
    /// "never scaled".
    #[serde(default = "default_layer_scale")]
    pub scaled_by: f32,
    /// Bumped whenever an object on this layer is edited in place
    /// (moved, rotated, restyled, reordered). Such edits leave the
    /// stroke / shape counts unchanged, so this is what tells the
    /// thumbnail cache its picture is stale. Runtime-only.
    #[serde(skip)]
    pub rev: u64,
}

pub(super) fn default_layer_scale() -> f32 { 1.0 }

pub(super) fn default_layer_opacity() -> f32 { 1.0 }

impl Default for Layer {
    fn default() -> Self {
        Self {
            name: "Layer 1".into(),
            visible: true,
            opacity: 1.0,
            strokes: Vec::new(),
            shapes: Vec::new(),
            image: None,
            moved_by: [0.0, 0.0],
            scaled_by: 1.0,
            rev: 0,
        }
    }
}

impl Layer {
    /// Shift every piece of geometry this layer owns by `d` canvas-local
    /// pixels — strokes, shapes, and the frozen-frame raster alike, so
    /// the layer moves as one rigid unit.
    pub fn translate(&mut self, d: [f32; 2]) {
        for s in &mut self.strokes {
            s.translate(d);
        }
        for s in &mut self.shapes {
            s.translate(d);
        }
        if let Some(img) = self.image.as_mut() {
            img.canvas_origin[0] += d[0];
            img.canvas_origin[1] += d[1];
        }
    }

    /// Apply `p → a · p + b` to every piece of geometry this layer owns.
    /// The uniform-scale counterpart of `translate`: strokes, shapes and
    /// the frozen-frame raster all scale about the same point, so the
    /// layer resizes as one rigid unit.
    ///
    /// The frozen frame's `logical_size` is what the renderer uses as
    /// the canvas-local extent to stretch the decoded PNG into, so
    /// scaling it (rather than the physical `size`) resizes the plate
    /// without touching the payload.
    pub fn transform(&mut self, a: f32, b: [f32; 2]) {
        for s in &mut self.strokes {
            s.transform(a, b);
        }
        for s in &mut self.shapes {
            s.transform(a, b);
        }
        if let Some(img) = self.image.as_mut() {
            img.canvas_origin[0] = a * img.canvas_origin[0] + b[0];
            img.canvas_origin[1] = a * img.canvas_origin[1] + b[1];
            if img.logical_size[0] > 0.0 && img.logical_size[1] > 0.0 {
                img.logical_size[0] *= a;
                img.logical_size[1] *= a;
            } else {
                // Older save with no logical size: the renderer falls
                // back to the physical dimensions, so seed the logical
                // size from those to make the scale stick.
                img.logical_size = [img.size[0] as f32 * a, img.size[1] as f32 * a];
            }
        }
    }

    /// Canvas-local bounding box `[x0, y0, x1, y1]` of everything this
    /// layer paints, or `None` when the layer is empty. Used both by
    /// the thumbnail renderer (to frame the preview) and by "reset
    /// layer zoom" (to pick a pivot that keeps the layer where it sits
    /// instead of flinging it toward the canvas origin).
    pub fn content_bounds(&self) -> Option<[f32; 4]> {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut update = |bb: [f32; 4]| {
            if bb[0] < min_x { min_x = bb[0]; }
            if bb[1] < min_y { min_y = bb[1]; }
            if bb[2] > max_x { max_x = bb[2]; }
            if bb[3] > max_y { max_y = bb[3]; }
        };
        if let Some(img) = self.image.as_ref() {
            let (w, h) = if img.logical_size[0] > 0.0 && img.logical_size[1] > 0.0 {
                (img.logical_size[0], img.logical_size[1])
            } else {
                (img.size[0] as f32, img.size[1] as f32)
            };
            update([
                img.canvas_origin[0],
                img.canvas_origin[1],
                img.canvas_origin[0] + w,
                img.canvas_origin[1] + h,
            ]);
        }
        for s in &self.strokes {
            if let Some(bb) = s.bounds() { update(bb); }
        }
        for shape in &self.shapes {
            update(shape_bounds(shape));
        }
        if !min_x.is_finite() { return None; }
        Some([min_x, min_y, max_x, max_y])
    }

    /// Every object on this layer in group `id` (none for 0).
    pub fn group_members(&self, id: u64) -> hit::Selection {
        let mut out = hit::Selection::default();
        if id == 0 {
            return out;
        }
        out.strokes = self.strokes.iter().enumerate().filter(|(_, s)| s.group == id).map(|(i, _)| i).collect();
        out.shapes = self.shapes.iter().enumerate().filter(|(_, s)| s.group() == id).map(|(i, _)| i).collect();
        out
    }

    /// The group `obj` is in; 0 when none (or no such object).
    pub fn group_of(&self, obj: hit::ObjRef) -> u64 {
        match obj {
            hit::ObjRef::Stroke(i) => self.strokes.get(i).map_or(0, |s| s.group),
            hit::ObjRef::Shape(i) => self.shapes.get(i).map_or(0, Shape::group),
        }
    }

    /// `obj` and everything grouped with it.
    pub fn with_group(&self, obj: hit::ObjRef) -> Vec<hit::ObjRef> {
        let g = self.group_members(self.group_of(obj));
        if g.is_empty() {
            return vec![obj];
        }
        g.strokes.into_iter().map(hit::ObjRef::Stroke).chain(g.shapes.into_iter().map(hit::ObjRef::Shape)).collect()
    }
}

/// Give copies groups of their own: objects grouped together stay
/// together under a new id, so a copy never joins its original's group.
/// Copies are not linked to a PureRef board item either: only the
/// original follows the board (`Shape::link`).
pub fn regroup_copies(strokes: &mut [Stroke], shapes: &mut [Shape]) {
    let mut new: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut fresh = |g: u64| if g == 0 { 0 } else { *new.entry(g).or_insert_with(crate::canvas::shape::fresh_group_id) };
    for s in strokes {
        s.group = fresh(s.group);
    }
    for s in shapes {
        let g = fresh(s.group());
        s.set_group(g);
        s.set_link(0);
    }
}

/// Canvas-local bounding box of one shape, padded by its stroke width
/// where that matters. Text is measured coarsely (the real layout only
/// happens at paint time) — good enough for hit-testing and framing.
/// A rotated shape reports the axis-aligned box around its turned
/// corners.
pub fn shape_bounds(shape: &Shape) -> [f32; 4] {
    let bb = shape.obb().aabb();
    let pad = match shape {
        Shape::Rect    { stroke_width, .. }
        | Shape::Ellipse { stroke_width, .. }
        | Shape::Line    { stroke_width, .. }
        | Shape::Arrow   { stroke_width, .. } => *stroke_width,
        Shape::Text { .. } | Shape::Raster { .. } => 0.0,
    };
    [bb[0] - pad, bb[1] - pad, bb[2] + pad, bb[3] + pad]
}

/// `p` in the axes of a frame turned by `angle`: turned back by it.
pub fn unturn(p: [f32; 2], angle: f32) -> [f32; 2] {
    let (s, c) = angle.sin_cos();
    [c * p[0] + s * p[1], -s * p[0] + c * p[1]]
}

/// `shape_bounds` along the axes of a frame turned by `angle`: the box,
/// in those axes (see `unturn`), round what the shape covers.
pub fn shape_bounds_turned(shape: &Shape, angle: f32) -> [f32; 4] {
    let points: Vec<[f32; 2]> = match shape {
        Shape::Line { a, b, .. } | Shape::Arrow { a, b, .. } => vec![*a, *b],
        s => s.obb().corners().to_vec(),
    };
    let pad = match shape {
        Shape::Rect    { stroke_width, .. }
        | Shape::Ellipse { stroke_width, .. }
        | Shape::Line    { stroke_width, .. }
        | Shape::Arrow   { stroke_width, .. } => *stroke_width,
        Shape::Text { .. } | Shape::Raster { .. } => 0.0,
    };
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in points {
        let q = unturn(p, angle);
        b = [b[0].min(q[0]), b[1].min(q[1]), b[2].max(q[0]), b[3].max(q[1])];
    }
    [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]
}
