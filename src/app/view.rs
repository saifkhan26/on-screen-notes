//! The canvas view: zoom to fit / to the selection, the glide there, and the held peek.

use super::*;

impl OnScreenNotesApp {
    /// Canvas box around everything on the active slide's shown layers.
    pub(super) fn slide_bounds(&self) -> Option<[f32; 4]> {
        let c = self.canvases.active();
        c.layers.iter().filter(|l| l.visible).filter_map(|l| l.content_bounds()).reduce(union_box)
    }

    /// Where on screen a fitted view goes, `[x, y, width, height]` from
    /// the canvas's top-left: the canvas less a margin, and below the
    /// toolbar and the bar under it while they show.
    pub(super) fn fit_area(&self, ctx: &egui::Context) -> [f32; 4] {
        const MARGIN: f32 = 32.0;
        let r = self.overlay_cache.canvas_rect(ctx);
        let top = if self.ui_visible {
            // The find bar too: what it goes to should not land under it.
            [
                ("osn_toolbar", egui::Order::Middle),
                ("osn_arrange_bar", egui::Order::Middle),
                ("osn_pin_bar", egui::Order::Middle),
                ("osn_find_bar", egui::Order::Foreground),
            ]
                .into_iter()
                .filter_map(|(id, order)| {
                    let id = egui::Id::new(id);
                    // A bar hidden now still has its old place on record.
                    let shown = ctx.memory(|m| m.areas().visible_last_frame(&egui::LayerId::new(order, id)));
                    shown.then(|| ctx.memory(|m| m.area_rect(id))).flatten()
                })
                .filter(|a| a.top() < r.top() + 200.0)
                .map(|a| a.bottom() - r.top())
                .fold(0.0_f32, f32::max)
        } else {
            0.0
        };
        [MARGIN, top + MARGIN, (r.width() - 2.0 * MARGIN).max(1.0), (r.height() - top - 2.0 * MARGIN).max(1.0)]
    }

    /// Glide the view to `to` over a few frames.
    pub(super) fn glide_view(&mut self, to: ([f32; 2], f32), ctx: &egui::Context) {
        let area = self.fit_area(ctx);
        let c = self.canvases.active();
        let about = [area[0] + area[2] * 0.5, area[1] + area[3] * 0.5];
        let glide = interaction::zoom_pan::ViewGlide::new(self.canvases.active_index(), (c.pan, c.zoom), to, about);
        self.view_glide = Some(glide);
        ctx.request_repaint();
    }

    /// The view that shows canvas box `bounds` whole, at most `max_zoom`.
    pub(super) fn view_fitting(&self, bounds: [f32; 4], max_zoom: f32, ctx: &egui::Context) -> ([f32; 2], f32) {
        interaction::zoom_pan::fit_view(bounds, self.fit_area(ctx), max_zoom)
    }

    /// Glide the view to show everything on the slide.
    pub(super) fn zoom_to_fit(&mut self, ctx: &egui::Context) {
        match self.slide_bounds() {
            Some(b) => {
                let to = self.view_fitting(b, interaction::zoom_pan::FIT_MAX_ZOOM, ctx);
                self.glide_view(to, ctx);
            }
            None => self.toast = Some(("Nothing on this slide to zoom to".to_string(), Instant::now())),
        }
    }

    /// Glide the view to show the selection; with nothing selected, the
    /// whole slide.
    pub(super) fn zoom_to_selection(&mut self, ctx: &egui::Context) {
        match self.selection_bounds() {
            Some(b) => {
                let to = self.view_fitting(b, interaction::zoom_pan::FIT_SELECTION_MAX_ZOOM, ctx);
                self.glide_view(to, ctx);
            }
            None => {
                if self.slide_bounds().is_some() {
                    self.toast = Some(("Nothing selected: showing the whole slide".to_string(), Instant::now()));
                }
                self.zoom_to_fit(ctx);
            }
        }
    }

    /// One frame of a glide in progress.
    pub(super) fn step_view_glide(&mut self, ctx: &egui::Context) {
        let Some(g) = self.view_glide.as_mut() else { return };
        if g.slide != self.canvases.active_index() {
            self.view_glide = None;
            return;
        }
        let (pan, zoom) = self.canvases.active_view_mut();
        if g.step(pan, zoom, Instant::now()) {
            ctx.request_repaint();
        } else {
            self.view_glide = None;
        }
    }

    /// The peek key (`canvas.hold_zoom_to_selection`): pressed, the view
    /// glides to the selection (the whole slide with nothing selected);
    /// held a moment and let go, it glides back. A quick tap stays.
    pub(super) fn update_peek(&mut self, ctx: &egui::Context) {
        /// Shorter than this is a tap, which keeps the new view.
        const TAP: std::time::Duration = std::time::Duration::from_millis(300);
        let Some(b) = self.hotkeys_cfg.canvas.hold_zoom_to_selection else { return };
        let typing = self.tool.in_progress_text.is_some()
            || self.slide_panel.is_renaming()
            || self.confirm_delete.is_some()
            || ctx.wants_keyboard_input();
        let held = ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            // Once going, only the key itself matters.
            i.key_down(b.key)
                && (self.peek.is_some() || (b.ctrl == ctrl && b.shift == i.modifiers.shift && b.alt == i.modifiers.alt))
        });
        match (&self.peek, held) {
            (None, true) if !typing => {
                let target = match self.selection_bounds() {
                    Some(sel) => Some((sel, interaction::zoom_pan::FIT_SELECTION_MAX_ZOOM)),
                    None => self.slide_bounds().map(|all| (all, interaction::zoom_pan::FIT_MAX_ZOOM)),
                };
                if let Some((bounds, max_zoom)) = target {
                    let c = self.canvases.active();
                    self.peek = Some(Peek { slide: self.canvases.active_index(), back: (c.pan, c.zoom), since: Instant::now() });
                    let to = self.view_fitting(bounds, max_zoom, ctx);
                    self.glide_view(to, ctx);
                }
            }
            (Some(p), false) => {
                if p.slide == self.canvases.active_index() && p.since.elapsed() >= TAP {
                    let back = p.back;
                    self.glide_view(back, ctx);
                }
                self.peek = None;
            }
            _ => {}
        }
    }
}
