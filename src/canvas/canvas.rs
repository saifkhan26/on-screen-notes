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
use crate::canvas::board_link::BoardLink;
use crate::canvas::hit;
use crate::canvas::shape::{RasterKind, Shape};
use crate::canvas::stroke::Stroke;
use serde::{Deserialize, Serialize};

mod erase;
mod history;
mod layer;
mod layers;
mod objects;

pub(crate) use erase::*;
use history::*;
pub use layer::*;

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

    /// The PureRef board the slide was opened from, kept so it can be
    /// resynced (`media::pureref_sync`). Not written when there is none,
    /// so other slides keep their old on-disk form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<Box<BoardLink>>,

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
    /// A resync from PureRef changed the slide: each `(layer, shapes)` is
    /// a layer's whole shape list as it was, and the link is the slide's
    /// board link as it was. Undo swaps them with what is there now, and
    /// the swapped-out state is the redo — the same step either way.
    /// Pictures share their bytes, so the lists are cheap to keep.
    Resynced(Vec<(usize, Vec<Shape>)>, Option<Box<BoardLink>>),
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
            board: None,
            undo_log: Vec::new(),
            redo_log: Vec::new(),
            // A freshly created canvas has no file behind it yet.
            dirty: true,
        }
    }
}

impl Canvas {
    /// A new slide holding copies of `strokes` and `shapes` where they sat,
    /// seen through the same `pan` / `zoom`, and the selection of all of
    /// them. They are its starting content, not an undo step. Videos and
    /// GIFs get fresh player ids, so each copy plays on its own, and the
    /// copies are in no pin: a pin shows its own objects only.
    pub fn with_objects(mut strokes: Vec<Stroke>, mut shapes: Vec<Shape>, pan: [f32; 2], zoom: f32) -> (Self, hit::Selection) {
        for s in &mut strokes {
            s.build_cache();
            s.pin = 0;
        }
        for s in &mut shapes {
            if let Shape::Raster { media: Some(m), .. } = s {
                m.id = crate::canvas::shape::fresh_media_id();
            }
            s.set_pin(0);
        }
        regroup_copies(&mut strokes, &mut shapes);
        let sel = hit::Selection { strokes: (0..strokes.len()).collect(), shapes: (0..shapes.len()).collect() };
        let mut canvas = Self { pan, zoom, ..Self::default() };
        canvas.layers[0].strokes = strokes;
        canvas.layers[0].shapes = shapes;
        (canvas, sel)
    }

    /// Migrate an old-format canvas (flat strokes/shapes, no layers) into
    /// the new layered representation. Idempotent: re-runs are no-ops.
    /// Always called after deserialise and after default-construction so
    /// the rest of the code can assume `layers` is non-empty.
    pub fn ensure_migrated(&mut self) {
        if self.layers.is_empty() {
            let l = Layer {
                strokes: std::mem::take(&mut self.legacy_strokes),
                shapes: std::mem::take(&mut self.legacy_shapes),
                ..Layer::default()
            };
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
    /// Changes whenever the slide's saved content does: the undo and
    /// redo history, the active layer, and each layer's object counts
    /// and edit `rev`. See `CanvasManager::edit_active`.
    pub fn edit_stamp(&self) -> (usize, usize, usize, Vec<(usize, usize, u64)>) {
        (
            self.undo_log.len(),
            self.redo_log.len(),
            self.active_layer,
            self.layers.iter().map(|l| (l.strokes.len(), l.shapes.len(), l.rev)).collect(),
        )
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
                UndoEntry::Resynced(layers, _) => {
                    layers.iter().flat_map(|(_, shapes)| shapes).for_each(&mut add);
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
    fn boxing_labels_is_one_undo_step_and_skips_other_shapes() {
        let mut c = Canvas::default();
        c.add_shape(Shape::Text { pos: [0.0, 0.0], content: "a".into(), font_size: 20.0, color: [255; 4], rotation: 0.0, pin: 0, background: None, group: 0, tags: Vec::new(), turn: 0.0, link: 0 });
        c.add_shape(Shape::Rect { a: [0.0, 0.0], b: [5.0, 5.0], color: [0; 4], stroke_width: 1.0, border: crate::canvas::shape::BorderStyle::Solid, rotation: 0.0, pin: 0, group: 0, tags: Vec::new(), turn: 0.0 });
        let steps = c.undo_log.len();
        assert!(c.set_text_box(0, &[0, 1], true));
        assert_eq!(c.undo_log.len(), steps + 1);
        assert!(matches!(c.layers[0].shapes[0], Shape::Text { background: Some(_), .. }));
        assert!(!c.set_text_box(0, &[0, 1], true), "already boxed: nothing to do");
        c.undo();
        assert!(matches!(c.layers[0].shapes[0], Shape::Text { background: None, .. }));
    }

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
            pin: 0,
            flip: false,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
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
    fn a_slide_made_from_objects_holds_copies_in_place_and_selected() {
        use crate::canvas::shape::{MediaKind, MediaRef};
        let video = |id: u64| {
            let mut s = raster(RasterKind::Image);
            if let Shape::Raster { media, .. } = &mut s {
                *media = Some(Box::new(MediaRef { file: "v.mp4".into(), kind: MediaKind::Video, source_name: String::new(), meta: None, id }));
            }
            s
        };
        let (c, sel) = Canvas::with_objects(vec![stroke(7, 30.0)], vec![raster(RasterKind::Image), video(42)], [5.0, -3.0], 0.25);
        assert_eq!((c.pan, c.zoom), ([5.0, -3.0], 0.25), "seen as it was on the old slide");
        assert_eq!(sel, hit::Selection { strokes: vec![0], shapes: vec![0, 1] });
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [30.0, 0.0], "copies stay where they were");
        assert!(c.undo_log.is_empty(), "the copies are the slide's content, not an undo step");
        assert!(c.dirty, "a new slide must be saved");
        let id = c.layers[0].shapes[1].media().map(|m| m.id);
        assert!(id.is_some_and(|id| id != 42), "the copied video plays on its own");
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
    fn restyling_while_zoomed_sizes_lines_on_screen() {
        let mut c = canvas(2);
        c.layers[0].strokes[1].style = StrokeStyle::Marker;
        c.zoom = 2.0;
        c.restyle_objects(0, &[0, 1], &[], None, Some(6.0), false);
        assert!((c.layers[0].strokes[0].base_width - 3.0).abs() < 1e-4, "a pen line looks 6 px wide at 2×");
        assert!((c.layers[0].strokes[1].base_width - 3.0).abs() < 1e-4, "so does a marker line");
    }

    #[test]
    fn a_run_of_nudges_is_one_undo_step() {
        let mut c = canvas(2);
        assert!(c.nudge_objects(0, &[0], &[], [1.0, 0.0], false));
        assert!(c.nudge_objects(0, &[0], &[], [1.0, 0.0], true));
        assert!(c.nudge_objects(0, &[0], &[], [0.0, -10.0], true));
        assert_eq!(c.undo_log.len(), 1);
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [2.0, -10.0]);
        assert_eq!(c.layers[0].strokes[1].samples[0].pos, [100.0, 0.0], "only the chosen stroke moves");
        c.undo();
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [0.0, 0.0]);
        c.redo();
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [2.0, -10.0]);
        // Other objects, or none at all: a step of their own.
        assert!(c.nudge_objects(0, &[1], &[], [1.0, 0.0], true));
        assert_eq!(c.undo_log.len(), 2);
        assert!(!c.nudge_objects(0, &[1], &[], [0.0, 0.0], true));
        assert_eq!(c.undo_log.len(), 2);
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
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
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
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
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
