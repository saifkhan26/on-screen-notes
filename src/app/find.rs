//! Find (Ctrl+F): the bar, going to the label it picks, and the flash on it.

use super::*;
use crate::canvas::shape::Shape;
use crate::canvas::units::bounds_of;
use crate::ui::find_bar::{Found, Hit, FOUND};

/// What find just went to, flashing so the eye lands on it.
pub(super) struct FindFlash {
    slide: usize,
    layer: usize,
    found: Found,
    since: Instant,
}

/// Where find started: the slide shown then, and each slide's view as it
/// was before find first moved it, so Esc can put everything back.
pub(super) struct FindStart {
    slide: usize,
    views: Vec<(usize, ([f32; 2], f32))>,
}

/// How long the flash lasts, the part of it spent closing in on the
/// label, and the part at the end spent fading.
const FLASH: f32 = 1.6;
const CLOSE_IN: f32 = 0.35;
const FADE: f32 = 0.6;
/// Smallest the found text comes up on screen, in points: a label far
/// out of reach is zoomed in to read, a readable one keeps the zoom.
const READABLE_PX: f32 = 18.0;
/// Smallest a tagged object comes up, as a share of the zoom that fits
/// it to the view: one far too small to see is zoomed in to.
const SEEN: f32 = 0.4;

impl OnScreenNotesApp {
    /// The find bar while it is open; go to the label it picks.
    pub(super) fn show_find_bar(&mut self, ctx: &egui::Context) {
        self.ws.find.free_retired();
        self.poll_find_reading(ctx);
        if !self.ws.find.is_open() {
            self.ws.find_paste_prev = false;
            self.ws.find_start = None;
            return;
        }
        if self.ws.find_start.is_none() {
            let (slide, c) = (self.canvases.active_index(), self.canvases.active());
            self.ws.find_start = Some(FindStart { slide, views: vec![(slide, (c.pan, c.zoom))] });
        }
        self.take_find_paste(ctx);
        let reading = self.update_picture_results(ctx).or_else(|| self.update_copies(ctx));
        self.update_word_pictures(ctx);
        self.update_tag_suggestions();
        let canvases = &self.canvases;
        let unread = (0..canvases.count()).filter(|&i| canvases.slide(i).is_none()).count();
        let picked = self.ws.find.show(
            ctx,
            |q| ui::find_bar::search(q, (0..canvases.count()).filter_map(|i| canvases.slide(i).map(|c| (i, c)))),
            unread,
            canvases.active_index(),
            |i| canvases.name(i).to_string(),
            reading,
            || canvases.tag_index().0,
        );
        if let Some(hit) = picked {
            self.go_to_find_hit(&hit, ctx);
        }
        if self.ws.find.take_cancelled() {
            self.back_to_find_start(ctx);
        }
        // Closed any other way: stay where it went.
        if !self.ws.find.is_open() {
            self.ws.find_start = None;
        }
        match self.ws.find.take_request() {
            Some(ui::find_bar::Request::SelectExtras { slide, layer, shapes }) => self.select_extras(slide, layer, shapes, ctx),
            Some(ui::find_bar::Request::Tag(a)) => self.apply_tag_action(a),
            Some(ui::find_bar::Request::TagPicture { slide, layer, shape, tag }) => self.tag_suggested(slide, layer, shape, &tag),
            Some(ui::find_bar::Request::NotThis { slide, layer, shape, tag }) => self.reject_suggested(slide, layer, shape, &tag),
            Some(ui::find_bar::Request::DeleteCopy { slide, layer, shape, png }) => self.delete_copy(slide, layer, shape, &png),
            None => {}
        }
    }

