//! Edits to a set of objects, each one undo step: what the Select tool, the
//! arrange bar and the tag and pin commands do.

use super::*;

impl Canvas {
    // -- selection edits --------------------------------------------------
    //
    // Everything the Select tool does to a set of objects. Each call is
    // one undo step. Index slices are expected ascending and unique
    // (`hit::Selection` keeps them that way); indices past the end of
    // a list are skipped rather than trusted.

    /// Run `edit_stroke` / `edit_shape` over the chosen objects on layer
    /// `li`, recording before / after snapshots as one undo entry.
    /// Returns false when nothing was touched.
    pub(super) fn edit_objects(
        &mut self,
        li: usize,
        strokes: &[usize],
        shapes: &[usize],
        mut edit_stroke: impl FnMut(usize, &mut Stroke),
        mut edit_shape: impl FnMut(usize, &mut Shape),
    ) -> bool {
        let Some(layer) = self.layers.get_mut(li) else { return false };
        let mut stroke_log = Vec::with_capacity(strokes.len());
        let mut shape_log = Vec::with_capacity(shapes.len());
        for &i in strokes {
            if let Some(s) = layer.strokes.get_mut(i) {
                let before = s.clone();
                edit_stroke(i, s);
                stroke_log.push((i, before, s.clone()));
            }
        }
        for &i in shapes {
            if let Some(s) = layer.shapes.get_mut(i) {
                let before = s.clone();
                edit_shape(i, s);
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
        self.edit_objects(li, strokes, shapes, |_, s| s.apply_affine(m), |_, s| s.apply_affine(m));
    }

    /// Shift each chosen object by its own offset, all in one undo step
    /// — what Arrange does. `strokes` / `shapes` pair an index with its
    /// offset, ascending by index. Zero offsets are left out of the
    /// step; returns false when nothing moved.
    pub fn move_objects_each(&mut self, li: usize, strokes: &[(usize, [f32; 2])], shapes: &[(usize, [f32; 2])]) -> bool {
        let moved = |list: &[(usize, [f32; 2])]| -> Vec<(usize, [f32; 2])> {
            list.iter().copied().filter(|(_, d)| d[0] != 0.0 || d[1] != 0.0).collect()
        };
        let (strokes, shapes) = (moved(strokes), moved(shapes));
        let offset = |list: &[(usize, [f32; 2])], i: usize| {
            list.binary_search_by_key(&i, |e| e.0).map_or([0.0, 0.0], |k| list[k].1)
        };
        let stroke_ix: Vec<usize> = strokes.iter().map(|e| e.0).collect();
        let shape_ix: Vec<usize> = shapes.iter().map(|e| e.0).collect();
        self.edit_objects(
            li,
            &stroke_ix,
            &shape_ix,
            |i, s| s.translate(offset(&strokes, i)),
            |i, s| s.translate(offset(&shapes, i)),
        )
    }

    /// Transform each chosen object by its own `Affine`, all in one undo
    /// step — what Arrange's size matching does. Laid out like
    /// `move_objects_each`; identity transforms are left out of the
    /// step, and it returns false when nothing changed.
    pub fn transform_objects_each(&mut self, li: usize, strokes: &[(usize, Affine)], shapes: &[(usize, Affine)]) -> bool {
        let changed = |list: &[(usize, Affine)]| -> Vec<(usize, Affine)> {
            list.iter().copied().filter(|(_, m)| !m.is_identity()).collect()
        };
        let (strokes, shapes) = (changed(strokes), changed(shapes));
        let transform = |list: &[(usize, Affine)], i: usize| {
            list.binary_search_by_key(&i, |e| e.0).map_or(Affine::IDENTITY, |k| list[k].1)
        };
        let stroke_ix: Vec<usize> = strokes.iter().map(|e| e.0).collect();
        let shape_ix: Vec<usize> = shapes.iter().map(|e| e.0).collect();
        self.edit_objects(
            li,
            &stroke_ix,
            &shape_ix,
            |i, s| s.apply_affine(&transform(&strokes, i)),
            |i, s| s.apply_affine(&transform(&shapes, i)),
        )
    }

    /// Put the chosen objects in pin `id`, or take them out of any pin
    /// with `id = 0` (see `canvas::pins`). One undo step; objects that
    /// already are so are left out of it. Returns false when nothing
    /// changed.
    pub fn set_pin(&mut self, li: usize, strokes: &[usize], shapes: &[usize], id: u64) -> bool {
        let Some(layer) = self.layers.get(li) else { return false };
        let strokes: Vec<usize> = strokes.iter().copied().filter(|&i| layer.strokes.get(i).is_some_and(|s| s.pin != id)).collect();
        let shapes: Vec<usize> = shapes.iter().copied().filter(|&i| layer.shapes.get(i).is_some_and(|s| s.pin() != id)).collect();
        self.edit_objects(li, &strokes, &shapes, |_, s| s.pin = id, |_, s| s.set_pin(id))
    }

    /// Put the chosen objects in group `id`, or take them out of any
    /// group with `id = 0`. One undo step; objects already so are left
    /// out of it. Returns false when nothing changed.
    pub fn set_group(&mut self, li: usize, strokes: &[usize], shapes: &[usize], id: u64) -> bool {
        let Some(layer) = self.layers.get(li) else { return false };
        let strokes: Vec<usize> = strokes.iter().copied().filter(|&i| layer.strokes.get(i).is_some_and(|s| s.group != id)).collect();
        let shapes: Vec<usize> = shapes.iter().copied().filter(|&i| layer.shapes.get(i).is_some_and(|s| s.group() != id)).collect();
        self.edit_objects(li, &strokes, &shapes, |_, s| s.group = id, |_, s| s.set_group(id))
    }

    /// Give each chosen object its own new tags, all in one undo step —
    /// what renaming or removing a tag everywhere does. Laid out like
    /// `move_objects_each`, ascending by index. Returns false when
    /// nothing changed.
    pub fn set_tags_each(&mut self, li: usize, strokes: &[(usize, Vec<String>)], shapes: &[(usize, Vec<String>)]) -> bool {
        let tags_of = |list: &[(usize, Vec<String>)], i: usize| {
            list.binary_search_by_key(&i, |e| e.0).map(|k| list[k].1.clone()).unwrap_or_default()
        };
        let stroke_ix: Vec<usize> = strokes.iter().map(|e| e.0).collect();
        let shape_ix: Vec<usize> = shapes.iter().map(|e| e.0).collect();
        self.edit_objects(li, &stroke_ix, &shape_ix, |i, s| s.tags = tags_of(strokes, i), |i, s| s.set_tags(&tags_of(shapes, i)))
    }

    /// Give the chosen objects these tags in place of their own (see
    /// `shape::parse_tags`). One undo step; objects that have them
    /// already are left out of it. Returns false when nothing changed.
    pub fn set_tags(&mut self, li: usize, strokes: &[usize], shapes: &[usize], tags: &[String]) -> bool {
        let Some(layer) = self.layers.get(li) else { return false };
        let strokes: Vec<usize> = strokes.iter().copied().filter(|&i| layer.strokes.get(i).is_some_and(|s| s.tags != tags)).collect();
        let shapes: Vec<usize> = shapes.iter().copied().filter(|&i| layer.shapes.get(i).is_some_and(|s| s.tags() != tags)).collect();
        self.edit_objects(li, &strokes, &shapes, |_, s| s.tags = tags.to_vec(), |_, s| s.set_tags(tags))
    }

    /// Put the chosen text labels in a box that keeps them readable
    /// (`shape::auto_text_box`), or take them out of any box. One undo
    /// step; other shapes, and labels already so, are left out of it.
    /// Returns false when nothing changed.
    pub fn set_text_box(&mut self, li: usize, shapes: &[usize], on: bool) -> bool {
        let Some(layer) = self.layers.get(li) else { return false };
        let shapes: Vec<usize> = shapes
            .iter()
            .copied()
            .filter(|&i| matches!(layer.shapes.get(i), Some(Shape::Text { background, .. }) if background.is_some() != on))
            .collect();
        let boxed = |_: usize, s: &mut Shape| {
            if let Shape::Text { background, color, .. } = s {
                *background = on.then(|| crate::canvas::shape::auto_text_box(*color));
            }
        };
        self.edit_objects(li, &[], &shapes, |_, _| {}, boxed)
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
        let zoom = self.zoom.max(1e-3);
        let edit_stroke = |s: &mut Stroke| {
            if let Some(c) = color { s.color = c; }
            // Lines take the size as it looks on screen, as when they
            // are drawn.
            if let Some(w) = width {
                s.set_base_width(w / zoom);
            }
        };
        let edit_shape = |s: &mut Shape| s.restyle(color, width);

        if merge_into_last && self.edit_into_last(li, strokes, shapes, &edit_stroke, &edit_shape) {
            return;
        }
        self.edit_objects(li, strokes, shapes, |_, s| edit_stroke(s), |_, s| edit_shape(s));
    }

    /// Shift the chosen objects by `d`, as one undo step: the Select
    /// tool's nudge keys. `merge_into_last` folds it into the undo entry
    /// on top of the history instead, as `restyle_objects` does, so a
    /// held key goes back in one Ctrl+Z. Returns false when nothing
    /// moved.
    pub fn nudge_objects(&mut self, li: usize, strokes: &[usize], shapes: &[usize], d: [f32; 2], merge_into_last: bool) -> bool {
        if d == [0.0, 0.0] {
            return false;
        }
        if merge_into_last && self.edit_into_last(li, strokes, shapes, |s| s.translate(d), |s| s.translate(d)) {
            return true;
        }
        self.edit_objects(li, strokes, shapes, |_, s| s.translate(d), |_, s| s.translate(d))
    }

    /// Run `edit_stroke` / `edit_shape` over the chosen objects as part of
    /// the undo entry on top of the history, provided that entry is an
    /// in-place edit of exactly these objects. Returns false, having
    /// changed nothing, when it is not.
    pub(super) fn edit_into_last(
        &mut self,
        li: usize,
        strokes: &[usize],
        shapes: &[usize],
        mut edit_stroke: impl FnMut(&mut Stroke),
        mut edit_shape: impl FnMut(&mut Shape),
    ) -> bool {
        let (Some(layer), Some(UndoEntry::ObjectsReplaced(l, stroke_log, shape_log))) =
            (self.layers.get_mut(li), self.undo_log.last_mut())
        else {
            return false;
        };
        let same = *l == li
            && stroke_log.iter().map(|e| e.0).eq(strokes.iter().copied())
            && shape_log.iter().map(|e| e.0).eq(shapes.iter().copied());
        if !same {
            return false;
        }
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
        true
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
}
