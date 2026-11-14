//! The Select tool's selection: what it holds, and the edits made to it as a whole —
//! arrange, flip, group, restyle, text boxes, a picture's colours, copies to other slides.

use super::*;

impl OnScreenNotesApp {
    /// Switch to the Select tool with exactly the shape at `index` on
    /// the active layer selected.
    pub(super) fn select_only_shape(&mut self, index: usize) {
        use crate::canvas::hit::ObjRef;
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel.set_only(ObjRef::Shape(index));
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Clones of the selected objects on the active layer.
    pub(super) fn selected_objects(&self) -> (Vec<crate::canvas::stroke::Stroke>, Vec<crate::canvas::shape::Shape>) {
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return (Vec::new(), Vec::new()) };
        let sel = &self.tool.select.sel;
        (
            sel.strokes.iter().filter_map(|&i| layer.strokes.get(i).cloned()).collect(),
            sel.shapes.iter().filter_map(|&i| layer.shapes.get(i).cloned()).collect(),
        )
    }

    /// Add `strokes` / `shapes` to the active layer, shifted by `offset`
    /// canvas pixels, as one undo step, and select them.
    pub(super) fn insert_and_select(
        &mut self,
        mut strokes: Vec<crate::canvas::stroke::Stroke>,
        mut shapes: Vec<crate::canvas::shape::Shape>,
        offset: [f32; 2],
    ) {
        if strokes.is_empty() && shapes.is_empty() { return; }
        // Copies of grouped objects make groups of their own.
        crate::canvas::canvas::regroup_copies(&mut strokes, &mut shapes);
        // Copies are in no pin: a pin shows its own objects only.
        for s in &mut strokes {
            s.translate(offset);
            s.pin = 0;
        }
        for s in &mut shapes {
            s.translate(offset);
            s.set_pin(0);
            // A copy of a video or GIF plays on its own.
            if let crate::canvas::shape::Shape::Raster { media: Some(m), .. } = s {
                m.id = crate::canvas::shape::fresh_media_id();
            }
        }
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        let placed = c.insert_objects(li, strokes, shapes);
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel = placed;
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Select every object the media filter highlights. A selection
    /// lives on one layer: the active layer's matches, or, when it has
    /// none, those of the topmost visible layer that does.
    pub(super) fn select_media_matches(&mut self) {
        let c = self.canvases.active();
        let visible = |li: usize| c.layers.get(li).is_some_and(|l| l.visible && l.opacity > 0.0);
        let found = std::iter::once(c.active_layer)
            .chain((0..c.layers.len()).rev().filter(|&li| li != c.active_layer))
            .filter(|&li| visible(li))
            .map(|li| (li, ui::media_filter::matches_on(c, li, self.media_filter)))
            .find(|(_, shapes)| !shapes.is_empty());
        let Some((li, shapes)) = found else { return };
        if li != c.active_layer {
            self.canvases.active_mut().active_layer = li;
        }
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel = crate::canvas::hit::Selection { strokes: Vec::new(), shapes };
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Copy the selected objects onto a new slide and go there, with the
    /// copies selected, at the same spot and in the same view. The slide
    /// goes after the last one: putting it in mid-deck would renumber,
    /// and so rewrite, every slide file after it.
    pub(super) fn copy_selection_to_new_slide(&mut self) {
        let (strokes, shapes) = self.selected_objects();
        let count = strokes.len() + shapes.len();
        if count == 0 { return; }
        let (pan, zoom) = { let c = self.canvases.active(); (c.pan, c.zoom) };
        let (canvas, sel) = crate::canvas::canvas::Canvas::with_objects(strokes, shapes, pan, zoom);
        let idx = self.canvases.push_canvas(canvas);
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel = sel;
        self.tool.select.rebind(idx, self.canvases.active());
        let what = if count == 1 { "1 object".to_string() } else { format!("{count} objects") };
        self.toast = Some((format!("Copied {what} to new slide {}", idx + 1), Instant::now()));
    }

    /// The Select tool was moving a selection on the canvas and let go
    /// over the slide window at `screen` (physical pixels; `ppp` the
    /// canvas's pixels per point): the objects move to the window's slide,
    /// landing under the pointer at the same size on screen, and are
    /// selected there. True when the release was such a drop (done or
    /// refused), so it is not the Select tool's.
    pub(super) fn drop_on_slide_window(&mut self, screen: [f32; 2], ppp: f32) -> bool {
        use crate::tools::select_tool::Gesture;
        let Some(Gesture::Move { start, .. }) = self.tool.select.gesture else { return false };
        if self.tool.kind != crate::tools::ToolKind::Select || self.tool.select.sel.is_empty() {
            return false;
        }
        let Some(win) = self.slide_window.as_mut() else { return false };
        if win.hwnd() == 0 || crate::platform::top_level_window_at(screen[0] as i32, screen[1] as i32) != win.hwnd() {
            return false;
        }
        let Some((to, target, window_scale)) = win.canvas_at(screen) else { return false };
        self.tool.select.gesture = None;
        if self.canvases.is_locked(to) {
            self.toast = Some((format!("Slide {} is locked — unlock it to move objects there", to + 1), Instant::now()));
            return true;
        }
        let from = self.canvases.active_index();
        let (li, zoom) = { let c = self.canvases.active(); (c.active_layer, c.zoom) };
        let m = ui::slide_edit::drop_mapping(start, target, zoom * ppp / window_scale.max(1e-6));
        let sel = std::mem::take(&mut self.tool.select.sel);
        match self.canvases.drop_objects(from, to, li, &sel.strokes, &sel.shapes, &m) {
            Some(placed) => {
                let n = placed.strokes.len() + placed.shapes.len();
                win.select_dropped(placed, &self.canvases);
                if from != to {
                    let what = if n == 1 { "1 object".to_string() } else { format!("{n} objects") };
                    self.toast = Some((format!("Moved {what} to slide {}", to + 1), Instant::now()));
                }
            }
            None => self.tool.select.sel = sel,
        }
        true
    }

    /// Move the selected objects to slide `to`, where they show now (see
    /// `CanvasManager::move_objects_to`), and say so. The selection goes
    /// with them: nothing is left here to select.
    pub(super) fn move_selection_to(&mut self, to: usize) {
        if !self.may_edit() {
            return;
        }
        if self.canvases.is_locked(to) {
            self.toast = Some((format!("Slide {} is locked — unlock it to move objects there", to + 1), Instant::now()));
            return;
        }
        let li = self.canvases.active().active_layer;
        let sel = std::mem::take(&mut self.tool.select.sel);
        self.tool.select.gesture = None;
        let count = self.canvases.move_objects_to(to, li, &sel.strokes, &sel.shapes);
        if count == 0 {
            self.tool.select.sel = sel;
            return;
        }
        let what = if count == 1 { "1 object".to_string() } else { format!("{count} objects") };
        self.toast = Some((format!("Moved {what} to slide {}", to + 1), Instant::now()));
    }

    /// Pack, group, align or distribute the selection, as one undo step.
    pub(super) fn arrange_selection(&mut self, op: crate::tools::arrange::Arrange, ctx: &egui::Context) {
        if op == crate::tools::arrange::Arrange::Pack {
            self.pack_selection(ctx);
            return;
        }
        let c = self.canvases.active();
        let li = c.active_layer;
        let Some(layer) = c.layers.get(li) else { return };
        let (strokes, shapes) = crate::tools::arrange::plan(layer, &self.tool.select.sel, op, c.zoom);
        if !self.canvases.active_mut().move_objects_each(li, &strokes, &shapes) {
            self.toast = Some(("Nothing to move".to_string(), std::time::Instant::now()));
        }
    }

    /// Pack the selection the way the arrange bar's arrow says, onto the
    /// screen: inside the view when it fits there at its size, as near
    /// where it was as it can be; else into a block shaped like the view,
    /// which the view glides out to show whole. One undo step.
    pub(super) fn pack_selection(&mut self, ctx: &egui::Context) {
        let area = self.fit_area(ctx);
        let c = self.canvases.active();
        let li = c.active_layer;
        let Some(layer) = c.layers.get(li) else { return };
        let (pan, zoom) = (c.pan, c.zoom.max(1e-3));
        let view = [
            (area[0] - pan[0]) / zoom,
            (area[1] - pan[1]) / zoom,
            (area[0] + area[2] - pan[0]) / zoom,
            (area[1] + area[3] - pan[1]) / zoom,
        ];
        let (strokes, shapes, block) = crate::tools::arrange::plan_pack(layer, &self.tool.select.sel, self.config.pack_mode, view, zoom);
        let moved = self.canvases.active_mut().move_objects_each(li, &strokes, &shapes);
        // Half a screen pixel of slack for f32 rounding.
        let slack = 0.5 / zoom;
        let on_screen = block[0] >= view[0] - slack && block[1] >= view[1] - slack && block[2] <= view[2] + slack && block[3] <= view[3] + slack;
        if !on_screen {
            let to = self.view_fitting(block, zoom, ctx);
            self.glide_view(to, ctx);
        } else if !moved {
            self.toast = Some(("Nothing to move".to_string(), std::time::Instant::now()));
        }
    }

    /// Canvas box around the Select tool's selection, if there is one.
    pub(super) fn selection_bounds(&self) -> Option<[f32; 4]> {
        if self.tool.kind != crate::tools::ToolKind::Select || self.tool.select.sel.is_empty() {
            return None;
        }
        let c = self.canvases.active();
        let layer = c.layers.get(c.active_layer)?;
        let sel = &self.tool.select.sel;
        sel.strokes
            .iter()
            .filter_map(|&i| layer.strokes.get(i)?.bounds())
            .chain(sel.shapes.iter().filter_map(|&i| layer.shapes.get(i).map(crate::canvas::canvas::shape_bounds)))
            .reduce(union_box)
    }

    /// Mirror the selection left to right, or top to bottom, about the
    /// middle of its box: one undo step. Pictures flip as they are drawn;
    /// text moves but keeps reading left to right.
    pub(super) fn flip_selection(&mut self, vertical: bool) {
        let Some(b) = self.selection_bounds() else { return };
        if !self.may_edit() { return; }
        let mid = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
        let m = if vertical {
            crate::canvas::affine::Affine::scale_about(mid, 1.0, -1.0)
        } else {
            crate::canvas::affine::Affine::scale_about(mid, -1.0, 1.0)
        };
        let sel = self.tool.select.sel.clone();
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        c.transform_objects(li, &sel.strokes, &sel.shapes, &m);
    }

    /// Selected text labels, with the Select tool.
    pub(super) fn selected_text(&self) -> Vec<usize> {
        if self.tool.kind != crate::tools::ToolKind::Select {
            return Vec::new();
        }
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return Vec::new() };
        self.tool.select.sel.shapes.iter().copied().filter(|&i| matches!(layer.shapes.get(i), Some(Shape::Text { .. }))).collect()
    }

    /// The text-box switch was flipped: the selected labels follow it.
    pub(super) fn apply_text_box_to_selection(&mut self) {
        let texts = self.selected_text();
        if texts.is_empty() || !self.may_edit() { return; }
        let on = self.tool.text_box;
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        c.set_text_box(li, &texts, on);
    }

    /// `(slide, layer, shape)` of the selected picture, when the
    /// selection is one picture (or video, or GIF) and nothing else.
    pub(super) fn selected_picture(&self) -> Option<(usize, usize, usize)> {
        let sel = &self.tool.select.sel;
        if self.tool.kind != crate::tools::ToolKind::Select || !sel.strokes.is_empty() || sel.shapes.len() != 1 {
            return None;
        }
        let c = self.canvases.active();
        let i = sel.shapes[0];
        let picture = matches!(
            c.layers.get(c.active_layer)?.shapes.get(i)?,
            Shape::Raster { kind: crate::canvas::shape::RasterKind::Image, .. }
        );
        picture.then_some((self.canvases.active_index(), c.active_layer, i))
    }

    /// Show the selected picture's colours in the arrange bar, or hide
    /// them. A video or GIF gives the colours of the frame it shows.
    pub(super) fn toggle_picture_colours(&mut self) {
        let Some(at) = self.selected_picture() else { return };
        if self.picture_colours.as_ref().is_some_and(|p| p.at == at) {
            self.picture_colours = None;
            return;
        }
        let c = self.canvases.active();
        let Some(Shape::Raster { png, media, .. }) = c.layers.get(at.1).and_then(|l| l.shapes.get(at.2)) else { return };
        let frame = media.as_ref().and_then(|m| self.media.current_frames().get(&m.id).cloned());
        let img = match frame {
            Some(f) => {
                let bytes: Vec<u8> = f.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
                image::RgbaImage::from_raw(f.size[0] as u32, f.size[1] as u32, bytes)
            }
            None => image::load_from_memory(png).ok().map(|i| i.to_rgba8()),
        };
        let Some(img) = img else {
            self.toast = Some(("Couldn't read this picture's colours".to_string(), Instant::now()));
            return;
        };
        let k = self.config.palette.len().max(1);
        let colours = crate::tools::picture_colours::picture_colours(&img, k);
        self.picture_colours = Some(PictureColours { at, colours });
    }

    /// The picture's colours become the toolbar's palette, the same
    /// number of swatches as before (so the colour keys keep their
    /// places); the old palette can come back with "Restore palette".
    pub(super) fn keep_picture_colours(&mut self) {
        let Some(p) = &self.picture_colours else { return };
        if p.colours.is_empty() { return; }
        // Fewer colours than swatches: the rest keep what they had.
        let new: Vec<[u8; 4]> = if self.config.palette.is_empty() {
            p.colours.clone()
        } else {
            self.config.palette.iter().enumerate().map(|(i, old)| p.colours.get(i).copied().unwrap_or(*old)).collect()
        };
        let before = std::mem::replace(&mut self.config.palette, new);
        if self.palette_before.is_none() {
            self.palette_before = Some(before);
        }
        self.toast = Some(("These colours are the palette now".to_string(), Instant::now()));
    }

    pub(super) fn restore_palette(&mut self) {
        if let Some(before) = self.palette_before.take() {
            self.config.palette = before;
            self.toast = Some(("Palette restored".to_string(), Instant::now()));
        }
    }

    /// Give the selection one width or height, as one undo step.
    pub(super) fn match_selection_size(&mut self, m: crate::tools::arrange::Match) {
        let c = self.canvases.active();
        let li = c.active_layer;
        let Some(layer) = c.layers.get(li) else { return };
        let (strokes, shapes) = crate::tools::arrange::plan_sizes(layer, &self.tool.select.sel, m, c.zoom);
        if !self.canvases.active_mut().transform_objects_each(li, &strokes, &shapes) {
            self.toast = Some(("Already that size".to_string(), std::time::Instant::now()));
        }
    }

    /// Move the selection `px` screen pixels: real ones, so a 1 px nudge
    /// is the smallest step the screen shows, at any zoom. Nudges less
    /// than a second apart on the same objects are one undo step.
    pub(super) fn nudge_selection(&mut self, px: [f32; 2], pixels_per_point: f32) {
        const RUN_GAP: std::time::Duration = std::time::Duration::from_secs(1);
        let idx = self.canvases.active_index();
        let sel = self.tool.select.sel.clone();
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        let per_px = 1.0 / (c.zoom.max(1e-3) * pixels_per_point.max(1e-3));
        let now = Instant::now();
        let merge = self
            .nudge_run
            .is_some_and(|(i, len, at)| i == idx && len == c.undo_log.len() && now - at < RUN_GAP);
        if c.nudge_objects(li, &sel.strokes, &sel.shapes, [px[0] * per_px, px[1] * per_px], merge) {
            self.nudge_run = Some((idx, c.undo_log.len(), now));
        }
    }

    /// Some selected object is in a group.
    pub(super) fn selection_grouped(&self) -> bool {
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return false };
        let sel = &self.tool.select.sel;
        sel.strokes.iter().any(|&i| layer.strokes.get(i).is_some_and(|s| s.group != 0))
            || sel.shapes.iter().any(|&i| layer.shapes.get(i).is_some_and(|s| s.group() != 0))
    }

