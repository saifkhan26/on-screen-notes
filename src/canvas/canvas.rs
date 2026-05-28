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

use crate::canvas::{shape::Shape, stroke::Stroke};
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
}

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
        }
    }
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
    }

    /// Append a shape and record an undo entry.
    pub fn add_shape(&mut self, shape: Shape) {
        let li = self.active_layer;
        self.layers[li].shapes.push(shape);
        self.undo_log.push(UndoEntry::ShapeAdded(li));
        self.redo_log.clear();
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
    }

    /// Undo the most recent edit if any. Pushes inverse onto redo log.
    pub fn undo(&mut self) {
        let Some(entry) = self.undo_log.pop() else { return };
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
        }
    }

    /// Redo the most recently undone edit if any.
    pub fn redo(&mut self) {
        let Some(entry) = self.redo_log.pop() else { return };
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
        }
    }

    /// Erase all strokes/shapes on the **active** layer whose bounds
    /// contain `point` (canvas-local). Returns true if anything was
    /// erased. Strokes / shapes on other layers are left alone, even
    /// when they sit visually under the eraser tip.
    pub fn erase_at(&mut self, point: [f32; 2], radius: f32) -> bool {
        let li = self.active_layer;
        let mut erased_any = false;
        let layer = &mut self.layers[li];
        // Strokes: walk in reverse so indices stay valid as we remove.
        let mut i = layer.strokes.len();
        while i > 0 {
            i -= 1;
            if let Some(b) = layer.strokes[i].bounds() {
                if rect_contains_circle(b, point, radius) {
                    let s = layer.strokes.remove(i);
                    self.undo_log.push(UndoEntry::StrokeErased(li, i, s));
                    erased_any = true;
                }
            }
        }
        let mut i = layer.shapes.len();
        while i > 0 {
            i -= 1;
            let bb = match &layer.shapes[i] {
                Shape::Rect    { a, b, stroke_width, .. }
                | Shape::Ellipse { a, b, stroke_width, .. }
                | Shape::Line    { a, b, stroke_width, .. }
                | Shape::Arrow   { a, b, stroke_width, .. } => {
                    let pad = *stroke_width;
                    [
                        a[0].min(b[0]) - pad,
                        a[1].min(b[1]) - pad,
                        a[0].max(b[0]) + pad,
                        a[1].max(b[1]) + pad,
                    ]
                }
                Shape::Text { pos, content, font_size, .. } => {
                    // Coarse bounding box: each line is at most
                    // `font_size * 0.6 * len_chars` wide, and there are
                    // (newline count + 1) lines. Overestimates a bit so
                    // a wide eraser still hits the text. Good enough
                    // for hit-testing — the actual layout happens at
                    // render time.
                    let lines: Vec<&str> = content.split('\n').collect();
                    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1) as f32;
                    let w = (font_size * 0.6 * max_chars).max(*font_size);
                    let h = (font_size * 1.25 * lines.len() as f32).max(*font_size);
                    [pos[0], pos[1], pos[0] + w, pos[1] + h]
                }
                Shape::Raster { pos, size, .. } => {
                    [pos[0], pos[1], pos[0] + size[0], pos[1] + size[1]]
                }
            };
            if rect_contains_circle(bb, point, radius) {
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
            // Raster shapes get a per-pixel partial erase so the
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
            let bb = match &layer.shapes[i] {
                Shape::Rect    { a, b, .. }
                | Shape::Ellipse { a, b, .. }
                | Shape::Line    { a, b, .. }
                | Shape::Arrow   { a, b, .. } => {
                    [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
                }
                Shape::Text { pos, content, font_size, .. } => {
                    let lines: Vec<&str> = content.split('\n').collect();
                    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1) as f32;
                    let w = (font_size * 0.6 * max_chars).max(*font_size);
                    let h = (font_size * 1.25 * lines.len() as f32).max(*font_size);
                    [pos[0], pos[1], pos[0] + w, pos[1] + h]
                }
                Shape::Raster { .. } => unreachable!("Raster handled above"),
            };
            let center = [(bb[0] + bb[2]) * 0.5, (bb[1] + bb[3]) * 0.5];
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

    // -- layer management -------------------------------------------------

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
        };
        self.layers.insert(0, l);
        // Keep the user pointing at whichever ink layer they were
        // editing — bumped up by one because we inserted underneath.
        self.active_layer = (self.active_layer + 1).min(self.layers.len() - 1);
        self.undo_log.clear();
        self.redo_log.clear();
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
        };
        let pos = (self.active_layer + 1).min(self.layers.len());
        self.layers.insert(pos, l);
        self.active_layer = pos;
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
    }

    /// Toggle visibility of the layer at `index`.
    pub fn toggle_layer_visible(&mut self, index: usize) {
        if let Some(l) = self.layers.get_mut(index) {
            l.visible = !l.visible;
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

fn layer_idx_of(e: &UndoEntry) -> usize {
    match e {
        UndoEntry::StrokeAdded(li)
        | UndoEntry::ShapeAdded(li)
        | UndoEntry::StrokeErased(li, _, _)
        | UndoEntry::ShapeErased(li, _, _)
        | UndoEntry::Cleared(li, _, _)
        | UndoEntry::StrokePartiallyErased(li, _, _, _)
        | UndoEntry::ShapeReplaced(li, _, _, _) => *li,
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
    let (pos, size, png, _px_size, color) = match shape {
        Shape::Raster { pos, size, png, px_size, color } => (*pos, *size, png, *px_size, *color),
        _ => return RasterEraseOutcome::Skip,
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
        png: new_png,
        px_size: [iw, ih],
        color,
    })
}

/// Classic horizontal-ray crossing test. Returns true if `p` lies
/// inside `polygon` (treated as a closed loop — the last vertex
/// implicitly connects back to the first). Stable for any vertex
/// winding order. Robust for lasso-erase hit-testing where the user
/// path is rough; we don't try to handle exact-on-edge ties because
/// real centroids almost never land on a path edge.
fn point_in_polygon(p: [f32; 2], polygon: &[[f32; 2]]) -> bool {
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
