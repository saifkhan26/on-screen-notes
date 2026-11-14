//! Pins: pinning the selection, the pin bar, and pointer handling for pins on the canvas.

use super::*;

impl OnScreenNotesApp {
    /// Pin the selection to every slide as one pin, or — when all of it
    /// is pinned already — take it out of its pins. The new pin starts a
    /// little off its objects, so it can be seen, and is selected, so it
    /// can be dragged where it should stay; its bar can then keep it to
    /// this slide.
    pub(super) fn toggle_pin_selection(&mut self) {
        let sel = self.tool.select.sel.clone();
        if self.tool.kind != crate::tools::ToolKind::Select || sel.is_empty() { return; }
        let li = self.canvases.active().active_layer;
        if self.selection_pinned() {
            if self.canvases.unpin_objects(li, &sel.strokes, &sel.shapes) {
                self.toast = Some(("Unpinned — the objects stay on this slide".to_string(), Instant::now()));
            }
        } else if let Some(id) = self.canvases.pin_objects(li, &sel.strokes, &sel.shapes, [24.0, 24.0], false) {
            // A new pin must be seen to be placed.
            self.canvases.set_pins_hidden(false);
            self.tool.select.sel.clear();
            self.tool.select.gesture = None;
            self.pin_tool.selected = Some(id);
            self.toast = Some(("Pinned to every slide — drag the pin where it should stay".to_string(), Instant::now()));
        }
    }

    /// Show pin `id` on its own slide only (`local`), or on every slide.
    /// Not an edit of any slide: only `pins.ron` is written.
    pub(super) fn set_pin_local(&mut self, id: u64, local: bool) {
        let Some(slide) = self.canvases.pin_board().members(id).map(|(s, _)| s) else { return };
        let Some(pin) = self.canvases.pin_board().get_mut(id) else { return };
        if pin.local == local { return; }
        pin.local = local;
        let msg = match local {
            false => "Pinned to every slide".to_string(),
            true if slide == self.canvases.active_index() => "Pinned to this slide only".to_string(),
            true => format!("Pinned to slide {} only", slide + 1),
        };
        self.toast = Some((msg, Instant::now()));
    }

    /// Hide every pin, or show them again. Hiding lets go of a selected
    /// pin.
    pub(super) fn set_pins_hidden(&mut self, hidden: bool) {
        if hidden == self.canvases.pins_hidden() { return; }
        self.canvases.set_pins_hidden(hidden);
        if hidden {
            self.pin_tool.clear();
        }
        let msg = if hidden { "Pins hidden — Shift+P shows them again" } else { "Pins shown" };
        self.toast = Some((msg.to_string(), Instant::now()));
    }

    /// Unpin the selected pin. Its objects stay on their slide, where the
    /// unpin is an undo step.
    pub(super) fn unpin_selected_pin(&mut self) {
        let Some(id) = self.pin_tool.selected else { return };
        let slide = self.canvases.pinned().iter().find(|p| p.pin.id == id).map(|p| p.slide);
        self.pin_tool.clear();
        if self.canvases.unpin(id) {
            let msg = match slide {
                Some(s) if s != self.canvases.active_index() => format!("Unpinned — the objects stay on slide {}", s + 1),
                _ => "Unpinned — the objects stay on this slide".to_string(),
            };
            self.toast = Some((msg, Instant::now()));
        }
    }