    /// Make the selection one group (`on`), or take it out of its groups:
    /// one undo step. The selection stays as it is.
    pub(super) fn group_selection(&mut self, on: bool) {
        let sel = self.tool.select.sel.clone();
        let n = sel.strokes.len() + sel.shapes.len();
        if on && n < 2 {
            self.toast = Some(("Select two or more objects to group them".to_string(), Instant::now()));
            return;
        }
        if n == 0 || !self.may_edit() {
            return;
        }
        let id = if on { crate::canvas::shape::fresh_group_id() } else { 0 };
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        let changed = c.set_group(li, &sel.strokes, &sel.shapes, id);
        let msg = match (on, changed) {
            (true, _) => format!("Grouped {n} objects — Alt+click picks one of them"),
            (false, true) => "Ungrouped".to_string(),
            (false, false) => "Nothing here is grouped".to_string(),
        };
        self.toast = Some((msg, Instant::now()));
    }

    /// Every selected object is in a pin.
    pub(super) fn selection_pinned(&self) -> bool {
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return false };
        let sel = &self.tool.select.sel;
        !sel.is_empty()
            && sel.strokes.iter().all(|&i| layer.strokes.get(i).is_some_and(|s| s.pin != 0))
            && sel.shapes.iter().all(|&i| layer.shapes.get(i).is_some_and(|s| s.pin() != 0))
    }

    /// An eyedropper pick becomes the pen colour and also replaces the
    /// toolbar swatch that held the old pen colour, so the picked colour
    /// stays one click away. With no such swatch only the pen changes.
    pub(super) fn take_picked_color(&mut self, c: [u8; 4]) {
        let old = self.tool.color;
        if let Some(slot) = self.config.palette.iter_mut().find(|s| **s == old) {
            *slot = c;
        }
        self.tool.color = c;
    }

    /// Apply a toolbar colour / size change to the selected objects.
    /// `prev_*` are the tool's values before the toolbar ran this frame.
    pub(super) fn restyle_selection(&mut self, prev_color: [u8; 4], prev_size: f32) {
        if self.tool.kind != crate::tools::ToolKind::Select || self.tool.select.sel.is_empty() {
            return;
        }
        let color = (self.tool.color != prev_color).then_some(self.tool.color);
        let width = ((self.tool.size - prev_size).abs() > 1e-4).then_some(self.tool.size);
        if color.is_none() && width.is_none() { return; }
        let idx = self.canvases.active_index();
        let sel = self.tool.select.sel.clone();
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        // Fold a run of size ticks into the entry the first one made.
        let merge = color.is_none() && self.restyle_run == Some((idx, c.undo_log.len()));
        c.restyle_objects(li, &sel.strokes, &sel.shapes, color, width, merge);
        self.restyle_run = (width.is_some() && color.is_none()).then_some((idx, c.undo_log.len()));
    }
}