    /// Go to `hit`'s slide and glide the view there, centred: at the zoom
    /// it had if the match fits and can be made out there, else at one
    /// where it can — a label read, an object seen. It flashes once there.
    fn go_to_find_hit(&mut self, hit: &Hit, ctx: &egui::Context) {
        if let (Some(start), Some(c)) = (&mut self.ws.find_start, self.canvases.slide(hit.slide)) {
            if !start.views.iter().any(|v| v.0 == hit.slide) {
                start.views.push((hit.slide, (c.pan, c.zoom)));
            }
        }
        if hit.slide != self.canvases.active_index() {
            self.canvases.go_to(hit.slide);
        }
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(hit.layer) else { return };
        let (b, least) = match &hit.found {
            Found::Label(i) => {
                let Some(Shape::Text { pos, content, font_size, rotation, background, .. }) = layer.shapes.get(*i) else { return };
                // The box it is drawn in: `obb` only estimates a label's width.
                let painter = ctx.layer_painter(egui::LayerId::background());
                let ring = crate::canvas::render::text_box_ring(&painter, content, *font_size, background.is_some(), *rotation, egui::pos2(pos[0], pos[1]), 0.0);
                let b = ring.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]);
                (b, Least::Readable(*font_size))
            }
            Found::Tagged { strokes, shapes, .. } => {
                let Some(b) = bounds_of(layer, strokes, shapes) else { return };
                (b, Least::Seen)
            }
        };
        let area = self.fit_area(ctx);
        let (_, fits) = self.view_fitting(b, interaction::zoom_pan::FIT_SELECTION_MAX_ZOOM, ctx);
        let least = match least {
            Least::Readable(font_size) => READABLE_PX / font_size.max(1.0),
            Least::Seen => fits * SEEN,
        }
        .min(fits);
        let zoom = c.zoom.clamp(least, fits);
        let pan = [
            area[0] + area[2] * 0.5 - (b[0] + b[2]) * 0.5 * zoom,
            area[1] + area[3] * 0.5 - (b[1] + b[3]) * 0.5 * zoom,
        ];
        self.glide_view((pan, zoom), ctx);
        self.ws.find_flash = Some(FindFlash { slide: hit.slide, layer: hit.layer, found: hit.found.clone(), since: Instant::now() });
        if hit.hidden {
            let msg = format!("That is on layer {}, which is hidden", hit.layer + 1);
            self.toast = Some((msg, Instant::now()));
        }
    }

    /// Esc closed find: put back every view it moved and go back to the
    /// slide it started from, gliding when that is the slide shown.
    fn back_to_find_start(&mut self, ctx: &egui::Context) {
        let Some(start) = self.ws.find_start.take() else { return };
        self.ws.find_flash = None;
        self.ws.view_glide = None;
        for &(slide, (pan, zoom)) in &start.views {
            if slide != start.slide {
                self.canvases.set_view(slide, pan, zoom);
            }
        }
        let Some(&(_, view)) = start.views.iter().find(|v| v.0 == start.slide) else { return };
        if start.slide == self.canvases.active_index() {
            self.glide_view(view, ctx);
        } else {
            self.canvases.go_to(start.slide);
            self.canvases.set_view(start.slide, view.0, view.1);
        }
    }

    /// A ring round what find went to that closes in on it, holds, and
    /// fades. Follows the view while it glides; gone on another slide,
    /// or once what it marked is not there any more.
    pub(super) fn show_find_flash(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        let Some(f) = &self.ws.find_flash else { return };
        let t = f.since.elapsed().as_secs_f32();
        let ease = |x: f32| 1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(3);
        // Screen points out from it: from far to snug.
        let pad = 6.0 + 26.0 * (1.0 - ease(t / CLOSE_IN));
        let alpha = ((FLASH - t) / FADE).clamp(0.0, 1.0);
        let c = self.canvases.active();
        let to_screen = |p: [f32; 2]| egui::pos2(canvas_rect.min.x + c.pan[0] + p[0] * c.zoom, canvas_rect.min.y + c.pan[1] + p[1] * c.zoom);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("osn_find_flash")));
        let layer = c.layers.get(f.layer).filter(|_| t < FLASH && f.slide == self.canvases.active_index());
        let ring = layer.and_then(|l| match &f.found {
            Found::Label(i) => match l.shapes.get(*i)? {
                Shape::Text { pos, content, font_size, rotation, background, .. } => {
                    let size = (font_size * c.zoom).max(2.0);
                    Some(crate::canvas::render::text_box_ring(&painter, content, size, background.is_some(), *rotation, to_screen(*pos), pad))
                }
                _ => None,
            },
            // One shape: round its own (maybe turned) box. Several: round
            // the box they all fit in.
            Found::Tagged { strokes, shapes, .. } => {
                let (centre, half, angle) = match (strokes.as_slice(), shapes.as_slice()) {
                    ([], [i]) => {
                        let o = l.shapes.get(*i)?.obb();
                        (o.center, o.half, o.angle)
                    }
                    _ => {
                        let b = bounds_of(l, strokes, shapes)?;
                        ([(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5], [(b[2] - b[0]) * 0.5, (b[3] - b[1]) * 0.5], 0.0)
                    }
                };
                let half = [half[0] * c.zoom + pad, half[1] * c.zoom + pad];
                Some(crate::canvas::render::box_ring(to_screen(centre), half, angle, pad))
            }
        });
        let Some(ring) = ring else {
            self.ws.find_flash = None;
            return;
        };
        // A light ring over a dark halo: it shows on a white page too.
        let glow = egui::Color32::BLACK.gamma_multiply(0.45 * alpha);
        painter.add(egui::Shape::closed_line(ring.clone(), egui::Stroke::new(6.0, glow)));
        painter.add(egui::Shape::closed_line(ring, egui::Stroke::new(2.5, FOUND.gamma_multiply(alpha))));
        ctx.request_repaint();
    }
}

/// How far in a match is zoomed, at least.
enum Least {
    /// A label: until text this tall (canvas units) reads.
    Readable(f32),
    /// An object: until it is a fair share of the view.
    Seen,
}
