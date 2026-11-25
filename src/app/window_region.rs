//! The slide window on part of a slide: a box dragged on the canvas
//! (`BoxFor::SlideWindow`, read by `capture::update_region_select`) or
//! the selection. With no window open it pops out over that part, at the
//! canvas's zoom, so the part looks lifted off the canvas. A window
//! already open stays where it is and shows that part, fitted. Either way
//! the window keeps the part as its region: Fit goes back to it.

use super::*;

/// Room kept around a selection shown in the window, in screen points.
const SELECTION_PAD: f32 = 16.0;

impl OnScreenNotesApp {
    /// Drag a box on the canvas, to show in the slide window.
    pub(super) fn begin_window_region(&mut self) {
        if self.canvases.in_view() {
            self.toast = Some(("Drag the box on the main window".to_string(), Instant::now()));
            return;
        }
        if self.canvases.count() > 0 {
            self.begin_box(BoxFor::SlideWindow);
        }
    }

    /// The box `sel` (screen points) was dragged over the canvas.
    pub(super) fn show_box_in_window(&mut self, ctx: &egui::Context, sel: egui::Rect) {
        let canvas_rect = self.overlay_cache.canvas_rect(ctx);
        let c = self.canvases.active();
        let to_slide = |p: egui::Pos2| [(p.x - canvas_rect.min.x - c.pan[0]) / c.zoom, (p.y - canvas_rect.min.y - c.pan[1]) / c.zoom];
        let (a, b) = (to_slide(sel.min), to_slide(sel.max));
        self.open_region(ctx, [a[0], a[1], b[0], b[1]], sel);
    }

    /// The Select tool's selection, with a little room around it, in the
    /// slide window.
    pub(super) fn show_selection_in_window(&mut self, ctx: &egui::Context) {
        // In the slide window already: zoom to it there.
        if self.canvases.in_view() {
            self.zoom_to_selection(ctx);
            return;
        }
        let Some([x0, y0, x1, y1]) = self.selection_bounds() else {
            self.toast = Some(("Select something to show in the slide window".to_string(), Instant::now()));
            return;
        };
        let canvas_rect = self.overlay_cache.canvas_rect(ctx);
        let c = self.canvases.active();
        let pad = SELECTION_PAD / c.zoom.max(1e-3);
        let region = [x0 - pad, y0 - pad, x1 + pad, y1 + pad];
        let to_screen = |x: f32, y: f32| egui::pos2(canvas_rect.min.x + c.pan[0] + x * c.zoom, canvas_rect.min.y + c.pan[1] + y * c.zoom);
        let on_screen = egui::Rect::from_min_max(to_screen(region[0], region[1]), to_screen(region[2], region[3]));
        self.open_region(ctx, region, on_screen);
    }

    /// Show `region` of the active slide (slide units) in the slide
    /// window; on the canvas it is at `on_screen` (points).
    fn open_region(&mut self, ctx: &egui::Context, region: [f32; 4], on_screen: egui::Rect) {
        let idx = self.canvases.active_index();
        if let Some(win) = self.slide_window.as_mut() {
            win.aim(idx, region);
            ctx.send_viewport_cmd_to(ui::slide_window::SlideWindow::viewport_id(), egui::ViewportCommand::Focus);
            return;
        }
        let canvas_rect = self.overlay_cache.canvas_rect(ctx);
        let local = ui::slide_window::pop_out_rect(on_screen, ui::slide_window::MIN_SIZE, ctx.screen_rect());
        // The overlay's top-left on screen, in points: the window's place
        // is counted from the screen's corner, the box from the overlay's.
        let ppp = ctx.pixels_per_point().max(0.01);
        let origin = crate::platform::client_rect(self.osn_hwnd).map_or(egui::Vec2::ZERO, |(l, t, _, _)| egui::vec2(l as f32 / ppp, t as f32 / ppp));
        let c = self.canvases.active();
        // Through the window the slide sits where it does on the canvas,
        // at the same size.
        let pan = [canvas_rect.min.x + c.pan[0] - local.min.x, canvas_rect.min.y + c.pan[1] - local.min.y];
        self.slide_window = Some(ui::slide_window::SlideWindow::popped_out(idx, region, local.translate(origin), pan, c.zoom, &self.config));
    }
}
