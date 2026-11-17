//! Crop mode (`tools::crop`): the selected picture or drawing pad with a
//! rectangle on it to drag, what falls outside it dimmed. Its edges and
//! corners move its sides; inside it, it moves whole. Enter, the bar's
//! Crop, or a press off the picture keeps what is inside; Esc or Cancel
//! leaves the picture as it was. A stored picture cropped before can show
//! all of its file again (Whole picture). Each is one undo step.

use super::*;
use crate::canvas::shape::PictureRef;
use crate::input::pen::PenEvent;
use crate::tools::crop::{self, CropRect, Grab};
use std::sync::Arc;

/// How far from an edge or corner a press still takes it, in screen points.
const REACH: f32 = 12.0;

/// The crop being set, if any.
#[derive(Default)]
pub(super) struct CropState {
    edit: Option<CropEdit>,
    /// A press off the picture kept the crop; the rest of that gesture
    /// does nothing.
    swallow: bool,
}

struct CropEdit {
    slide: usize,
    layer: usize,
    shape: usize,
    /// Address of the picture's `png` bytes: how it is found again.
    key: usize,
    rect: CropRect,
    /// The press in hand: what it holds, the rectangle and the point (in
    /// the picture's own frame) it started from.
    held: Option<(Grab, CropRect, [f32; 2])>,
}

impl CropState {
    pub(super) fn is_open(&self) -> bool {
        self.edit.is_some()
    }
}

impl OnScreenNotesApp {
    /// The cropped picture, where it is now.
    fn crop_target(&self) -> Option<(&Shape, usize)> {
        let e = self.crop.edit.as_ref()?;
        let layer = self.canvases.slide(e.slide)?.layers.get(e.layer)?;
        let is = |s: &Shape| matches!(s, Shape::Raster { png, .. } if Arc::as_ptr(png) as usize == e.key);
        let i = if layer.shapes.get(e.shape).is_some_and(is) { e.shape } else { layer.shapes.iter().position(is)? };
        Some((&layer.shapes[i], i))
    }

    /// Start cropping the selected picture or pad.
    pub(super) fn start_crop(&mut self) {
        if !self.may_edit() {
            return;
        }
        let say = |app: &mut Self, msg: &str| app.toast = Some((msg.to_string(), Instant::now()));
        let Some((slide, layer, shape)) = self.selected_picture() else {
            return say(self, "Select one picture to crop");
        };
        let Some(s) = self.canvases.active().layers.get(layer).and_then(|l| l.shapes.get(shape)) else { return };
        let Shape::Raster { png, media, .. } = s else { return };
        if media.is_some() {
            return say(self, "Videos and GIFs cannot be cropped");
        }
        if s.pin() != 0 {
            return say(self, "Unpin the picture to crop it");
        }
        let key = Arc::as_ptr(png) as usize;
        self.close_pad(false);
        self.crop.edit = Some(CropEdit { slide, layer, shape, key, rect: CropRect::WHOLE, held: None });
        // Its selection handles would sit on the crop's.
        self.tool.select.sel = Default::default();
        self.tool.select.gesture = None;
        self.toast = Some(("Drag the edges or corners. Enter crops, Esc cancels".to_string(), Instant::now()));
    }

    /// Once a frame: leave crop mode if its slide, the Select tool or the
    /// picture went.
    pub(super) fn crop_frame_start(&mut self) {
        let Some(e) = self.crop.edit.as_ref() else { return };
        let gone = self.canvases.active_index() != e.slide
            || self.tool.kind != crate::tools::ToolKind::Select
            || !self.canvases.active_editable()
            || self.crop_target().is_none();
        if gone {
            self.crop.edit = None;
        }
    }

    /// Leave crop mode with the picture selected again.
    fn end_crop(&mut self, at: Option<usize>) {
        let Some(e) = self.crop.edit.take() else { return };
        if let Some(i) = at {
            self.tool.select.sel = crate::canvas::hit::Selection { strokes: Vec::new(), shapes: vec![i] };
            self.tool.select.rebind(e.slide, self.canvases.active());
        }
    }

    /// Leave the picture as it was.
    fn cancel_crop(&mut self) {
        let at = self.crop_target().map(|(_, i)| i);
        self.end_crop(at);
    }

    /// Replace the picture at `at` with `new`, one undo step.
    fn replace_cropped(&mut self, at: usize, new: Shape) {
        let Some(e) = self.crop.edit.as_ref() else { return };
        let layer = e.layer;
        self.canvases.edit_active(|c| {
            if let Some(l) = c.layers.get_mut(layer) {
                if let Some(slot) = l.shapes.get_mut(at) {
                    let old = std::mem::replace(slot, new.clone());
                    l.rev = l.rev.wrapping_add(1);
                    c.undo_log.push(crate::canvas::canvas::UndoEntry::ShapeReplaced(layer, at, old, new));
                    c.redo_log.clear();
                }
            }
        });
    }

