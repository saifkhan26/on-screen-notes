//! "Are you sure?" for the big edits — deleting a layer that has something
//! on it, clearing one, deleting a large selection — and the window every
//! such question uses, "Delete this slide?" included. Each of these edits
//! is also one undo step, so the question is a guard, not the only way back.

use super::*;
use crate::canvas::canvas::Layer;
use crate::canvas::shape::RasterKind;
use crate::input::hotkeys_config::Binding;

/// From this many objects up, deleting a selection asks first. Fewer go at
/// once (the undo key still brings them back).
pub(super) const BIG_DELETE: usize = 20;

/// A big edit waiting on its "are you sure?". Each names the slide it was
/// asked on: if another slide is shown by the time the answer comes, the
/// edit is dropped rather than done to the wrong one.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PendingEdit {
    DeleteLayer { slide: usize, layer: usize },
    ClearLayer { slide: usize, layer: usize },
    DeleteObjects { slide: usize, layer: usize, strokes: Vec<usize>, shapes: Vec<usize> },
}

/// How many objects a layer holds, and how many of them are pictures. Its
/// frozen frame, if any, counts as one picture.
pub(super) fn layer_counts(l: &Layer) -> (usize, usize) {
    let image = usize::from(l.image.is_some());
    let pictures = l.shapes.iter().filter(|s| matches!(s, Shape::Raster { kind: RasterKind::Image, .. })).count();
    (l.strokes.len() + l.shapes.len() + image, pictures + image)
}

/// "12 objects, 3 of them pictures" (no full stop).
pub(super) fn objects_text(objects: usize, pictures: usize) -> String {
    let n = if objects == 1 { "1 object".to_string() } else { format!("{objects} objects") };
    match pictures {
        0 => n,
        p if p == objects && p == 1 => "1 picture".to_string(),
        p if p == objects => format!("{p} pictures"),
        1 => format!("{n}, 1 of them a picture"),
        p => format!("{n}, {p} of them pictures"),
    }
}

/// The question window: `title`, `body`, a muted `note` under it, a yes
/// button called `yes` and "Keep it". Enter, or the `again` key (the one
/// that asked), says yes; Esc says no. `None` while it waits.
pub(super) fn ask(ctx: &egui::Context, id: &str, title: &str, body: &str, note: &str, yes: &str, again: Option<Binding>) -> Option<bool> {
    let mut decision: Option<bool> = None;
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            decision = Some(false);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            || again.is_some_and(|b| i.consume_shortcut(&egui::KeyboardShortcut::new(b.to_modifiers(), b.key)))
        {
            decision = Some(true);
        }
    });
    egui::Window::new(title)
        .id(egui::Id::new(id))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_max_width(340.0);
            ui.label(body);
            ui.label(egui::RichText::new(note).small().color(ui::theme::TEXT_MUTED));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = egui::RichText::new(yes).color(ui::theme::accent_ink());
                if ui.add(egui::Button::new(label).fill(ui::theme::accent())).clicked() {
                    decision = Some(true);
                }
                if ui.button("Keep it").clicked() {
                    decision = Some(false);
                }
            });
            ui.add_space(2.0);
            ui.label(egui::RichText::new(format!("Enter: {} · Esc: keep it", yes.to_lowercase())).small().color(ui::theme::TEXT_MUTED));
        });
    decision
}

impl OnScreenNotesApp {
    /// The undo key as the user sees it ("Ctrl+Z" unless rebound).
    pub(super) fn undo_key(&self) -> String {
        self.hotkeys_cfg.canvas.undo.map_or_else(|| "Undo".to_string(), |b| b.label())
    }

    /// Delete layer `layer` of the active slide: at once when it is empty,
    /// else once confirmed.
    pub(super) fn request_layer_delete(&mut self, layer: usize) {
        if !self.may_edit() {
            return;
        }
        let c = self.canvases.active();
        let Some(l) = c.layers.get(layer) else { return };
        if c.layers.len() <= 1 {
            self.toast = Some(("The last layer stays".to_string(), Instant::now()));
            return;
        }
        if layer_counts(l).0 == 0 {
            self.delete_layer_now(layer);
        } else {
            self.ws.confirm_edit = Some(PendingEdit::DeleteLayer { slide: self.canvases.active_index(), layer });
        }
    }

    fn delete_layer_now(&mut self, layer: usize) {
        self.canvases.active_mut().delete_layer(layer);
        // The selection named objects by their place on a layer that may
        // be gone, or no longer the active one.
        self.ws.tool.select.sel.clear();
        self.ws.restyle_run = None;
        self.ws.nudge_run = None;
        let msg = format!("Layer {} deleted — {} brings it back", layer + 1, self.undo_key());
        self.toast = Some((msg, Instant::now()));
    }

    /// Clear the active layer, once confirmed. An empty one needs nothing.
    pub(super) fn request_layer_clear(&mut self) {
        if !self.may_edit() {
            return;
        }
        let c = self.canvases.active();
        let layer = c.active_layer;
        let Some(l) = c.layers.get(layer) else { return };
        if l.strokes.is_empty() && l.shapes.is_empty() {
            self.toast = Some(("Nothing on this layer to clear".to_string(), Instant::now()));
            return;
        }
        self.ws.confirm_edit = Some(PendingEdit::ClearLayer { slide: self.canvases.active_index(), layer });
    }

