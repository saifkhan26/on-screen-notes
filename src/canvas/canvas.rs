//! `Canvas` — one drawable surface holding a stack of layers plus its own
//! view transform (pan + zoom).
//!
//! Each `Layer` holds its own strokes and shapes; layers render in order
//! (index 0 = bottom, last = top). The "active" layer is the one tool
//! mutations target — new strokes are appended to it and the eraser only
//! deletes from it.
//!
//! Rendering lives in `render.rs`; tools live in `crate::tools`. This
//! separation makes each part easier to read and test.

use crate::canvas::affine::Affine;
use crate::canvas::hit;
use crate::canvas::shape::{RasterKind, Shape};
use crate::canvas::stroke::Stroke;
use serde::{Deserialize, Serialize};

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

fn default_capture_zoom() -> f32 { 1.0 }

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

fn default_layer_scale() -> f32 { 1.0 }

fn default_layer_opacity() -> f32 { 1.0 }

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

/// `Canvas` owns a stack of layers and a view transform. The on-disk
/// format keeps legacy `strokes` / `shapes` fields so canvases saved by
/// older builds still load cleanly — `ensure_migrated` folds them into a
/// new "Layer 1" on first access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Canvas {
    /// Layer stack — `[0]` paints bottom, last paints top.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Index of the layer that receives new strokes and is targeted by
    /// the eraser.
    #[serde(default)]
    pub active_layer: usize,

    /// **Legacy** flat stroke list. Populated only when loading an old
    /// file; migrated into `layers[0]` by `ensure_migrated`.
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "strokes")]
    pub legacy_strokes: Vec<Stroke>,
    /// **Legacy** flat shape list — same migration story as above.
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "shapes")]
    pub legacy_shapes: Vec<Shape>,

    /// Pan offset in canvas pixels: a positive `pan.x` shifts the canvas
    /// content right.
    pub pan: [f32; 2],
    /// Zoom factor: 1.0 = 100%. Stored separately from pan so we can zoom
    /// "toward the cursor" by adjusting both.
    pub zoom: f32,

    /// History of "undo" snapshots. We don't snapshot the whole canvas
    /// (memory!) — instead we store a tag that lets us undo the last edit.
    /// Skipped from serialisation: undo history doesn't survive restarts.
    #[serde(skip)]
    pub undo_log: Vec<UndoEntry>,
    /// Redo stack — populated when the user undoes; cleared on a new edit.
    #[serde(skip)]
    pub redo_log: Vec<UndoEntry>,

    /// "Has unsaved changes." Set by `CanvasManager::active_mut`, cleared
    /// once the canvas is written to disk, so the debounced autosave can
    /// skip canvases nobody has touched.
    ///
    /// Without this the autosave re-serialised *every* canvas every
    /// 1.5 s. With frozen-frame layers (a full-screen PNG stored inline)
    /// that is hundreds of megabytes of text per pass — a ~1 s freeze of
    /// the UI thread, on repeat, even while completely idle.
    ///
    /// `#[serde(skip)]` deserialises to `false`: a canvas just read from
    /// disk matches the file, so it needs no rewrite.
    #[serde(skip)]
    pub dirty: bool,
}

/// One reversible action. The `usize` is always the layer index the
/// action targeted, so undo/redo replays into the right layer even if
/// the user has since switched the active layer.
#[derive(Debug, Clone)]
pub enum UndoEntry {
    StrokeAdded(usize),                   // pop last stroke on layer `usize`
    ShapeAdded(usize),                    // pop last shape on layer `usize`
    StrokeErased(usize, usize, Stroke),   // (layer, index, stroke) → re-insert
    ShapeErased(usize, usize, Shape),     // (layer, index, shape)  → re-insert
    Cleared(usize, Vec<Stroke>, Vec<Shape>), // restore both lists on layer
    /// Lasso partial-erase: the stroke at `idx` was replaced with
    /// zero or more `pieces`. Undo reverses the substitution
    /// atomically so one Ctrl+Z restores the original stroke and
    /// removes every fragment.
    StrokePartiallyErased(usize, usize, Stroke, Vec<Stroke>),
    /// Shape at `idx` was replaced in place (e.g. a raster fill
    /// whose alpha was partially cleared by the lasso). Undo
    /// swaps `old` back in; redo swaps `new` back. Atomic — one
    /// Ctrl+Z step per lasso pass.
    ShapeReplaced(usize, usize, Shape, Shape),
    /// Layer `usize` was panned by the given canvas-local delta
    /// (Shift+Space drag). The translation is baked into the layer's
    /// point data, so undo just translates back by `-delta` — no
    /// geometry is cloned into the history.
    LayerMoved(usize, [f32; 2]),
    /// Layer `usize` was scaled (Shift+Space wheel) by the similarity
    /// `p → a · p + b`, where `a` is the zoom factor and `b` folds in
    /// the pivot offset — plus any pan that shared the same gesture.
    /// Like `LayerMoved` the transform is baked into the point data,
    /// so undo applies the exact inverse `p → (p - b) / a` and no
    /// geometry is cloned into the history.
    LayerScaled(usize, f32, [f32; 2]),
    /// Objects on layer `usize` were edited in place — moved, rotated,
    /// resized or restyled as a selection. Each element is `(index,
    /// before, after)`; undo writes `before` back, redo writes `after`.
    /// Snapshots rather than a stored transform because a shape's
    /// rotation / size cannot be recovered exactly by inverting an
    /// affine, and raster shapes share their PNG so a snapshot is cheap.
    ObjectsReplaced(usize, Vec<(usize, Stroke, Stroke)>, Vec<(usize, Shape, Shape)>),
    /// Objects were removed from layer `usize` in one step (Delete /
    /// cut). Each element is `(index it sat at, object)`, ascending, so
    /// re-inserting them in order puts every one back where it was.
    ObjectsRemoved(usize, Vec<(usize, Stroke)>, Vec<(usize, Shape)>),
    /// Objects were inserted into layer `usize` in one step (duplicate /
    /// paste). Same payload shape as `ObjectsRemoved`; the two are each
    /// other's inverse.
    ObjectsInserted(usize, Vec<(usize, Stroke)>, Vec<(usize, Shape)>),
    /// The stroke list and / or shape list of layer `usize` was
    /// re-ordered (bring to front / send to back): `new[i] =
    /// old[perm[i]]`. An empty permutation means that list was left
    /// alone. Undo applies the inverse permutation.
    ObjectsReordered(usize, Vec<usize>, Vec<usize>),
    /// Layer `usize`'s background plate was turned into an ordinary
    /// image object at the bottom of its shape list (see
    /// `Canvas::promote_plate`). The `f32` is the plate's
    /// `capture_zoom`, kept only so undo can rebuild the plate exactly.
    ///
    /// The picture looks the same either way, so on its own this step
    /// would be a Ctrl+Z that appears to do nothing. Undo and redo
    /// therefore pass straight through it to the next entry.
    PlatePromoted(usize, f32),
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            layers: vec![Layer::default()],
            active_layer: 0,
            legacy_strokes: Vec::new(),
            legacy_shapes:  Vec::new(),
            pan: [0.0, 0.0],
            zoom: 1.0,
            undo_log: Vec::new(),
            redo_log: Vec::new(),
            // A freshly created canvas has no file behind it yet.
            dirty: true,
        }
    }
}

impl Canvas {
    /// Migrate an old-format canvas (flat strokes/shapes, no layers) into
    /// the new layered representation. Idempotent: re-runs are no-ops.
    /// Always called after deserialise and after default-construction so
    /// the rest of the code can assume `layers` is non-empty.
    pub fn ensure_migrated(&mut self) {
        if self.layers.is_empty() {
            let mut l = Layer::default();
            l.strokes = std::mem::take(&mut self.legacy_strokes);
            l.shapes  = std::mem::take(&mut self.legacy_shapes);
            self.layers.push(l);
            self.active_layer = 0;
            // The in-memory shape no longer matches the legacy file, so
            // the migrated form needs writing back out.
            self.dirty = true;
        } else {
            // Fresh-format save — drop any stray legacy data.
            self.legacy_strokes.clear();
            self.legacy_shapes.clear();
        }
        if self.active_layer >= self.layers.len() {
            self.active_layer = self.layers.len() - 1;
        }
    }

