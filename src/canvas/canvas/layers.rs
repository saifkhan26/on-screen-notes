//! Layer management: moving and zooming a layer's content, adding, deleting,
//! reordering, showing and stepping through layers.

use super::*;

impl Canvas {
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

    pub(super) fn next_layer_number(&self) -> usize {
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
