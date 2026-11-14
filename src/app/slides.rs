//! Slides as a whole: deleting (with its confirmation), moving, locking, and the status badges.

use super::*;

impl OnScreenNotesApp {
    /// The delete-slide key: ask first, unless the slide has nothing on
    /// it to lose.
    pub(super) fn request_slide_delete(&mut self) {
        let idx = self.canvases.active_index();
        let empty = self.canvases.slide(idx).is_some_and(|c| {
            c.layers.iter().all(|l| l.strokes.is_empty() && l.shapes.is_empty() && l.image.is_none())
        });
        if empty {
            self.delete_slide(idx);
        } else {
            self.confirm_delete = Some(idx);
        }
    }

    /// Delete slide `idx` (the only slide is emptied instead).
    pub(super) fn delete_slide(&mut self, idx: usize) {
        if idx >= self.canvases.count() { return; }
        if self.canvases.active_index() != idx {
            self.canvases.go_to(idx);
        }
        let before = self.canvases.count();
        self.canvases.delete_active();
        self.view_glide = None;
        self.peek = None;
        // The slide window follows its slide to its new number, or
        // closes with it.
        if self.canvases.count() < before && self.slide_window.as_mut().is_some_and(|w| !w.slide_removed(idx)) {
            self.slide_window = None;
        }
    }