    /// Keep what is inside the rectangle.
    fn apply_crop(&mut self) {
        let Some((shape, at)) = self.crop_target().map(|(s, i)| (s.clone(), i)) else {
            self.crop.edit = None;
            return;
        };
        let rect = self.crop.edit.as_ref().map_or(CropRect::WHOLE, |e| e.rect);
        if !rect.is_whole() {
            match crop::apply(&shape, rect) {
                Some(new) => self.replace_cropped(at, new),
                None => self.toast = Some(("Could not read the picture to crop it".to_string(), Instant::now())),
            }
        }
        self.end_crop(Some(at));
    }

    /// A stored picture cropped before shows all of its file again.
    fn crop_whole(&mut self) {
        let Some((shape, at)) = self.crop_target().map(|(s, i)| (s.clone(), i)) else { return };
        let Shape::Raster { picture: Some(p), .. } = &shape else { return };
        let read = crate::persistence::paths::media_dir().ok().and_then(|d| image::open(d.join(&p.file)).ok());
        let Some(img) = read.map(|i| i.to_rgba8()) else {
            self.toast = Some(("Could not read the picture's file".to_string(), Instant::now()));
            return;
        };
        let Some(mut new) = crop::whole(&shape, [img.width(), img.height()]) else { return };
        // A new preview, of all of it.
        if let Shape::Raster { png, px_size, picture: Some(np), .. } = &mut new {
            let whole = PictureRef { crop: None, ..(**np).clone() };
            let preview = whole.view(&img, crate::media::pureref::PREVIEW_SIDE);
            if let Some(bytes) = crate::canvas::pad::encode_png(&preview) {
                *png = Arc::new(bytes);
                *px_size = [preview.width(), preview.height()];
            }
        }
        let key = match &new {
            Shape::Raster { png, .. } => Arc::as_ptr(png) as usize,
            _ => 0,
        };
        self.replace_cropped(at, new);
        // Go on cropping what is now all of it.
        if let Some(e) = self.crop.edit.as_mut() {
            e.key = key;
            e.shape = at;
            e.rect = CropRect::WHOLE;
            e.held = None;
        }
    }

    /// A pen or mouse step, in canvas units. `true` when crop mode took it.
    pub(super) fn crop_pointer(&mut self, ev: PenEvent) -> bool {
        if self.crop.swallow {
            if matches!(ev, PenEvent::Up(_)) {
                self.crop.swallow = false;
            }
            return true;
        }
        let Some((shape, _)) = self.crop_target() else { return false };
        let obb = shape.obb();
        let min = crop::shown_px(shape).map_or([0.01, 0.01], crop::min_share);
        let zoom = self.canvases.active().zoom.max(1e-3);
        let Some(e) = self.crop.edit.as_mut() else { return false };
        match ev {
            PenEvent::Down(s) => match crop::grab_at(obb, e.rect, s.pos, REACH / zoom) {
                Some(g) => e.held = Some((g, e.rect, obb.to_local(s.pos))),
                None => {
                    let on_picture = {
                        let q = obb.to_local(s.pos);
                        q[0].abs() <= obb.half[0] && q[1].abs() <= obb.half[1]
                    };
                    if !on_picture {
                        // Off the picture: keep the crop, like clicking away.
                        self.apply_crop();
                        self.crop.swallow = true;
                    }
                }
            },
            PenEvent::Move(s) | PenEvent::Up(s) => {
                if let Some((g, start, from)) = e.held {
                    let q = obb.to_local(s.pos);
                    e.rect = crop::drag(g, start, [q[0] - from[0], q[1] - from[1]], obb.half, min);
                }
                if matches!(ev, PenEvent::Up(_)) {
                    e.held = None;
                }
            }
        }
        true
    }