    /// Go to the slide pin `id`'s objects are on, with them selected and
    /// moved into the middle of the canvas area (`area` across). The zoom
    /// stays as it was.
    pub(super) fn open_pin_slide(&mut self, id: u64, area: egui::Vec2) {
        // A global pin may be away in the slide window, out of `pinned`.
        let find = |pins: Vec<crate::canvas::pins::Pinned>| pins.iter().find(|p| p.pin.id == id).map(|p| (p.slide, p.members.to_vec(), p.anchor()));
        let found = find(self.canvases.pinned()).or_else(|| find(self.canvases.global_pins()));
        let Some((slide, members, anchor)) = found else { return };
        self.canvases.go_to(slide);
        self.pin_tool.clear();
        // Only the view moves, so the slide is not flagged for the autosave.
        let (pan, zoom) = self.canvases.active_view_mut();
        *pan = [area.x * 0.5 - anchor[0] * *zoom, area.y * 0.5 - anchor[1] * *zoom];
        // A selection lives on one layer: that of the pin's first object.
        let Some(li) = members.first().map(|m| m.layer) else { return };
        let mut sel = crate::canvas::hit::Selection::default();
        for m in members.iter().filter(|m| m.layer == li) {
            sel.insert(m.obj);
        }
        // Only a real layer switch may flag the slide for the autosave.
        if self.canvases.active().active_layer != li {
            self.canvases.active_mut().active_layer = li;
        }
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel = sel;
        self.tool.select.rebind(slide, self.canvases.active());
        self.toast = Some((format!("Slide {} — the pin's objects are selected", slide + 1), Instant::now()));
    }

    /// Offer a pointer event to the pins. With the Select tool a press on
    /// a pin is the pin's, and so is the rest of that gesture; `q` is in
    /// points from the canvas area's top-left, `snap` turns in steps.
    /// A pin let go of keeps its centre inside the canvas area (`area`
    /// across), so it can always be grabbed again. Returns true when a
    /// pin took the event, which the Select tool must then not see.
    pub(super) fn pin_pointer(&mut self, ev: crate::input::pen::PenEvent, q: [f32; 2], snap: bool, area: egui::Vec2) -> bool {
        use crate::canvas::pins::Press;
        use crate::input::pen::PenEvent;
        match ev {
            PenEvent::Down(_) => {
                if self.tool.kind != crate::tools::ToolKind::Select { return false; }
                let pinned = self.canvases.pinned();
                let press = self.pin_tool.press(q, &pinned);
                drop(pinned);
                match press {
                    Press::Missed => false,
                    Press::Took => {
                        if let Some(id) = self.pin_tool.selected {
                            self.canvases.pin_board().raise(id);
                        }
                        // A pin and the slide's objects are never selected
                        // together.
                        self.tool.select.sel.clear();
                        self.tool.select.gesture = None;
                        true
                    }
                    Press::Open(id) => {
                        self.open_pin_slide(id, area);
                        true
                    }
                }
            }
            PenEvent::Move(_) | PenEvent::Up(_) if self.pin_tool.busy() => {
                let board = self.canvases.pin_board();
                if matches!(ev, PenEvent::Up(_)) {
                    self.pin_tool.release(q, snap, board);
                    if let Some(pin) = self.pin_tool.selected.and_then(|id| board.get_mut(id)) {
                        pin.center = [pin.center[0].clamp(0.0, area.x.max(0.0)), pin.center[1].clamp(0.0, area.y.max(0.0))];
                    }
                } else {
                    self.pin_tool.drag(q, snap, board);
                }
                true
            }
            _ => false,
        }
    }

    /// The selected pin's bar: where its objects are, and buttons to go
    /// there, show it on one slide or every slide, or unpin it.
    pub(super) fn show_pin_bar(&mut self, ctx: &egui::Context) {
        let Some(id) = self.pin_tool.selected else { return };
        if !self.ui_visible || self.tool.kind != crate::tools::ToolKind::Select || self.pin_tool.dragging() { return; }
        let Some((slide, local)) = self.canvases.pinned().iter().find(|p| p.pin.id == id).map(|p| (p.slide, p.pin.local)) else { return };
        match ui::arrange_bar::show_pin(ctx, slide, slide == self.canvases.active_index(), local) {
            Some(ui::arrange_bar::PinBarAction::Open) => {
                let area = self.overlay_cache.canvas_rect(ctx).size();
                self.open_pin_slide(id, area);
            }
            Some(ui::arrange_bar::PinBarAction::SetLocal(local)) => self.set_pin_local(id, local),
            Some(ui::arrange_bar::PinBarAction::Unpin) => self.unpin_selected_pin(),
            None => {}
        }
    }
}