    /// "Delete this slide?", while one is waiting on it. Enter (or the
    /// delete key again) deletes, Esc keeps it.
    pub(super) fn show_delete_confirm(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.confirm_delete else { return };
        if idx >= self.canvases.count() {
            self.confirm_delete = None;
            return;
        }
        let only = self.canvases.count() == 1;
        let name = self.canvases.name(idx);
        let which = if name.is_empty() { format!("Slide {}", idx + 1) } else { format!("Slide {} \u{201c}{name}\u{201d}", idx + 1) };
        let holds = match self.canvases.slide(idx) {
            Some(c) => {
                let objects: usize = c.layers.iter().map(|l| l.strokes.len() + l.shapes.len() + usize::from(l.image.is_some())).sum();
                let pictures: usize = c
                    .layers
                    .iter()
                    .map(|l| {
                        usize::from(l.image.is_some())
                            + l.shapes.iter().filter(|s| matches!(s, Shape::Raster { kind: crate::canvas::shape::RasterKind::Image, .. })).count()
                    })
                    .sum();
                let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
                match pictures {
                    0 => format!("It holds {}.", plural(objects, "object", "objects")),
                    p => format!("It holds {}, {} of them {}.", plural(objects, "object", "objects"), p, if p == 1 { "a picture" } else { "pictures" }),
                }
            }
            None => "It is still opening.".to_string(),
        };
        let mut decision: Option<bool> = None;
        let delete_key = self.hotkeys_cfg.canvas.delete_canvas;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                decision = Some(false);
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || delete_key.is_some_and(|b| i.consume_shortcut(&egui::KeyboardShortcut::new(b.to_modifiers(), b.key)))
            {
                decision = Some(true);
            }
        });
        egui::Window::new(if only { "Empty this slide?" } else { "Delete this slide?" })
            .id(egui::Id::new("osn_delete_slide"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(340.0);
                ui.label(format!("{which}. {holds}"));
                let after = if only {
                    "It is the only slide, so it is emptied instead. This can't be undone."
                } else {
                    "This can't be undone."
                };
                ui.label(egui::RichText::new(after).small().color(ui::theme::TEXT_MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = egui::RichText::new(if only { "Empty slide" } else { "Delete slide" }).color(egui::Color32::WHITE);
                    if ui.add(egui::Button::new(label).fill(egui::Color32::from_rgb(190, 52, 52))).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Keep it").clicked() {
                        decision = Some(false);
                    }
                });
                ui.add_space(2.0);
                ui.label(egui::RichText::new("Enter deletes · Esc keeps it").small().color(ui::theme::TEXT_MUTED));
            });
        match decision {
            Some(true) => {
                self.confirm_delete = None;
                self.delete_slide(idx);
            }
            Some(false) => self.confirm_delete = None,
            None => {}
        }
    }

    /// Move slide `from` to place `to` (`CanvasManager::move_slide`), and
    /// everything here that knows slides by number along with it; or say
    /// why it can't move yet.
    pub(super) fn move_slide(&mut self, from: usize, to: usize) {
        if let Err(why) = self.canvases.move_slide(from, to) {
            self.toast = Some((why.to_string(), Instant::now()));
            return;
        }
        let map = |i: usize| crate::canvas::moved_index(from, to, i);
        if let Some(w) = self.slide_window.as_mut() {
            w.renumber(map);
        }
        self.overlay_cache.slides_moved(map);
        if let Some(g) = self.view_glide.as_mut() {
            g.slide = map(g.slide);
        }
        if let Some(p) = self.peek.as_mut() {
            p.slide = map(p.slide);
        }
        if let Some(p) = self.picture_colours.as_mut() {
            p.at.0 = map(p.at.0);
        }
        if let Some(c) = self.confirm_delete.as_mut() {
            *c = map(*c);
        }
        // Their undo entries are where they were; only the run's slide
        // number is out of date.
        self.restyle_run = None;
        self.nudge_run = None;
        // The selection is on the slide still shown, now numbered anew.
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// How the lock key reads, for messages ("Ctrl+R" unless rebound).
    pub(super) fn lock_key(&self) -> String {
        self.hotkeys_cfg.canvas.lock_slide.map_or_else(|| "its hotkey".into(), |b| b.label())
    }

    /// Whether the active slide takes an edit now. If not, a toast says
    /// why: it is locked, or still opening.
    pub(super) fn may_edit(&mut self) -> bool {
        if self.canvases.active_editable() {
            return true;
        }
        let msg = if self.canvases.opening().is_some() {
            "This slide is still opening".to_string()
        } else {
            format!("This slide is locked — {} unlocks it", self.lock_key())
        };
        self.toast = Some((msg, Instant::now()));
        false
    }

    /// Lock the active slide against edits, or unlock it.
    pub(super) fn toggle_lock(&mut self) {
        let i = self.canvases.active_index();
        let locked = !self.canvases.is_locked(i);
        self.canvases.set_locked(i, locked);
        if locked {
            // Nothing half-done carries on into the lock: a stroke or
            // shape being drawn, a move, a layer drag.
            self.tool.cancel();
            self.eraser_override.cancel();
            self.mouse_gesture = false;
            self.pen_using_eraser = false;
            self.pin_tool.clear();
            self.layer_xform = None;
            self.pan_drag_anchor = None;
        }
        let msg = if locked {
            format!("Slide {} locked — drag to pan, wheel to zoom · {} unlocks", i + 1, self.lock_key())
        } else {
            format!("Slide {} unlocked", i + 1)
        };
        self.toast = Some((msg, Instant::now()));
    }

    /// While the active slide loads, a bar saying how far it has got:
    /// its file first, then its pictures. On a locked slide, a badge
    /// that unlocks it when clicked.
    pub(super) fn show_slide_status(&mut self, ctx: &egui::Context) {
        let idx = self.canvases.active_index();
        let work = match self.canvases.opening() {
            Some((done, size)) => Some((format!("Opening slide {} · {}", idx + 1, size_label(size)), done)),
            None => {
                let (ready, all) = self.overlay_cache.pictures_shown();
                (ready < all).then(|| (format!("Loading pictures · {ready} of {all}"), ready as f32 / all as f32))
            }
        };
        match work {
            Some(_) => {
                self.progress_since.get_or_insert_with(Instant::now);
            }
            None => self.progress_since = None,
        }
        let work = work.filter(|_| self.progress_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(150)));
        let locked = self.canvases.is_locked(idx) && self.ui_visible;
        // Kept out of screenshots and captures like the rest of the chrome.
        let capturing = self.pending_screenshot.is_some()
            || self.pending_freeze.is_some()
            || self.pending_lasso.is_some()
            || self.region_select.is_some();
        if capturing || (work.is_none() && !locked) {
            return;
        }
        // Under the toolbar while it shows.
        let toolbar = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar")));
        let top = toolbar.filter(|_| self.ui_visible).map_or(12.0, |r| r.max.y + 8.0);
        let key = self.lock_key();
        let mut unlock = false;
        egui::Area::new(egui::Id::new("osn_slide_status"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let card = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                        egui::Frame::none()
                            .fill(ui::theme::GLASS_FILL)
                            .rounding(8.0)
                            .shadow(ui::theme::card_shadow())
                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                            .show(ui, |ui| add(ui))
                            .response
                    };
                    if locked {
                        let r = card(ui, &mut |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let icon = egui::Image::new(egui::include_image!("../../assets/icons/lock.svg"))
                                    .tint(ui::theme::TEXT_PRIMARY)
                                    .fit_to_exact_size(egui::vec2(14.0, 14.0));
                                ui.add(icon);
                                ui.label(egui::RichText::new("Locked").color(ui::theme::TEXT_PRIMARY));
                                ui.label(egui::RichText::new("pan and zoom only").small().color(ui::theme::TEXT_MUTED));
                            });
                        });
                        let r = ui.interact(r.rect, egui::Id::new("osn_unlock_badge"), egui::Sense::click());
                        if r.hovered() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if r.on_hover_text(format!("Click or press {key} to unlock")).clicked() {
                            unlock = true;
                        }
                    }
                    if let Some((label, done)) = &work {
                        card(ui, &mut |ui| {
                            ui.label(egui::RichText::new(label.as_str()).small().color(ui::theme::TEXT_PRIMARY));
                            let (bar, _) = ui.allocate_exact_size(egui::vec2(240.0, 4.0), egui::Sense::hover());
                            let p = ui.painter();
                            p.rect_filled(bar, 2.0, egui::Color32::from_white_alpha(28));
                            let filled = egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * done.clamp(0.0, 1.0), bar.height()));
                            p.rect_filled(filled, 2.0, egui::Color32::from_rgb(86, 160, 255));
                        });
                    }
                });
            });
        if unlock {
            self.toggle_lock();
        }
    }

    /// A tag renamed or removed everywhere from the slide panel's tag
    /// list: done on every slide read but locked ones, and said in a
    /// toast. The highlight's picked tags follow.
    pub(super) fn apply_tag_action(&mut self, a: ui::slide_panel::TagAction) {
        use crate::canvas::units::same_tag;
        let tags_text = |tags: &[String]| tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ");
        let (what, (n, locked)) = match &a {
            ui::slide_panel::TagAction::Rename(from, to) => {
                (format!("#{from} is now {}", tags_text(to)), self.canvases.rename_tag(from, to))
            }
            ui::slide_panel::TagAction::Remove(tag) => (format!("#{tag} removed"), self.canvases.remove_tag(tag)),
        };
        let picked = &mut self.media_filter.tags;
        match a {
            ui::slide_panel::TagAction::Rename(from, to) if picked.iter().any(|t| same_tag(t, &from)) => {
                picked.retain(|t| !same_tag(t, &from));
                for t in to {
                    if !picked.iter().any(|p| same_tag(p, &t)) {
                        picked.push(t);
                    }
                }
            }
            ui::slide_panel::TagAction::Remove(tag) => picked.retain(|t| !same_tag(t, &tag)),
            _ => {}
        }
        let things = if n == 1 { "object" } else { "objects" };
        let mut msg = format!("{what} on {n} {things}");
        if locked > 0 {
            let s = if locked == 1 { "slide was" } else { "slides were" };
            msg.push_str(&format!(" — {locked} locked {s} left as they were"));
        }
        self.toast = Some((msg, Instant::now()));
    }
}