    /// While cropping: Enter crops, Esc cancels, and undo and redo wait.
    pub(super) fn crop_keys(&mut self, ctx: &egui::Context) {
        if self.crop.edit.is_none() || ctx.wants_keyboard_input() {
            return;
        }
        let shortcut = |b: Option<crate::input::hotkeys_config::Binding>| {
            b.is_some_and(|b| ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(b.to_modifiers(), b.key))))
        };
        shortcut(self.hotkeys_cfg.canvas.redo);
        shortcut(self.hotkeys_cfg.canvas.undo);
        let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if enter {
            self.apply_crop();
        } else if esc {
            self.cancel_crop();
        }
    }

    /// The rectangle on the picture, what is outside it dimmed, its
    /// handles, and the bar.
    pub(super) fn crop_chrome(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        let Some((shape, _)) = self.crop_target() else { return };
        let Some(e) = self.crop.edit.as_ref() else { return };
        let c = self.canvases.active();
        let to_screen = |p: [f32; 2]| egui::pos2(canvas_rect.min.x + c.pan[0] + p[0] * c.zoom, canvas_rect.min.y + c.pan[1] + p[1] * c.zoom);
        let obb = shape.obb();
        let [hw, hh] = obb.half;
        let [x0, y0, x1, y1] = e.rect.local(obb.half);
        let quad = |a: [f32; 2], b: [f32; 2]| -> Vec<egui::Pos2> {
            [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]].into_iter().map(|q| to_screen(obb.to_canvas(q))).collect()
        };
        let can_whole = matches!(shape, Shape::Raster { picture: Some(p), .. } if p.crop.is_some());
        // On the canvas's own layer, after it: over the picture, under the
        // toolbar and panels.
        let painter = ctx.layer_painter(egui::LayerId::background()).with_clip_rect(canvas_rect);
        let dim = egui::Color32::from_black_alpha(150);
        for (a, b) in [([-hw, -hh], [hw, y0]), ([-hw, y1], [hw, hh]), ([-hw, y0], [x0, y1]), ([x1, y0], [hw, y1])] {
            if b[0] - a[0] > 1e-3 && b[1] - a[1] > 1e-3 {
                painter.add(egui::Shape::convex_polygon(quad(a, b), dim, egui::Stroke::NONE));
            }
        }
        let frame = quad([x0, y0], [x1, y1]);
        painter.add(egui::Shape::closed_line(frame.clone(), egui::Stroke::new(3.0, egui::Color32::from_black_alpha(160))));
        painter.add(egui::Shape::closed_line(frame, egui::Stroke::new(1.5, egui::Color32::WHITE)));
        // Thirds, faintly, to line things up by.
        for t in [1.0 / 3.0, 2.0 / 3.0] {
            let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
            let faint = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(70));
            painter.line_segment([to_screen(obb.to_canvas([x, y0])), to_screen(obb.to_canvas([x, y1]))], faint);
            painter.line_segment([to_screen(obb.to_canvas([x0, y])), to_screen(obb.to_canvas([x1, y]))], faint);
        }
        let (mx, my) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        for q in [[x0, y0], [mx, y0], [x1, y0], [x1, my], [x1, y1], [mx, y1], [x0, y1], [x0, my]] {
            let at = to_screen(obb.to_canvas(q));
            let r = egui::Rect::from_center_size(at, egui::vec2(9.0, 9.0));
            painter.rect_filled(r, 1.5, egui::Color32::WHITE);
            painter.rect_stroke(r, 1.5, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(200)));
        }

        // The bar: under the rectangle, or above it when there is no room.
        let pts: Vec<egui::Pos2> = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].into_iter().map(|q| to_screen(obb.to_canvas(q))).collect();
        let (lo, hi) = pts.iter().fold((egui::pos2(f32::MAX, f32::MAX), egui::pos2(f32::MIN, f32::MIN)), |(a, b), p| {
            (egui::pos2(a.x.min(p.x), a.y.min(p.y)), egui::pos2(b.x.max(p.x), b.y.max(p.y)))
        });
        let width = if can_whole { 300.0 } else { 170.0 };
        let mut at = egui::pos2((lo.x + hi.x) * 0.5 - width * 0.5, hi.y + 10.0);
        if at.y > canvas_rect.max.y - 50.0 {
            at.y = lo.y - 50.0;
        }
        at.x = at.x.clamp(canvas_rect.min.x + 8.0, (canvas_rect.max.x - width - 8.0).max(canvas_rect.min.x + 8.0));
        at.y = at.y.clamp(canvas_rect.min.y + 56.0, (canvas_rect.max.y - 48.0).max(canvas_rect.min.y + 56.0));
        let (mut apply, mut cancel, mut whole) = (false, false, false);
        egui::Area::new(egui::Id::new("osn_crop_bar"))
            .order(egui::Order::Middle)
            .fixed_pos(at)
            .show(ctx, |ui| {
                egui::Frame {
                    inner_margin: egui::Margin::symmetric(10.0, 6.0),
                    outer_margin: egui::Margin::ZERO,
                    rounding: egui::Rounding::same(10.0),
                    shadow: ui::theme::card_shadow(),
                    fill: ui::theme::GLASS_FILL,
                    stroke: egui::Stroke::NONE,
                }
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        apply = ui.button("Crop").on_hover_text("Keep what is inside — Enter").clicked();
                        cancel = ui.button("Cancel").on_hover_text("Leave it as it was — Esc").clicked();
                        if can_whole {
                            whole = ui
                                .button("Whole picture")
                                .on_hover_text("Show all of the picture again (it was cropped before)")
                                .clicked();
                        }
                    });
                });
            });
        if apply {
            self.apply_crop();
        } else if cancel {
            self.cancel_crop();
        } else if whole {
            self.crop_whole();
        }
    }
}
