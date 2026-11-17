//! Undo and redo, and the steps they replay: layer transforms, plates
//! promoted and put back, objects removed, inserted and reordered.

use super::*;

impl Canvas {
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
            UndoEntry::Resynced(layers, link) => {
                let swapped = self.swap_resynced(layers, link);
                self.redo_log.push(swapped);
            }
        }
    }

    /// Put a resync's result in place (`media::pureref_sync`) as one undo
    /// step: these layers' new shape lists and the slide's new board link.
    pub fn apply_resync(&mut self, layers: Vec<(usize, Vec<Shape>)>, link: crate::canvas::board_link::BoardLink) {
        let entry = self.swap_resynced(layers, Some(Box::new(link)));
        self.undo_log.push(entry);
        self.redo_log.clear();
        self.dirty = true;
    }

    /// Put these shape lists and this board link in place, and return the
    /// `Resynced` step that puts back what they replaced.
    pub(crate) fn swap_resynced(&mut self, layers: Vec<(usize, Vec<Shape>)>, link: Option<Box<crate::canvas::board_link::BoardLink>>) -> UndoEntry {
        let mut out = Vec::with_capacity(layers.len());
        for (li, shapes) in layers {
            if let Some(layer) = self.layers.get_mut(li) {
                out.push((li, std::mem::replace(&mut layer.shapes, shapes)));
                layer.rev += 1;
            }
        }
        let link = std::mem::replace(&mut self.board, link);
        UndoEntry::Resynced(out, link)
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
            UndoEntry::Resynced(layers, link) => {
                let swapped = self.swap_resynced(layers, link);
                self.undo_log.push(swapped);
            }
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
pub(super) fn apply_layer_xform(layers: &mut [Layer], li: usize, a: f32, b: [f32; 2]) {
    let Some(layer) = layers.get_mut(li) else { return };
    layer.transform(a, b);
    layer.scaled_by *= a;
    layer.moved_by[0] = a * layer.moved_by[0] + b[0];
    layer.moved_by[1] = a * layer.moved_by[1] + b[1];
}

pub(super) fn layer_idx_of(e: &UndoEntry) -> usize {
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
        UndoEntry::Resynced(layers, _) => layers.first().map_or(0, |(li, _)| *li),
    }
}

/// Turn layer `li`'s background plate into an image object at the front
/// of its shape list, returning the plate's `capture_zoom`. A layer
/// paints its plate first and then its shapes in order, so shape 0
/// lands exactly where the plate was, under everything else on the
/// layer — nothing on screen moves.
pub(super) fn promote_plate_in(layers: &mut [Layer], li: usize) -> Option<f32> {
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
        pin: 0,
        flip: false,
        group: 0,
        tags: Vec::new(),
        turn: 0.0,
        link: 0,
        paper: None,
    });
    layer.rev += 1;
    Some(img.capture_zoom)
}

/// Exact inverse of `promote_plate_in`. Refuses (returns false) unless
/// the layer is in the state a promotion leaves it in — no plate, and
/// an unrotated image as shape 0 — which is what the undo history
/// guarantees by the time this entry is reached.
pub(super) fn demote_plate(layers: &mut [Layer], li: usize, capture_zoom: f32) -> bool {
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
pub(super) fn remove_at<T>(list: &mut Vec<T>, indices: &[usize]) -> Vec<(usize, T)> {
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
pub(super) fn insert_at<T: Clone>(list: &mut Vec<T>, items: &[(usize, T)]) {
    for (i, item) in items {
        list.insert((*i).min(list.len()), item.clone());
    }
}

/// Re-order `list` so that `new[i] = old[perm[i]]`. Ignored unless
/// `perm` really is a permutation of the list's indices.
pub(super) fn permute<T>(list: &mut Vec<T>, perm: &[usize]) {
    if perm.len() != list.len() { return; }
    let mut seen = vec![false; perm.len()];
    for &p in perm {
        if p >= seen.len() || std::mem::replace(&mut seen[p], true) { return; }
    }
    let mut old: Vec<Option<T>> = std::mem::take(list).into_iter().map(Some).collect();
    list.extend(perm.iter().filter_map(|&p| old[p].take()));
}

pub(super) fn inverse_perm(perm: &[usize]) -> Vec<usize> {
    let mut inv = vec![0; perm.len()];
    for (new_i, &old_i) in perm.iter().enumerate() {
        if old_i < inv.len() { inv[old_i] = new_i; }
    }
    inv
}

/// Undo one of the insert / remove / re-order entries and return the
/// entry that would undo *that*. Shared by `undo` and `redo`: for these
/// three, redoing is just undoing the undo.
pub(super) fn invert_object_entry(layers: &mut [Layer], entry: UndoEntry) -> Option<UndoEntry> {
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
pub(super) fn regroup_perm(len: usize, selected: &[usize], to_front: bool) -> Vec<usize> {
    let picked: Vec<usize> = selected.iter().copied().filter(|&i| i < len).collect();
    let rest = (0..len).filter(|i| picked.binary_search(i).is_err());
    if to_front {
        rest.chain(picked.iter().copied()).collect()
    } else {
        picked.iter().copied().chain(rest).collect()
    }
}