    /// Mutable access to the active layer. Panics only if a caller
    /// somehow bypassed `ensure_migrated`, which is invariant on every
    /// construction path.
    #[allow(dead_code)]
    pub fn active_layer_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.active_layer]
    }

    /// Append a finished stroke and record an undo entry. We pre-build the
    /// stroke's render cache here so the first paint after commit is as
    /// fast as every subsequent paint (the user has just lifted the pen
    /// and a frame later will see their stroke — keeping that frame cheap
    /// matters for the perceived snappiness).
    pub fn add_stroke(&mut self, mut stroke: Stroke) {
        stroke.build_cache();
        let li = self.active_layer;
        self.layers[li].strokes.push(stroke);
        self.undo_log.push(UndoEntry::StrokeAdded(li));
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Append a shape and record an undo entry.
    pub fn add_shape(&mut self, shape: Shape) {
        let li = self.active_layer;
        self.layers[li].shapes.push(shape);
        self.undo_log.push(UndoEntry::ShapeAdded(li));
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Remove all strokes and shapes from the active layer.
    /// (Ctrl+Delete clears the current layer only — to wipe everything,
    /// delete the canvas itself with Ctrl+Shift+Backspace.)
    pub fn clear(&mut self) {
        let li = self.active_layer;
        let prev_strokes = std::mem::take(&mut self.layers[li].strokes);
        let prev_shapes  = std::mem::take(&mut self.layers[li].shapes);
        self.undo_log.push(UndoEntry::Cleared(li, prev_strokes, prev_shapes));
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Undo the most recent edit if any. Pushes inverse onto redo log.
    pub fn undo(&mut self) {
        let Some(entry) = self.undo_log.pop() else { return };
        self.dirty = true;
        match entry {
            UndoEntry::StrokeAdded(li) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    if let Some(s) = layer.strokes.pop() {
                        self.redo_log.push(UndoEntry::StrokeErased(li, layer.strokes.len(), s));
                    }
                }
            }
            UndoEntry::ShapeAdded(li) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    if let Some(s) = layer.shapes.pop() {
                        self.redo_log.push(UndoEntry::ShapeErased(li, layer.shapes.len(), s));
                    }
                }
            }
            UndoEntry::StrokeErased(li, i, s) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    let i = i.min(layer.strokes.len());
                    layer.strokes.insert(i, s);
                    self.redo_log.push(UndoEntry::StrokeAdded(li));
                }
            }
            UndoEntry::ShapeErased(li, i, s) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    let i = i.min(layer.shapes.len());
                    layer.shapes.insert(i, s);
                    self.redo_log.push(UndoEntry::ShapeAdded(li));
                }
            }
            UndoEntry::Cleared(li, strokes, shapes) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    let cur_s = std::mem::take(&mut layer.strokes);
                    let cur_p = std::mem::take(&mut layer.shapes);
                    layer.strokes = strokes;
                    layer.shapes  = shapes;
                    self.redo_log.push(UndoEntry::Cleared(li, cur_s, cur_p));
                }
            }
            UndoEntry::StrokePartiallyErased(li, idx, original, pieces) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    // Reverse the lasso: remove the fragment range
                    // starting at `idx`, then put the original back.
                    let n = pieces.len();
                    let start = idx.min(layer.strokes.len());
                    for _ in 0..n {
                        if start < layer.strokes.len() {
                            layer.strokes.remove(start);
                        }
                    }
                    let original_clone = original.clone();
                    let pieces_clone = pieces.clone();
                    let i = start.min(layer.strokes.len());
                    layer.strokes.insert(i, original);
                    self.redo_log.push(UndoEntry::StrokePartiallyErased(
                        li, idx, original_clone, pieces_clone,
                    ));
                }
            }
            UndoEntry::ShapeReplaced(li, idx, old, new) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    if idx < layer.shapes.len() {
                        layer.shapes[idx] = old.clone();
                        self.redo_log.push(UndoEntry::ShapeReplaced(li, idx, old, new));
                    }
                }
            }
            UndoEntry::LayerMoved(li, d) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    layer.translate([-d[0], -d[1]]);
                    layer.moved_by[0] -= d[0];
                    layer.moved_by[1] -= d[1];
                    self.redo_log.push(UndoEntry::LayerMoved(li, d));
                }
            }
            UndoEntry::LayerScaled(li, a, b) => {
                if a.abs() > 1e-6 {
                    apply_layer_xform(&mut self.layers, li, 1.0 / a, [-b[0] / a, -b[1] / a]);
                    self.redo_log.push(UndoEntry::LayerScaled(li, a, b));
                }
            }
            UndoEntry::ObjectsReplaced(li, strokes, shapes) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    for (i, before, _) in &strokes {
                        if let Some(s) = layer.strokes.get_mut(*i) { *s = before.clone(); }
                    }
                    for (i, before, _) in &shapes {
                        if let Some(s) = layer.shapes.get_mut(*i) { *s = before.clone(); }
                    }
                    layer.rev += 1;
                    self.redo_log.push(UndoEntry::ObjectsReplaced(li, strokes, shapes));
                }
            }
            entry @ (UndoEntry::ObjectsRemoved(..)
            | UndoEntry::ObjectsInserted(..)
            | UndoEntry::ObjectsReordered(..)) => {
                if let Some(inverse) = invert_object_entry(&mut self.layers, entry) {
                    self.redo_log.push(inverse);
                }
            }
            UndoEntry::PlatePromoted(li, capture_zoom) => {
                if demote_plate(&mut self.layers, li, capture_zoom) {
                    self.redo_log.push(UndoEntry::PlatePromoted(li, capture_zoom));
                }
                // Invisible step: carry on to the edit before it.
                self.undo();
            }
        }
    }

    /// Redo the most recently undone edit if any.
    pub fn redo(&mut self) {
        let Some(entry) = self.redo_log.pop() else { return };
        self.dirty = true;
        match entry {
            UndoEntry::StrokeAdded(li) => self.undo_log.push(UndoEntry::StrokeAdded(li)),
            UndoEntry::ShapeAdded(li)  => self.undo_log.push(UndoEntry::ShapeAdded(li)),
            UndoEntry::StrokeErased(li, i, s) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    let i = i.min(layer.strokes.len());
                    layer.strokes.insert(i, s);
                    self.undo_log.push(UndoEntry::StrokeAdded(li));
                }
            }
            UndoEntry::ShapeErased(li, i, s) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    let i = i.min(layer.shapes.len());
                    layer.shapes.insert(i, s);
                    self.undo_log.push(UndoEntry::ShapeAdded(li));
                }
            }
            UndoEntry::Cleared(li, strokes, shapes) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    let cur_s = std::mem::take(&mut layer.strokes);
                    let cur_p = std::mem::take(&mut layer.shapes);
                    layer.strokes = strokes;
                    layer.shapes  = shapes;
                    self.undo_log.push(UndoEntry::Cleared(li, cur_s, cur_p));
                }
            }
            UndoEntry::StrokePartiallyErased(li, idx, original, pieces) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    // Re-apply the lasso: remove the original at
                    // `idx`, splice the fragments back in.
                    let start = idx.min(layer.strokes.len());
                    if start < layer.strokes.len() {
                        layer.strokes.remove(start);
                    }
                    let original_clone = original.clone();
                    let pieces_clone = pieces.clone();
                    for (k, mut p) in pieces.into_iter().enumerate() {
                        p.build_cache();
                        let pos = (start + k).min(layer.strokes.len());
                        layer.strokes.insert(pos, p);
                    }
                    self.undo_log.push(UndoEntry::StrokePartiallyErased(
                        li, idx, original_clone, pieces_clone,
                    ));
                }
            }
            UndoEntry::ShapeReplaced(li, idx, old, new) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    if idx < layer.shapes.len() {
                        layer.shapes[idx] = new.clone();
                        self.undo_log.push(UndoEntry::ShapeReplaced(li, idx, old, new));
                    }
                }
            }
            UndoEntry::LayerMoved(li, d) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    layer.translate(d);
                    layer.moved_by[0] += d[0];
                    layer.moved_by[1] += d[1];
                    self.undo_log.push(UndoEntry::LayerMoved(li, d));
                }
            }
            UndoEntry::LayerScaled(li, a, b) => {
                apply_layer_xform(&mut self.layers, li, a, b);
                self.undo_log.push(UndoEntry::LayerScaled(li, a, b));
            }
            UndoEntry::ObjectsReplaced(li, strokes, shapes) => {
                if let Some(layer) = self.layers.get_mut(li) {
                    for (i, _, after) in &strokes {
                        if let Some(s) = layer.strokes.get_mut(*i) { *s = after.clone(); }
                    }
                    for (i, _, after) in &shapes {
                        if let Some(s) = layer.shapes.get_mut(*i) { *s = after.clone(); }
                    }
                    layer.rev += 1;
                    self.undo_log.push(UndoEntry::ObjectsReplaced(li, strokes, shapes));
                }
            }
            // These three record what the *undo* just did, so redoing
            // is the same "apply the inverse" step undo uses.
            entry @ (UndoEntry::ObjectsRemoved(..)
            | UndoEntry::ObjectsInserted(..)
            | UndoEntry::ObjectsReordered(..)) => {
                if let Some(inverse) = invert_object_entry(&mut self.layers, entry) {
                    self.undo_log.push(inverse);
                }
            }
            UndoEntry::PlatePromoted(li, _) => {
                if let Some(capture_zoom) = promote_plate_in(&mut self.layers, li) {
                    self.undo_log.push(UndoEntry::PlatePromoted(li, capture_zoom));
                }
                // Invisible step: carry on to the edit after it.
                self.redo();
            }
        }
    }

    /// Mark this canvas dirty without other side effects. Use when a
    /// mutation lives outside the canvas methods (e.g. layer-opacity
    /// edits via a slider or hotkey).
    pub fn mark_dirty(&mut self) { self.dirty = true; }

    /// Add to `out` the name of every media-folder file this canvas uses
    /// (videos, GIFs, stored pictures): on its layers, and in its undo
    /// and redo history, since an undo can bring back an object that was
    /// deleted.
    pub fn media_files(&self, out: &mut std::collections::HashSet<String>) {
        let mut add = |s: &Shape| {
            if let Some(f) = s.media_file() {
                out.insert(f.to_string());
            }
        };
        for layer in &self.layers {
            layer.shapes.iter().for_each(&mut add);
        }
        self.legacy_shapes.iter().for_each(&mut add);
        for entry in self.undo_log.iter().chain(&self.redo_log) {
            // No catch-all arm: a new entry kind that holds shapes must
            // be added here, or its videos could be deleted while an undo
            // still needs them.
            match entry {
                UndoEntry::ShapeErased(_, _, s) => add(s),
                UndoEntry::Cleared(_, _, shapes) => shapes.iter().for_each(&mut add),
                UndoEntry::ShapeReplaced(_, _, old, new) => {
                    add(old);
                    add(new);
                }
                UndoEntry::ObjectsReplaced(_, _, shapes) => {
                    for (_, before, after) in shapes {
                        add(before);
                        add(after);
                    }
                }
                UndoEntry::ObjectsRemoved(_, _, shapes) | UndoEntry::ObjectsInserted(_, _, shapes) => {
                    shapes.iter().for_each(|(_, s)| add(s));
                }
                UndoEntry::StrokeAdded(_)
                | UndoEntry::ShapeAdded(_)
                | UndoEntry::StrokeErased(..)
                | UndoEntry::StrokePartiallyErased(..)
                | UndoEntry::LayerMoved(..)
                | UndoEntry::LayerScaled(..)
                | UndoEntry::ObjectsReordered(..)
                | UndoEntry::PlatePromoted(..) => {}
            }
        }
    }

    /// Erase every stroke / shape on the **active** layer whose ink the
    /// eraser disc at `point` (canvas-local) actually touches. Returns
    /// true if anything was erased. Strokes / shapes on other layers
    /// are left alone, even when they sit visually under the eraser tip.
    ///
    /// "Touches the ink" is a distance test against the drawn line (see
    /// `canvas::hit`), not against the bounding box — a long diagonal
    /// stroke is no longer erased by a tip passing through the empty
    /// corner of its box. Two raster cases differ:
    /// * an `Image` (screen capture, pasted picture) is never erased —
    ///   here or by the lasso eraser. The user is rubbing out ink drawn
    ///   over it; the picture itself is removed with the Select tool.
    /// * a `Fill` sticker keeps the bounding-box test: telling which of
    ///   its pixels are transparent would mean decoding the PNG on every
    ///   pen sample.
    pub fn erase_at(&mut self, point: [f32; 2], radius: f32) -> bool {
        let li = self.active_layer;
        let mut erased_any = false;
        let layer = &mut self.layers[li];
        // Strokes: walk in reverse so indices stay valid as we remove.
        let mut i = layer.strokes.len();
        while i > 0 {
            i -= 1;
            if hit::stroke_hit(&layer.strokes[i], point, radius) {
                let s = layer.strokes.remove(i);
                self.undo_log.push(UndoEntry::StrokeErased(li, i, s));
                erased_any = true;
            }
        }
        let mut i = layer.shapes.len();
        while i > 0 {
            i -= 1;
            let touched = match &layer.shapes[i] {
                Shape::Raster { kind: RasterKind::Image, .. } => false,
                s @ Shape::Raster { .. } => rect_contains_circle(shape_bounds(s), point, radius),
                s => hit::shape_hit(s, point, radius),
            };
            if touched {
                let s = layer.shapes.remove(i);
                self.undo_log.push(UndoEntry::ShapeErased(li, i, s));
                erased_any = true;
            }
        }
        if erased_any {
            self.redo_log.clear();
        }
        erased_any
    }

    /// Lasso-erase. Walks every stroke / shape on the active layer
    /// and removes those whose centroid lies inside `polygon` (a
    /// closed loop of canvas-local points). Each removal records an
    /// undo entry so a stray loop is one Ctrl+Z away from recovery.
    /// `polygon` is assumed to have at least 3 vertices — callers
    /// should gate on that before invoking.
    pub fn erase_in_polygon(&mut self, polygon: &[[f32; 2]]) -> bool {
        if polygon.len() < 3 { return false; }
        let li = self.active_layer;
        let mut erased_any = false;
        let layer = &mut self.layers[li];
        // Walk strokes in reverse so we can splice fragments back at
        // each removed slot without index shifts breaking the rest
        // of the walk.
        let mut i = layer.strokes.len();
        while i > 0 {
            i -= 1;
            if layer.strokes[i].samples.is_empty() { continue; }
            // Quick reject: if no sample sits inside the polygon,
            // the stroke is untouched and we can skip the split.
            let any_inside = layer.strokes[i]
                .samples
                .iter()
                .any(|s| point_in_polygon(s.pos, polygon));
            if !any_inside { continue; }
            // Split: build runs of consecutive *outside* samples
            // into fragment strokes. Single-sample runs are dropped
            // — a one-point fragment would render as a stray dot.
            let pieces = split_stroke_outside_polygon(&layer.strokes[i], polygon);
            let original = layer.strokes.remove(i);
            for (k, mut piece) in pieces.iter().cloned().enumerate() {
                // Pre-build the cache so the next paint is cheap.
                piece.build_cache();
                layer.strokes.insert(i + k, piece);
            }
            self.undo_log
                .push(UndoEntry::StrokePartiallyErased(li, i, original, pieces));
            erased_any = true;
        }
        let mut i = layer.shapes.len();
        while i > 0 {
            i -= 1;
            // Pictures are what the user is annotating, not ink: neither
            // eraser touches them (see `erase_at`). Looping ink drawn
            // over a screenshot must not punch a hole in the screenshot.
            if let Shape::Raster { kind: RasterKind::Image, .. } = &layer.shapes[i] {
                continue;
            }
            // Fill stickers get a per-pixel partial erase so the
            // lasso clips out only the bit of the fill the user
            // looped, leaving the rest intact.
            if let Shape::Raster { .. } = &layer.shapes[i] {
                match raster_partial_erase(&layer.shapes[i], polygon) {
                    RasterEraseOutcome::Skip => {}
                    RasterEraseOutcome::Delete => {
                        let removed = layer.shapes.remove(i);
                        self.undo_log.push(UndoEntry::ShapeErased(li, i, removed));
                        erased_any = true;
                    }
                    RasterEraseOutcome::Replace(new_shape) => {
                        let old = std::mem::replace(&mut layer.shapes[i], new_shape.clone());
                        self.undo_log
                            .push(UndoEntry::ShapeReplaced(li, i, old, new_shape));
                        erased_any = true;
                    }
                }
                continue;
            }
            let center = layer.shapes[i].obb().center;
            if point_in_polygon(center, polygon) {
                let removed = layer.shapes.remove(i);
                self.undo_log.push(UndoEntry::ShapeErased(li, i, removed));
                erased_any = true;
            }
        }
        if erased_any {
            self.redo_log.clear();
        }
        erased_any
    }

    /// Turn layer `li`'s background plate (a frozen frame, or a lasso
    /// capture made before captures became objects) into an ordinary
    /// image object, so the Select tool can move, resize, rotate or
    /// delete it like any other. Returns the new object's index in the
    /// layer's shape list, or `None` if the layer has no plate.
    ///
    /// The image keeps its place and its position in the paint order.
    /// Every shape already on the layer moves up one index, which is
    /// why this is recorded in the undo history rather than done
    /// quietly: earlier entries refer to shapes by index, and they
    /// stay correct because undoing back past this point puts the
    /// plate — and the old indices — back first.
    pub fn promote_plate(&mut self, li: usize) -> Option<usize> {
        let capture_zoom = promote_plate_in(&mut self.layers, li)?;
        self.undo_log.push(UndoEntry::PlatePromoted(li, capture_zoom));
        self.redo_log.clear();
        self.dirty = true;
        Some(0)
    }

    // -- selection edits --------------------------------------------------
    //
    // Everything the Select tool does to a set of objects. Each call is
    // one undo step. Index slices are expected ascending and unique
    // (`hit::Selection` keeps them that way); indices past the end of
    // a list are skipped rather than trusted.

    /// Run `edit_stroke` / `edit_shape` over the chosen objects on layer
    /// `li`, recording before / after snapshots as one undo entry.
    /// Returns false when nothing was touched.
    fn edit_objects(
        &mut self,
        li: usize,
        strokes: &[usize],
        shapes: &[usize],
        mut edit_stroke: impl FnMut(&mut Stroke),
        mut edit_shape: impl FnMut(&mut Shape),
    ) -> bool {
        let Some(layer) = self.layers.get_mut(li) else { return false };
        let mut stroke_log = Vec::with_capacity(strokes.len());
        let mut shape_log = Vec::with_capacity(shapes.len());
        for &i in strokes {
            if let Some(s) = layer.strokes.get_mut(i) {
                let before = s.clone();
                edit_stroke(s);
                stroke_log.push((i, before, s.clone()));
            }
        }
        for &i in shapes {
            if let Some(s) = layer.shapes.get_mut(i) {
                let before = s.clone();
                edit_shape(s);
                shape_log.push((i, before, s.clone()));
            }
        }
        if stroke_log.is_empty() && shape_log.is_empty() { return false; }
        layer.rev += 1;
        self.undo_log.push(UndoEntry::ObjectsReplaced(li, stroke_log, shape_log));
        self.redo_log.clear();
        self.dirty = true;
        true
    }

    /// Move / rotate / resize the chosen objects by `m`. Baked into the
    /// geometry for the same reason layer transforms are (see
    /// `move_layer_by`): every other subsystem assumes one canvas-local
    /// space.
    pub fn transform_objects(&mut self, li: usize, strokes: &[usize], shapes: &[usize], m: &Affine) {
        if m.is_identity() { return; }
        self.edit_objects(li, strokes, shapes, |s| s.apply_affine(m), |s| s.apply_affine(m));
    }

    /// Recolour and / or re-width the chosen objects.
    ///
    /// `merge_into_last` folds this change into the undo entry on top of
    /// the history instead of adding one, provided that entry is an
    /// in-place edit of exactly these objects. The size slider fires on
    /// every pixel of a drag; merging turns that stream into a single
    /// Ctrl+Z step back to the width the objects started with. The
    /// caller is responsible for only asking when the top entry is its
    /// own previous restyle.
    pub fn restyle_objects(
        &mut self,
        li: usize,
        strokes: &[usize],
        shapes: &[usize],
        color: Option<[u8; 4]>,
        width: Option<f32>,
        merge_into_last: bool,
    ) {
        let edit_stroke = |s: &mut Stroke| {
            if let Some(c) = color { s.color = c; }
            if let Some(w) = width { s.set_base_width(w); }
        };
        let edit_shape = |s: &mut Shape| s.restyle(color, width);

        if merge_into_last {
            let layer = self.layers.get_mut(li);
            if let (Some(layer), Some(UndoEntry::ObjectsReplaced(l, stroke_log, shape_log))) =
                (layer, self.undo_log.last_mut())
            {
                let same = *l == li
                    && stroke_log.iter().map(|e| e.0).eq(strokes.iter().copied())
                    && shape_log.iter().map(|e| e.0).eq(shapes.iter().copied());
                if same {
                    for (i, _, after) in stroke_log.iter_mut() {
                        if let Some(s) = layer.strokes.get_mut(*i) {
                            edit_stroke(s);
                            *after = s.clone();
                        }
                    }
                    for (i, _, after) in shape_log.iter_mut() {
                        if let Some(s) = layer.shapes.get_mut(*i) {
                            edit_shape(s);
                            *after = s.clone();
                        }
                    }
                    layer.rev += 1;
                    self.dirty = true;
                    return;
                }
            }
        }
        self.edit_objects(li, strokes, shapes, edit_stroke, edit_shape);
    }

    /// Remove the chosen objects in one undo step.
    pub fn delete_objects(&mut self, li: usize, strokes: &[usize], shapes: &[usize]) {
        let Some(layer) = self.layers.get_mut(li) else { return };
        let removed_strokes = remove_at(&mut layer.strokes, strokes);
        let removed_shapes = remove_at(&mut layer.shapes, shapes);
        if removed_strokes.is_empty() && removed_shapes.is_empty() { return; }
        layer.rev += 1;
        self.undo_log.push(UndoEntry::ObjectsRemoved(li, removed_strokes, removed_shapes));
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Append objects to layer `li` in one undo step (duplicate /
    /// paste). Returns where they landed, ready to become the new
    /// selection.
    pub fn insert_objects(
        &mut self,
        li: usize,
        strokes: Vec<Stroke>,
        shapes: Vec<Shape>,
    ) -> hit::Selection {
        let mut placed = hit::Selection::default();
        let Some(layer) = self.layers.get_mut(li) else { return placed };
        if strokes.is_empty() && shapes.is_empty() { return placed; }
        let mut stroke_log = Vec::with_capacity(strokes.len());
        for mut s in strokes {
            // No-op when the stroke already carries its cache (a clone
            // of a painted stroke does).
            s.build_cache();
            let i = layer.strokes.len();
            layer.strokes.push(s.clone());
            stroke_log.push((i, s));
            placed.strokes.push(i);
        }
        let mut shape_log = Vec::with_capacity(shapes.len());
        for s in shapes {
            let i = layer.shapes.len();
            layer.shapes.push(s.clone());
            shape_log.push((i, s));
            placed.shapes.push(i);
        }
        layer.rev += 1;
        self.undo_log.push(UndoEntry::ObjectsInserted(li, stroke_log, shape_log));
        self.redo_log.clear();
        self.dirty = true;
        placed
    }

    /// Bring the chosen objects to the front of their list (painted
    /// last, so on top) or send them to the back. Strokes only move
    /// among strokes and shapes among shapes — a layer paints all its
    /// shapes before any stroke, and that order is part of the save
    /// format. Returns where the objects sit afterwards.
    pub fn reorder_objects(
        &mut self,
        li: usize,
        strokes: &[usize],
        shapes: &[usize],
        to_front: bool,
    ) -> hit::Selection {
        let mut placed = hit::Selection { strokes: strokes.to_vec(), shapes: shapes.to_vec() };
        let Some(layer) = self.layers.get_mut(li) else { return placed };
        let identity = |perm: &[usize]| perm.iter().enumerate().all(|(i, &p)| i == p);
        // An empty permutation is the entry's way of saying "this list
        // was not touched".
        let perm_for = |len: usize, picked: &[usize]| {
            if picked.is_empty() { return Vec::new(); }
            let perm = regroup_perm(len, picked, to_front);
            if identity(&perm) { Vec::new() } else { perm }
        };
        let stroke_perm = perm_for(layer.strokes.len(), strokes);
        let shape_perm = perm_for(layer.shapes.len(), shapes);
        let landing = |len: usize, picked: &[usize]| -> Vec<usize> {
            let n = picked.iter().filter(|&&i| i < len).count();
            if to_front { (len - n..len).collect() } else { (0..n).collect() }
        };
        placed.strokes = landing(layer.strokes.len(), strokes);
        placed.shapes = landing(layer.shapes.len(), shapes);
        if stroke_perm.is_empty() && shape_perm.is_empty() { return placed; }
        permute(&mut layer.strokes, &stroke_perm);
        permute(&mut layer.shapes, &shape_perm);
        layer.rev += 1;
        self.undo_log.push(UndoEntry::ObjectsReordered(li, stroke_perm, shape_perm));
        self.redo_log.clear();
        self.dirty = true;
        placed
    }

    // -- layer management -------------------------------------------------

    /// Pan the layer at `li` by `d` canvas-local pixels, baking the
    /// translation straight into its geometry.
    ///
    /// Baking (rather than storing a per-layer transform the renderer
    /// applies) keeps `Stroke::samples` and `Shape` coordinates in the
    /// single canvas-local space every other subsystem already assumes —
    /// the eraser's hit-testing, the flood fill's rasterise/map-back
    /// round trip, and the tiny-skia screenshot path all keep working
    /// with no offset plumbing. The cost is one add per point, paid once
    /// per gesture on pen-up rather than on every frame.
    pub fn move_layer_by(&mut self, li: usize, d: [f32; 2]) {
        // A Shift+Space click with no motion should not litter the undo
        // history with a no-op entry.
        if d[0] == 0.0 && d[1] == 0.0 { return; }
        let Some(layer) = self.layers.get_mut(li) else { return };
        layer.translate(d);
        layer.moved_by[0] += d[0];
        layer.moved_by[1] += d[1];
        self.undo_log.push(UndoEntry::LayerMoved(li, d));
        self.redo_log.clear();
    }

    /// Apply the similarity `p → a · p + b` to the layer at `li`,
    /// baking it into the geometry for exactly the reasons
    /// `move_layer_by` bakes translations: every other subsystem
    /// (eraser hit-tests, flood fill, tiny-skia screenshots) assumes
    /// one canvas-local space, so a per-layer transform sitting in the
    /// renderer would need offset plumbing everywhere.
    ///
    /// Callers normally want `scale_layer_about`; this is the raw form
    /// the Shift+Space gesture commits, because a gesture that both
    /// pans and zooms collapses into a single `a`/`b` pair.
    pub fn transform_layer(&mut self, li: usize, a: f32, b: [f32; 2]) {
        // Identity: a wheel-less Shift+Space tap should not litter the
        // undo history. `a <= 0` would mirror or collapse the layer —
        // no gesture produces it, and inverting it on undo is
        // undefined, so refuse.
        if a <= 0.0 { return; }
        if (a - 1.0).abs() < 1e-6 && b[0] == 0.0 && b[1] == 0.0 { return; }
        if self.layers.get(li).is_none() { return; }
        apply_layer_xform(&mut self.layers, li, a, b);
        self.undo_log.push(UndoEntry::LayerScaled(li, a, b));
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Zoom the layer at `li` by `s` about the canvas-local point
    /// `pivot` — the wheel-cursor position, so the ink under the
    /// pointer stays put while the rest of the layer grows away from
    /// it. Same maths as the canvas-level zoom-around-cursor, folded
    /// into the `p → a · p + b` form the layer bakes.
    pub fn scale_layer_about(&mut self, li: usize, s: f32, pivot: [f32; 2]) {
        self.transform_layer(li, s, [(1.0 - s) * pivot[0], (1.0 - s) * pivot[1]]);
    }

    /// Insert a new image-backed layer at the *bottom* of the stack
    /// (index 0) so the frozen frame sits underneath every other
    /// layer's ink. Existing strokes' undo entries shift up one
    /// layer index; we clear undo/redo so a later `Ctrl+Z` doesn't
    /// rewind into the wrong layer slot.
    pub fn add_image_layer_bottom(&mut self, image: LayerImage, name: String) {
        let l = Layer {
            name,
            visible: true,
            opacity: 1.0,
            strokes: Vec::new(),
            shapes:  Vec::new(),
            image: Some(image),
            moved_by: [0.0, 0.0],
            scaled_by: 1.0,
            rev: 0,
        };
        self.layers.insert(0, l);
        // Keep the user pointing at whichever ink layer they were
        // editing — bumped up by one because we inserted underneath.
        self.active_layer = (self.active_layer + 1).min(self.layers.len() - 1);
        self.undo_log.clear();
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Insert a new blank layer directly above the active one and make
    /// it active. The new layer is named "Layer N" where N is the next
    /// integer not already in use.
    pub fn add_layer(&mut self) {
        let n = self.next_layer_number();
        let l = Layer {
            name: format!("Layer {n}"),
            visible: true,
            opacity: 1.0,
            strokes: Vec::new(),
            shapes:  Vec::new(),
            image:   None,
            moved_by: [0.0, 0.0],
            scaled_by: 1.0,
            rev: 0,
        };
        let pos = (self.active_layer + 1).min(self.layers.len());
        self.layers.insert(pos, l);
        self.active_layer = pos;
        self.dirty = true;
    }

    /// Remove the layer at `index`. Refuses to remove the last layer
    /// (keeps at least one always present). Adjusts `active_layer` so
    /// it still points at a real layer afterwards.
    pub fn delete_layer(&mut self, index: usize) {
        if self.layers.len() <= 1 || index >= self.layers.len() {
            return;
        }
        self.layers.remove(index);
        if self.active_layer >= self.layers.len() {
            self.active_layer = self.layers.len() - 1;
        } else if self.active_layer > index {
            self.active_layer -= 1;
        }
        // Edits referencing the deleted layer are dropped from the
        // history; otherwise undo would resurrect strokes onto an
        // index that no longer matches their original layer.
        let cur_len = self.layers.len();
        self.undo_log.retain(|e| layer_idx_of(e) < cur_len);
        self.redo_log.retain(|e| layer_idx_of(e) < cur_len);
        self.dirty = true;
    }

    /// Move the layer at `index` to `new_index`, clamping the
    /// destination into range. Keeps `active_layer` pointing at the
    /// same layer instance after the swap.
    pub fn move_layer(&mut self, index: usize, new_index: usize) {
        if index >= self.layers.len() { return; }
        let new_index = new_index.min(self.layers.len() - 1);
        if index == new_index { return; }
        let layer = self.layers.remove(index);
        self.layers.insert(new_index, layer);
        // Update active index so it still points at the same logical
        // layer the user was editing.
        if self.active_layer == index {
            self.active_layer = new_index;
        } else if index < self.active_layer && self.active_layer <= new_index {
            self.active_layer -= 1;
        } else if new_index <= self.active_layer && self.active_layer < index {
            self.active_layer += 1;
        }
        // History indexes are now meaningless across the swap — drop
        // them rather than chase rewrites.
        self.undo_log.clear();
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Toggle visibility of the layer at `index`.
    pub fn toggle_layer_visible(&mut self, index: usize) {
        if let Some(l) = self.layers.get_mut(index) {
            l.visible = !l.visible;
            self.dirty = true;
        }
    }

    /// Cycle the active layer up (toward the top of the stack).
    /// Stops at the top — does not wrap, so the user can hold the
    /// shortcut without accidentally jumping back to the bottom.
    pub fn next_layer(&mut self) {
        if self.active_layer + 1 < self.layers.len() {
            self.active_layer += 1;
        }
    }

    /// Cycle the active layer down (toward the bottom of the stack).
    pub fn prev_layer(&mut self) {
        if self.active_layer > 0 {
            self.active_layer -= 1;
        }
    }

    fn next_layer_number(&self) -> usize {
        let mut n = self.layers.len() + 1;
        // Probe up until we find a name nobody already uses.
        loop {
            let candidate = format!("Layer {n}");
            if !self.layers.iter().any(|l| l.name == candidate) {
                return n;
            }
            n += 1;
        }
    }
}

/// Apply `p → a · p + b` to the layer at `li` and keep its running
/// `scaled_by` / `moved_by` totals in step. Those two describe the
/// composed transform from the layer's original geometry to where it
/// sits now — composing `p → a · p + b` on top of `p → A · p + B`
/// gives `a·A` and `a·B + b`, which is what "reset layer zoom /
/// position" divides back out. Shared by the forward path and by
/// undo/redo so the bookkeeping can never drift from the geometry.
fn apply_layer_xform(layers: &mut [Layer], li: usize, a: f32, b: [f32; 2]) {
    let Some(layer) = layers.get_mut(li) else { return };
    layer.transform(a, b);
    layer.scaled_by *= a;
    layer.moved_by[0] = a * layer.moved_by[0] + b[0];
    layer.moved_by[1] = a * layer.moved_by[1] + b[1];
}

fn layer_idx_of(e: &UndoEntry) -> usize {
    match e {
        UndoEntry::StrokeAdded(li)
        | UndoEntry::ShapeAdded(li)
        | UndoEntry::StrokeErased(li, _, _)
        | UndoEntry::ShapeErased(li, _, _)
        | UndoEntry::Cleared(li, _, _)
        | UndoEntry::StrokePartiallyErased(li, _, _, _)
        | UndoEntry::ShapeReplaced(li, _, _, _)
        | UndoEntry::LayerMoved(li, _)
        | UndoEntry::LayerScaled(li, _, _)
        | UndoEntry::ObjectsReplaced(li, _, _)
        | UndoEntry::ObjectsRemoved(li, _, _)
        | UndoEntry::ObjectsInserted(li, _, _)
        | UndoEntry::ObjectsReordered(li, _, _)
        | UndoEntry::PlatePromoted(li, _) => *li,
    }
}

/// Turn layer `li`'s background plate into an image object at the front
/// of its shape list, returning the plate's `capture_zoom`. A layer
/// paints its plate first and then its shapes in order, so shape 0
/// lands exactly where the plate was, under everything else on the
/// layer — nothing on screen moves.
fn promote_plate_in(layers: &mut [Layer], li: usize) -> Option<f32> {
    let layer = layers.get_mut(li)?;
    let img = layer.image.take()?;
    let size = img.extent();
    layer.shapes.insert(0, Shape::Raster {
        pos: img.canvas_origin,
        size,
        png: std::sync::Arc::new(img.png),
        px_size: img.size,
        color: [0, 0, 0, 0],
        rotation: 0.0,
        kind: RasterKind::Image,
        media: None,
        picture: None,
    });
    layer.rev += 1;
    Some(img.capture_zoom)
}

/// Exact inverse of `promote_plate_in`. Refuses (returns false) unless
/// the layer is in the state a promotion leaves it in — no plate, and
/// an unrotated image as shape 0 — which is what the undo history
/// guarantees by the time this entry is reached.
fn demote_plate(layers: &mut [Layer], li: usize, capture_zoom: f32) -> bool {
    let Some(layer) = layers.get_mut(li) else { return false };
    let promoted = matches!(
        layer.shapes.first(),
        Some(Shape::Raster { kind: RasterKind::Image, rotation, media: None, picture: None, .. }) if *rotation == 0.0
    );
    if layer.image.is_some() || !promoted { return false; }
    let Shape::Raster { pos, size, png, px_size, .. } = layer.shapes.remove(0) else { return false };
    layer.image = Some(LayerImage {
        // Normally the only owner by now; a copy is made only if some
        // clone of the shape (clipboard, redo snapshot) still shares it.
        png: std::sync::Arc::try_unwrap(png).unwrap_or_else(|shared| (*shared).clone()),
        size: px_size,
        logical_size: size,
        canvas_origin: pos,
        capture_zoom,
    });
    layer.rev += 1;
    true
}

/// Remove the items at `indices` (ascending, unique) and return them as
/// `(index, item)` in the same ascending order — the form
/// `insert_at` takes to put them back.
fn remove_at<T>(list: &mut Vec<T>, indices: &[usize]) -> Vec<(usize, T)> {
    let mut out = Vec::with_capacity(indices.len());
    // Highest index first, so removing one never shifts those still to go.
    for &i in indices.iter().rev() {
        if i < list.len() {
            out.push((i, list.remove(i)));
        }
    }
    out.reverse();
    out
}

/// Insert `(index, item)` pairs given in ascending index order. Each
/// index is the item's position in the finished list, which is where
/// it lands because everything below it has already been placed.
fn insert_at<T: Clone>(list: &mut Vec<T>, items: &[(usize, T)]) {
    for (i, item) in items {
        list.insert((*i).min(list.len()), item.clone());
    }
}

/// Re-order `list` so that `new[i] = old[perm[i]]`. Ignored unless
/// `perm` really is a permutation of the list's indices.
fn permute<T>(list: &mut Vec<T>, perm: &[usize]) {
    if perm.len() != list.len() { return; }
    let mut seen = vec![false; perm.len()];
    for &p in perm {
        if p >= seen.len() || std::mem::replace(&mut seen[p], true) { return; }
    }
    let mut old: Vec<Option<T>> = std::mem::take(list).into_iter().map(Some).collect();
    list.extend(perm.iter().filter_map(|&p| old[p].take()));
}

fn inverse_perm(perm: &[usize]) -> Vec<usize> {
    let mut inv = vec![0; perm.len()];
    for (new_i, &old_i) in perm.iter().enumerate() {
        if old_i < inv.len() { inv[old_i] = new_i; }
    }
    inv
}

/// Undo one of the insert / remove / re-order entries and return the
/// entry that would undo *that*. Shared by `undo` and `redo`: for these
/// three, redoing is just undoing the undo.
fn invert_object_entry(layers: &mut [Layer], entry: UndoEntry) -> Option<UndoEntry> {
    match entry {
        UndoEntry::ObjectsRemoved(li, strokes, shapes) => {
            let layer = layers.get_mut(li)?;
            insert_at(&mut layer.strokes, &strokes);
            insert_at(&mut layer.shapes, &shapes);
            layer.rev += 1;
            Some(UndoEntry::ObjectsInserted(li, strokes, shapes))
        }
        UndoEntry::ObjectsInserted(li, strokes, shapes) => {
            let layer = layers.get_mut(li)?;
            let si: Vec<usize> = strokes.iter().map(|e| e.0).collect();
            let hi: Vec<usize> = shapes.iter().map(|e| e.0).collect();
            remove_at(&mut layer.strokes, &si);
            remove_at(&mut layer.shapes, &hi);
            layer.rev += 1;
            Some(UndoEntry::ObjectsRemoved(li, strokes, shapes))
        }
        UndoEntry::ObjectsReordered(li, stroke_perm, shape_perm) => {
            let layer = layers.get_mut(li)?;
            let (si, hi) = (inverse_perm(&stroke_perm), inverse_perm(&shape_perm));
            permute(&mut layer.strokes, &si);
            permute(&mut layer.shapes, &hi);
            layer.rev += 1;
            Some(UndoEntry::ObjectsReordered(li, si, hi))
        }
        _ => None,
    }
}

/// Permutation that moves the `selected` indices (ascending) of a list
/// of length `len` to its end (`to_front`, painted last = on top) or
/// its start, keeping both groups in their existing order.
fn regroup_perm(len: usize, selected: &[usize], to_front: bool) -> Vec<usize> {
    let picked: Vec<usize> = selected.iter().copied().filter(|&i| i < len).collect();
    let rest = (0..len).filter(|i| picked.binary_search(i).is_err());
    if to_front {
        rest.chain(picked.iter().copied()).collect()
    } else {
        picked.iter().copied().chain(rest).collect()
    }
}

/// True if axis-aligned rect `[x0,y0,x1,y1]` overlaps the disc at `c` with
/// `radius`. Cheap conservative test (we treat it as rect-vs-rect with the
/// disc's bounding box), which is fine for an eraser.
fn rect_contains_circle(rect: [f32; 4], c: [f32; 2], radius: f32) -> bool {
    let r = radius;
    !(rect[2] < c[0] - r || rect[0] > c[0] + r || rect[3] < c[1] - r || rect[1] > c[1] + r)
}

/// Build fragment strokes from the runs of consecutive samples in
/// `stroke` that lie OUTSIDE `polygon`. Each fragment inherits the
/// source stroke's colour, width, style, and `filled` flag.
/// Single-sample runs are skipped so we do not leave stray dots
/// behind. May return zero fragments if every sample sits inside
/// the lasso.
fn split_stroke_outside_polygon(stroke: &Stroke, polygon: &[[f32; 2]]) -> Vec<Stroke> {
    let mut out = Vec::new();
    let mut current: Vec<crate::canvas::stroke::PenSample> = Vec::new();
    let flush = |runs: &mut Vec<Stroke>, run: &mut Vec<crate::canvas::stroke::PenSample>, src: &Stroke| {
        if run.len() >= 2 {
            let mut s = Stroke::with_style(src.color, src.base_width, src.style);
            s.filled = src.filled;
            s.samples = std::mem::take(run);
            s.cache = None;
            runs.push(s);
        } else {
            run.clear();
        }
    };
    for sp in &stroke.samples {
        if point_in_polygon(sp.pos, polygon) {
            flush(&mut out, &mut current, stroke);
        } else {
            current.push(*sp);
        }
    }
    flush(&mut out, &mut current, stroke);
    out
}

/// Result of running the lasso polygon against a Raster shape.
enum RasterEraseOutcome {
    /// Polygon and raster don't overlap — leave the shape alone.
    Skip,
    /// Every visible pixel of the raster sits inside the polygon —
    /// caller should drop the shape entirely.
    Delete,
    /// Partial overlap — `Shape::Raster` with cleared pixels;
    /// caller should swap it in.
    Replace(Shape),
}

/// Build a partial-erase variant of `shape` (must be `Shape::Raster`)
/// by zeroing the alpha of every pixel whose canvas-local centre
/// lies inside `polygon`. Returns one of `Skip` / `Delete` /
/// `Replace`. Decodes + re-encodes the PNG once.
fn raster_partial_erase(shape: &Shape, polygon: &[[f32; 2]]) -> RasterEraseOutcome {
    let (pos, size, png, color, rotation, kind) = match shape {
        Shape::Raster { pos, size, png, color, rotation, kind, .. } => {
            (*pos, *size, png, *color, *rotation, *kind)
        }
        _ => return RasterEraseOutcome::Skip,
    };
    // The pixel grid below is axis-aligned in the image's own frame. For
    // a turned image, turn the lasso back by the same angle about the
    // image centre instead, so the rest of this function can keep
    // treating the raster as unrotated.
    let unturned: Vec<[f32; 2]>;
    let polygon: &[[f32; 2]] = if rotation != 0.0 {
        let o = shape.obb();
        unturned = polygon
            .iter()
            .map(|p| {
                let q = o.to_local(*p);
                [o.center[0] + q[0], o.center[1] + q[1]]
            })
            .collect();
        &unturned
    } else {
        polygon
    };
    let decoded = match image::load_from_memory(png) {
        Ok(d) => d.to_rgba8(),
        Err(_) => return RasterEraseOutcome::Skip,
    };
    let iw = decoded.width();
    let ih = decoded.height();
    if iw == 0 || ih == 0 || size[0] <= 0.0 || size[1] <= 0.0 {
        return RasterEraseOutcome::Skip;
    }

    // Polygon bounding box in canvas-local coords.
    let mut pxmin = f32::INFINITY;
    let mut pymin = f32::INFINITY;
    let mut pxmax = f32::NEG_INFINITY;
    let mut pymax = f32::NEG_INFINITY;
    for p in polygon {
        if p[0] < pxmin { pxmin = p[0]; }
        if p[1] < pymin { pymin = p[1]; }
        if p[0] > pxmax { pxmax = p[0]; }
        if p[1] > pymax { pymax = p[1]; }
    }

    // Intersect polygon bbox with raster bbox; bail early if empty.
    let cx0 = pxmin.max(pos[0]);
    let cy0 = pymin.max(pos[1]);
    let cx1 = pxmax.min(pos[0] + size[0]);
    let cy1 = pymax.min(pos[1] + size[1]);
    if cx0 >= cx1 || cy0 >= cy1 {
        return RasterEraseOutcome::Skip;
    }

    let sx = iw as f32 / size[0];
    let sy = ih as f32 / size[1];
    let x0 = (((cx0 - pos[0]) * sx).floor().max(0.0) as u32).min(iw);
    let y0 = (((cy0 - pos[1]) * sy).floor().max(0.0) as u32).min(ih);
    let x1 = (((cx1 - pos[0]) * sx).ceil()  as u32).min(iw);
    let y1 = (((cy1 - pos[1]) * sy).ceil()  as u32).min(ih);

    let mut output = decoded.clone();
    let mut any_modified = false;
    for py in y0..y1 {
        let cp_y = pos[1] + (py as f32 + 0.5) / sy;
        for px in x0..x1 {
            let cp_x = pos[0] + (px as f32 + 0.5) / sx;
            if point_in_polygon([cp_x, cp_y], polygon) {
                let pixel = output.get_pixel_mut(px, py);
                if pixel.0[3] != 0 {
                    pixel.0 = [0, 0, 0, 0];
                    any_modified = true;
                }
            }
        }
    }
    if !any_modified {
        return RasterEraseOutcome::Skip;
    }
    let any_remaining = output.pixels().any(|p| p.0[3] != 0);
    if !any_remaining {
        return RasterEraseOutcome::Delete;
    }
    let mut new_png: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut new_png);
        if output.write_to(&mut cursor, image::ImageFormat::Png).is_err() {
            return RasterEraseOutcome::Skip;
        }
    }
    RasterEraseOutcome::Replace(Shape::Raster {
        pos,
        size,
        png: std::sync::Arc::new(new_png),
        px_size: [iw, ih],
        color,
        rotation,
        kind,
        media: None,
        picture: None,
    })
}

/// Classic horizontal-ray crossing test. Returns true if `p` lies
/// inside `polygon` (treated as a closed loop — the last vertex
/// implicitly connects back to the first). Stable for any vertex
/// winding order. Robust for lasso-erase hit-testing where the user
/// path is rough; we don't try to handle exact-on-edge ties because
/// real centroids almost never land on a path edge.
pub(crate) fn point_in_polygon(p: [f32; 2], polygon: &[[f32; 2]]) -> bool {
    let n = polygon.len();
    if n < 3 { return false; }
    let (x, y) = (p[0], p[1]);
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (polygon[i][0], polygon[i][1]);
        let (xj, yj) = (polygon[j][0], polygon[j][1]);
        let crosses = (yi > y) != (yj > y)
            && x < (xj - xi) * (y - yi) / (yj - yi + f32::EPSILON) + xi;
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod layer_xform_tests {
    use super::*;
    use crate::canvas::stroke::{PenSample, Stroke, StrokeStyle};

    fn canvas_with_dot(x: f32, y: f32) -> Canvas {
        let mut c = Canvas::default();
        let mut s = Stroke::with_style([255, 0, 0, 255], 8.0, StrokeStyle::Default);
        s.samples.push(PenSample { pos: [x, y], pressure: 1.0, tilt: [0.0, 0.0] });
        c.layers[0].strokes.push(s);
        c
    }

    fn dot(c: &Canvas) -> [f32; 2] { c.layers[0].strokes[0].samples[0].pos }
    fn width(c: &Canvas) -> f32 { c.layers[0].strokes[0].base_width }

    #[test]
    fn scale_about_pivot_pins_the_pivot_and_scales_width() {
        let mut c = canvas_with_dot(30.0, 10.0);
        c.scale_layer_about(0, 2.0, [10.0, 10.0]);
        // Distance from the pivot doubles; the pivot itself is fixed.
        assert!((dot(&c)[0] - 50.0).abs() < 1e-3);
        assert!((dot(&c)[1] - 10.0).abs() < 1e-3);
        assert!((width(&c) - 16.0).abs() < 1e-3);
        assert!((c.layers[0].scaled_by - 2.0).abs() < 1e-6);
    }

    #[test]
    fn undo_redo_restores_geometry() {
        let mut c = canvas_with_dot(30.0, 10.0);
        c.scale_layer_about(0, 2.5, [4.0, -7.0]);
        c.undo();
        assert!((dot(&c)[0] - 30.0).abs() < 1e-3);
        assert!((dot(&c)[1] - 10.0).abs() < 1e-3);
        assert!((width(&c) - 8.0).abs() < 1e-3);
        assert!((c.layers[0].scaled_by - 1.0).abs() < 1e-6);
        c.redo();
        assert!((dot(&c)[0] - 69.0).abs() < 1e-3);
        assert!((c.layers[0].scaled_by - 2.5).abs() < 1e-6);
    }

    #[test]
    fn reset_zoom_then_position_lands_on_the_original() {
        // Mixed history: pan, zoom about one pivot, pan again, zoom
        // about another. Resetting scale (about any pivot) and then
        // translation must return the exact original geometry — that
        // invariant is what `scaled_by` / `moved_by` exist to keep.
        let mut c = canvas_with_dot(30.0, 10.0);
        c.move_layer_by(0, [12.0, -3.0]);
        c.scale_layer_about(0, 1.5, [0.0, 40.0]);
        c.move_layer_by(0, [-5.0, 9.0]);
        c.scale_layer_about(0, 0.8, [100.0, 100.0]);

        let a = c.layers[0].scaled_by;
        c.scale_layer_about(0, 1.0 / a, [17.0, 23.0]);
        let m = c.layers[0].moved_by;
        c.move_layer_by(0, [-m[0], -m[1]]);

        assert!((dot(&c)[0] - 30.0).abs() < 1e-2, "x = {}", dot(&c)[0]);
        assert!((dot(&c)[1] - 10.0).abs() < 1e-2, "y = {}", dot(&c)[1]);
        assert!((width(&c) - 8.0).abs() < 1e-2);
        assert!((c.layers[0].scaled_by - 1.0).abs() < 1e-4);
    }

    #[test]
    fn identity_transform_writes_no_history() {
        let mut c = canvas_with_dot(30.0, 10.0);
        c.scale_layer_about(0, 1.0, [10.0, 10.0]);
        assert!(c.undo_log.is_empty());
        // A mirroring / collapsing factor is refused outright.
        c.scale_layer_about(0, -1.0, [10.0, 10.0]);
        assert!(c.undo_log.is_empty());
        assert!((dot(&c)[0] - 30.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod object_edit_tests {
    use super::*;
    use crate::canvas::shape::BorderStyle;
    use crate::canvas::stroke::{PenSample, StrokeStyle};
    use std::sync::Arc;

    /// A short horizontal stroke starting at `(x, 0)`, tagged by its
    /// red channel so tests can tell strokes apart after re-ordering.
    fn stroke(tag: u8, x: f32) -> Stroke {
        let mut s = Stroke::with_style([tag, 0, 0, 255], 4.0, StrokeStyle::Default);
        for i in 0..=4 {
            s.samples.push(PenSample { pos: [x + i as f32 * 10.0, 0.0], pressure: 1.0, tilt: [0.0, 0.0] });
        }
        s.build_cache();
        s
    }

    fn raster(kind: RasterKind) -> Shape {
        Shape::Raster {
            pos: [0.0, 0.0], size: [200.0, 200.0], png: Arc::new(vec![1, 2, 3]), px_size: [1, 1],
            color: [0; 4], rotation: 0.0, kind, media: None,
            picture: None,
        }
    }

    fn tags(c: &Canvas) -> Vec<u8> {
        c.layers[0].strokes.iter().map(|s| s.color[0]).collect()
    }

    fn canvas(n: u8) -> Canvas {
        let mut c = Canvas::default();
        for t in 0..n {
            c.layers[0].strokes.push(stroke(t, t as f32 * 100.0));
        }
        c
    }

    #[test]
    fn delete_is_one_step_and_undo_puts_everything_back_in_place() {
        let mut c = canvas(5);
        c.delete_objects(0, &[1, 3], &[]);
        assert_eq!(tags(&c), vec![0, 2, 4]);
        assert_eq!(c.undo_log.len(), 1);
        c.undo();
        assert_eq!(tags(&c), vec![0, 1, 2, 3, 4]);
        c.redo();
        assert_eq!(tags(&c), vec![0, 2, 4]);
        c.undo();
        assert_eq!(tags(&c), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn insert_reports_where_things_landed_and_undoes_cleanly() {
        let mut c = canvas(2);
        let placed = c.insert_objects(0, vec![stroke(7, 0.0), stroke(8, 0.0)], vec![raster(RasterKind::Image)]);
        assert_eq!(placed.strokes, vec![2, 3]);
        assert_eq!(placed.shapes, vec![0]);
        assert_eq!(tags(&c), vec![0, 1, 7, 8]);
        c.undo();
        assert_eq!(tags(&c), vec![0, 1]);
        assert!(c.layers[0].shapes.is_empty());
        c.redo();
        assert_eq!(tags(&c), vec![0, 1, 7, 8]);
        assert_eq!(c.layers[0].shapes.len(), 1);
    }

    #[test]
    fn bring_to_front_and_send_to_back_keep_relative_order() {
        let mut c = canvas(5);
        let placed = c.reorder_objects(0, &[1, 3], &[], true);
        assert_eq!(tags(&c), vec![0, 2, 4, 1, 3]);
        assert_eq!(placed.strokes, vec![3, 4]);
        c.undo();
        assert_eq!(tags(&c), vec![0, 1, 2, 3, 4]);
        c.redo();
        assert_eq!(tags(&c), vec![0, 2, 4, 1, 3]);

        let placed = c.reorder_objects(0, &[3, 4], &[], false);
        assert_eq!(tags(&c), vec![1, 3, 0, 2, 4]);
        assert_eq!(placed.strokes, vec![0, 1]);

        // Already at the back: nothing to do, nothing recorded.
        let before = c.undo_log.len();
        c.reorder_objects(0, &[0, 1], &[], false);
        assert_eq!(c.undo_log.len(), before);
    }

    #[test]
    fn a_run_of_size_ticks_is_one_undo_step() {
        let mut c = canvas(2);
        c.restyle_objects(0, &[0], &[], None, Some(6.0), false);
        c.restyle_objects(0, &[0], &[], None, Some(9.0), true);
        c.restyle_objects(0, &[0], &[], None, Some(12.0), true);
        assert_eq!(c.undo_log.len(), 1);
        assert!((c.layers[0].strokes[0].base_width - 12.0).abs() < 1e-4);
        // The cached widths follow, so the stroke repaints at the new size.
        let w = c.layers[0].strokes[0].cache.as_ref().unwrap().widths[0];
        assert!(w > 4.0 * 2.0, "cache width {w} did not grow");
        c.undo();
        assert!((c.layers[0].strokes[0].base_width - 4.0).abs() < 1e-4);
        c.redo();
        assert!((c.layers[0].strokes[0].base_width - 12.0).abs() < 1e-4);
    }

    #[test]
    fn merging_never_folds_into_an_edit_of_different_objects() {
        let mut c = canvas(2);
        c.restyle_objects(0, &[0], &[], None, Some(6.0), false);
        c.restyle_objects(0, &[1], &[], None, Some(9.0), true);
        assert_eq!(c.undo_log.len(), 2);
    }

    #[test]
    fn recolour_and_transform_undo_restore_a_rotated_shape_exactly() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(Shape::Rect {
            a: [0.0, 0.0], b: [40.0, 20.0], color: [1, 2, 3, 255], stroke_width: 2.0,
            border: BorderStyle::Solid, rotation: 0.0,
        });
        c.transform_objects(0, &[], &[0], &Affine::rotate_about([20.0, 10.0], 0.5));
        c.restyle_objects(0, &[], &[0], Some([9, 9, 9, 255]), None, false);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Rect { rotation, color, .. }
            if (rotation - 0.5).abs() < 1e-5 && *color == [9, 9, 9, 255]));
        c.undo();
        c.undo();
        assert!(matches!(&c.layers[0].shapes[0], Shape::Rect { a, b, rotation, color, .. }
            if *a == [0.0, 0.0] && *b == [40.0, 20.0] && *rotation == 0.0 && *color == [1, 2, 3, 255]));
    }

    #[test]
    fn identity_transform_writes_no_history() {
        let mut c = canvas(1);
        c.transform_objects(0, &[0], &[], &Affine::IDENTITY);
        assert!(c.undo_log.is_empty());
    }

    #[test]
    fn eraser_needs_to_touch_the_ink_and_never_takes_an_image() {
        let mut c = Canvas::default();
        // Diagonal stroke: its bounding box is 100×100, the ink a thin line.
        let mut diagonal = Stroke::with_style([0, 0, 0, 255], 4.0, StrokeStyle::Default);
        for i in 0..=10 {
            let v = i as f32 * 10.0;
            diagonal.samples.push(PenSample { pos: [v, v], pressure: 1.0, tilt: [0.0, 0.0] });
        }
        diagonal.build_cache();
        c.layers[0].strokes.push(diagonal);
        c.layers[0].shapes.push(raster(RasterKind::Image));

        // Inside the stroke's box and on top of the image, but off the ink.
        assert!(!c.erase_at([90.0, 10.0], 6.0));
        assert_eq!(c.layers[0].strokes.len(), 1);
        assert_eq!(c.layers[0].shapes.len(), 1);

        // On the ink: the stroke goes, the image underneath stays.
        assert!(c.erase_at([50.0, 50.0], 6.0));
        assert!(c.layers[0].strokes.is_empty());
        assert_eq!(c.layers[0].shapes.len(), 1);

        // A fill sticker is still ink and still erasable.
        c.layers[0].shapes.push(raster(RasterKind::Fill));
        assert!(c.erase_at([50.0, 50.0], 6.0));
        assert_eq!(c.layers[0].shapes.len(), 1);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { kind: RasterKind::Image, .. }));
    }

    #[test]
    fn the_lasso_eraser_takes_ink_and_leaves_pictures_whole() {
        let mut c = canvas(1);
        c.layers[0].shapes.push(raster(RasterKind::Image));
        let around_everything = [[-50.0, -50.0], [300.0, -50.0], [300.0, 300.0], [-50.0, 300.0]];
        assert!(c.erase_in_polygon(&around_everything));
        assert!(c.layers[0].strokes.is_empty());
        assert_eq!(c.layers[0].shapes.len(), 1, "the image survives");
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { png, .. } if **png == vec![1, 2, 3]));
    }

    #[test]
    fn promoting_a_plate_keeps_earlier_undo_entries_pointing_at_the_right_shapes() {
        let mut c = Canvas::default();
        c.layers[0].image = Some(LayerImage {
            png: vec![9, 9],
            size: [4, 2],
            logical_size: [0.0, 0.0], // an older save: no logical size
            canvas_origin: [10.0, 20.0],
            capture_zoom: 1.0,
        });
        let line = |x: f32| Shape::Line {
            a: [x, 0.0], b: [x, 50.0], color: [0; 4], stroke_width: 2.0, border: BorderStyle::Solid,
        };
        c.add_shape(line(1.0));
        c.add_shape(line(2.0));
        assert_eq!(c.promote_plate(0), Some(0));
        // The image sits where the plate was drawn: origin, pixel size.
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { pos, size, kind: RasterKind::Image, .. }
            if *pos == [10.0, 20.0] && *size == [4.0, 2.0]));
        assert_eq!(c.layers[0].shapes.len(), 3);

        // Undo steps through the promotion and removes the second line —
        // not the image, and not the first line.
        c.undo();
        assert!(c.layers[0].image.is_some());
        assert_eq!(c.layers[0].shapes.len(), 1);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Line { a, .. } if a[0] == 1.0));
        // Redo brings the line back; the promotion had no visible edit
        // after it to ride along with, so it waits for the next redo.
        c.redo();
        assert!(c.layers[0].image.is_some());
        assert_eq!(c.layers[0].shapes.len(), 2);
        c.redo();
        assert!(c.layers[0].image.is_none());
        assert_eq!(c.layers[0].shapes.len(), 3);
        assert!(matches!(&c.layers[0].shapes[2], Shape::Line { a, .. } if a[0] == 2.0));
        // And back again: every index still lines up.
        c.undo();
        c.undo();
        assert!(c.layers[0].image.is_some());
        assert!(c.layers[0].shapes.is_empty());
    }
}