    /// Delete the selected objects on the active layer: at once when there
    /// are few, else once confirmed.
    pub(super) fn request_objects_delete(&mut self, strokes: Vec<usize>, shapes: Vec<usize>) {
        if strokes.len() + shapes.len() >= BIG_DELETE {
            let c = self.canvases.active();
            self.ws.confirm_edit = Some(PendingEdit::DeleteObjects { slide: self.canvases.active_index(), layer: c.active_layer, strokes, shapes });
            return;
        }
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        c.delete_objects(li, &strokes, &shapes);
        self.ws.tool.select.sel.clear();
    }

    /// The question for `confirm_edit`, while one waits, and the edit once
    /// it is answered yes.
    pub(super) fn show_edit_confirm(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.ws.confirm_edit.clone() else { return };
        let slide = match &pending {
            PendingEdit::DeleteLayer { slide, .. } | PendingEdit::ClearLayer { slide, .. } | PendingEdit::DeleteObjects { slide, .. } => *slide,
        };
        // Another slide is shown now (a click on the slide panel): drop it.
        if slide != self.canvases.active_index() {
            self.ws.confirm_edit = None;
            return;
        }
        let undo = self.undo_key();
        let c = self.canvases.active();
        let kc = &self.hotkeys_cfg.canvas;
        let asked = match &pending {
            PendingEdit::DeleteLayer { layer, .. } => {
                let Some(l) = c.layers.get(*layer) else {
                    self.ws.confirm_edit = None;
                    return;
                };
                let (objects, pictures) = layer_counts(l);
                ask(
                    ctx,
                    "osn_confirm_delete_layer",
                    "Delete this layer?",
                    &format!("Layer {} holds {}.", layer + 1, objects_text(objects, pictures)),
                    &format!("{undo} brings it back until you close the app."),
                    "Delete layer",
                    kc.delete_layer,
                )
            }
            PendingEdit::ClearLayer { layer, .. } => {
                let Some(l) = c.layers.get(*layer) else {
                    self.ws.confirm_edit = None;
                    return;
                };
                let (objects, pictures) = layer_counts(l);
                // A frozen frame stays: clearing takes the objects only.
                let objects = objects - usize::from(l.image.is_some());
                let pictures = pictures - usize::from(l.image.is_some());
                ask(
                    ctx,
                    "osn_confirm_clear_layer",
                    "Clear this layer?",
                    &format!("Everything on layer {} goes: {}.", layer + 1, objects_text(objects, pictures)),
                    &format!("{undo} brings it back until you close the app."),
                    "Clear layer",
                    kc.clear_layer,
                )
            }
            PendingEdit::DeleteObjects { layer, strokes, shapes, .. } => {
                let pictures = c
                    .layers
                    .get(*layer)
                    .map_or(0, |l| shapes.iter().filter(|&&i| matches!(l.shapes.get(i), Some(Shape::Raster { kind: RasterKind::Image, .. }))).count());
                let n = strokes.len() + shapes.len();
                ask(
                    ctx,
                    "osn_confirm_delete_objects",
                    &format!("Delete {n} objects?"),
                    &format!("The selection holds {}.", objects_text(n, pictures)),
                    &format!("{undo} brings them back until you close the app."),
                    "Delete",
                    kc.delete_selection,
                )
            }
        };
        match asked {
            Some(true) => {
                self.ws.confirm_edit = None;
                self.do_pending_edit(pending);
            }
            Some(false) => self.ws.confirm_edit = None,
            None => {}
        }
    }

    /// Carry out an edit the user said yes to, if it still fits what is
    /// there: the layer still exists, the selection is still the same.
    fn do_pending_edit(&mut self, pending: PendingEdit) {
        if !self.may_edit() {
            return;
        }
        match pending {
            PendingEdit::DeleteLayer { layer, .. } => {
                if layer < self.canvases.active().layers.len() {
                    self.delete_layer_now(layer);
                }
            }
            PendingEdit::ClearLayer { layer, .. } => {
                let c = self.canvases.active_mut();
                if c.active_layer == layer {
                    c.clear();
                    self.ws.tool.select.sel.clear();
                    let msg = format!("Layer {} cleared — {} brings it back", layer + 1, self.undo_key());
                    self.toast = Some((msg, Instant::now()));
                }
            }
            PendingEdit::DeleteObjects { layer, strokes, shapes, .. } => {
                let sel = &self.ws.tool.select.sel;
                let same = self.canvases.active().active_layer == layer && sel.strokes == strokes && sel.shapes == shapes;
                if !same {
                    self.toast = Some(("The selection changed meanwhile — nothing deleted".to_string(), Instant::now()));
                    return;
                }
                let n = strokes.len() + shapes.len();
                self.canvases.active_mut().delete_objects(layer, &strokes, &shapes);
                self.ws.tool.select.sel.clear();
                let msg = format!("{n} objects deleted — {} brings them back", self.undo_key());
                self.toast = Some((msg, Instant::now()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_naturally() {
        assert_eq!(objects_text(1, 0), "1 object");
        assert_eq!(objects_text(3, 0), "3 objects");
        assert_eq!(objects_text(1, 1), "1 picture");
        assert_eq!(objects_text(254, 254), "254 pictures");
        assert_eq!(objects_text(5, 1), "5 objects, 1 of them a picture");
        assert_eq!(objects_text(30, 12), "30 objects, 12 of them pictures");
    }
}
