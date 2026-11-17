//! Drawing pads (`canvas::pad`): open a still picture for painting, send
//! the pen into it, and put it back into its slide as that picture's
//! pixels.
//!
//! While a pad is open the pen and eraser tools paint into it: the pen
//! style picks the brush (pencil and airbrush draw with the pencil, the
//! rest with the pen) and Ctrl or the eraser tool erases. Undo and redo
//! step through its strokes. Esc, a press outside it, another tool or
//! another slide puts it away: its pixels go into the slide, one undo
//! step there for the whole time it was open. Every 30 s of drawing they
//! are also written into the slide quietly, so a crash loses little.

use super::*;
use crate::canvas::pad::{self, Brush, PadFrame, Walk};
use crate::canvas::pad_gpu::{self, Session};
use crate::canvas::stroke::StrokeStyle;
use crate::input::pen::PenEvent;
use crate::tools::ToolKind;
use std::sync::Arc;

/// How often an open pad's pixels go into its slide while it is drawn on.
const SNAPSHOT_EVERY: Duration = Duration::from_secs(30);

/// The drawing pad open now, if any.
#[derive(Default)]
pub(super) struct PadState {
    edit: Option<PadEdit>,
    /// Shown textures of pads put away, freed at the start of the next
    /// frame: never in the frame that drew them.
    retired: Vec<egui::TextureId>,
    /// A press outside the open pad put it away; the rest of that gesture
    /// draws nothing.
    swallow: bool,
}

struct PadEdit {
    session: Session,
    slide: usize,
    layer: usize,
    shape: usize,
    /// Address of the picture's `png` bytes as the slide holds them now:
    /// how the picture is found again, and drawn live (`OverlayCache::set_pad`).
    key: usize,
    /// The picture as it was when opened: the slide's one undo step.
    original: Shape,
    frame: PadFrame,
    walk: Option<Walk>,
    /// The tool before the pad was opened, given back by Esc.
    prev_tool: ToolKind,
    /// `session.edits` when the slide last got the pixels, and when.
    saved_edits: u64,
    saved_at: Instant,
    /// The pad's paper (straight RGBA, alpha its opacity); `None` for a
    /// picture that is not a pad. Changed with the paper bar.
    paper: Option<[u8; 4]>,
    /// The paper changed while open: the slide's undo step covers it.
    paper_changed: bool,
}

/// Paper colours one tap away on the paper bar.
const PAPERS: [([u8; 3], &str); 5] = [
    ([255, 255, 255], "White"),
    ([246, 238, 220], "Cream"),
    ([160, 160, 160], "Grey"),
    ([52, 54, 58], "Slate"),
    ([18, 18, 20], "Black"),
];

impl PadState {
    pub(super) fn is_open(&self) -> bool {
        self.edit.is_some()
    }

    /// What the canvas draws in the open pad's place.
    fn shown(&self) -> Option<(usize, egui::TextureId)> {
        self.edit.as_ref().map(|e| (e.key, e.session.texture_id()))
    }
}

/// Where picture `key` is on `layer`: at `hint` if it is still there.
fn find(layer: &crate::canvas::canvas::Layer, key: usize, hint: usize) -> Option<usize> {
    let is = |s: &Shape| matches!(s, Shape::Raster { png, .. } if Arc::as_ptr(png) as usize == key);
    if layer.shapes.get(hint).is_some_and(is) {
        return Some(hint);
    }
    layer.shapes.iter().position(is)
}

impl OnScreenNotesApp {
    /// Once a frame, before anything is drawn: free what pads put away
    /// last frame, put the open one away if its slide, tool or picture
    /// went, follow the picture, and have the canvas show it live.
    pub(super) fn pad_frame_start(&mut self) {
        for id in self.pad.retired.drain(..) {
            pad_gpu::free_texture(id);
        }
        if let Some(e) = self.pad.edit.as_ref() {
            let leave = self.canvases.active_index() != e.slide
                || !self.canvases.active_editable()
                || !matches!(self.tool.kind, ToolKind::Pen | ToolKind::Eraser);
            if leave {
                self.close_pad(false);
            }
        }
        if let Some(e) = self.pad.edit.as_mut() {
            let found = self
                .canvases
                .slide(e.slide)
                .and_then(|c| c.layers.get(e.layer))
                .and_then(|l| find(l, e.key, e.shape).map(|i| (i, &l.shapes[i])));
            match found {
                Some((i, shape)) => {
                    e.shape = i;
                    if let Some(f) = PadFrame::of(shape, e.session.size()) {
                        e.frame = f;
                    }
                }
                None => {
                    // Gone from under it (nothing should do that while it
                    // is open): what was drawn since the last save is lost.
                    let e = self.pad.edit.take().expect("checked above");
                    self.pad.retired.push(e.session.texture_id());
                    self.toast = Some(("The picture being drawn on went away".to_string(), Instant::now()));
                }
            }
        }
        self.overlay_cache.set_pad(self.pad.shown());
    }

    /// The picture store moved the open pad's bytes to the media folder:
    /// follow them to the slide's new (preview) bytes.
    pub(super) fn pad_follow_store(&mut self, swaps: &[(Arc<Vec<u8>>, crate::canvas::shape::PictureRef)]) {
        let Some(e) = self.pad.edit.as_mut() else { return };
        if !swaps.iter().any(|(from, _)| Arc::as_ptr(from) as usize == e.key) {
            return;
        }
        if let Some(Shape::Raster { png, .. }) = self.canvases.slide(e.slide).and_then(|c| c.layers.get(e.layer)).and_then(|l| l.shapes.get(e.shape)) {
            e.key = Arc::as_ptr(png) as usize;
        }
    }

    /// A new pad on the last paper picked (white to start), in the middle
    /// of the view at one pixel a screen pixel (smaller if the view is),
    /// open for drawing.
    pub(super) fn new_pad(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        if !self.may_edit() {
            return;
        }
        if !pad_gpu::available() {
            self.toast = Some(("Drawing pads need the GPU renderer".to_string(), Instant::now()));
            return;
        }
        self.close_pad(false);
        let px = pad::NEW_SIZE;
        let Some(png) = pad::blank_png(px) else { return };
        let (pan, zoom) = {
            let c = self.canvases.active();
            (c.pan, c.zoom.max(1e-3))
        };
        let ppp = ctx.pixels_per_point().max(0.01);
        let mut size = [px[0] as f32 / ppp / zoom, px[1] as f32 / ppp / zoom];
        let view = [canvas_rect.width() / zoom, canvas_rect.height() / zoom];
        let fit = (view[0] * 0.9 / size[0]).min(view[1] * 0.9 / size[1]).min(1.0);
        if fit > 0.0 {
            size = [size[0] * fit, size[1] * fit];
        }
        let center = [(canvas_rect.width() * 0.5 - pan[0]) / zoom, (canvas_rect.height() * 0.5 - pan[1]) / zoom];
        let shape = Shape::Raster {
            pos: [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            size,
            png: Arc::new(png),
            px_size: px,
            color: [0; 4],
            rotation: 0.0,
            kind: crate::canvas::shape::RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            flip: false,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: Some(self.config.pad_paper),
        };
        self.insert_and_select(Vec::new(), vec![shape], [0.0, 0.0]);
        let layer = self.canvases.active().active_layer;
        if let Some(&i) = self.tool.select.sel.shapes.first() {
            self.open_pad(layer, i);
        }
    }

    /// Open the selected picture for drawing on.
    pub(super) fn draw_on_selected_picture(&mut self) {
        if !self.may_edit() {
            return;
        }
        match self.selected_picture() {
            Some((_, layer, shape)) => {
                self.open_pad(layer, shape);
            }
            None => self.toast = Some(("Select one picture to draw on".to_string(), Instant::now())),
        }
    }

    /// Open picture `shape` on `layer` of the active slide for drawing on.
    fn open_pad(&mut self, layer: usize, shape: usize) -> bool {
        self.close_pad(false);
        let say = |app: &mut Self, msg: &str| {
            app.toast = Some((msg.to_string(), Instant::now()));
            false
        };
        if !pad_gpu::available() {
            return say(self, "Drawing pads need the GPU renderer");
        }
        let slide = self.canvases.active_index();
        let Some(s) = self.canvases.active().layers.get(layer).and_then(|l| l.shapes.get(shape)).cloned() else {
            return false;
        };
        let Shape::Raster { png, media, link, paper, color, kind, .. } = &s else {
            return say(self, "Only a picture can be drawn on");
        };
        let mut paper = *paper;
        // Pads made before they had paper of their own: white pictures
        // with ink drawn into them, told apart by the colour they were
        // made with. Their ink is lifted off the white once, and white
        // paper goes behind it.
        let old_pad = paper.is_none() && *color == pad::WHITE && *kind == crate::canvas::shape::RasterKind::Image;
        if media.is_some() {
            return say(self, "Videos and GIFs cannot be drawn on");
        }
        if *link != 0 {
            return say(self, "This picture is linked to a PureRef board; a resync would undo drawing on it");
        }
        let key = Arc::as_ptr(png) as usize;
        let media_dir = crate::persistence::paths::media_dir().ok();
        let Some(img) = crate::clipboard::still_pixels(&s, media_dir.as_deref()) else {
            return say(self, "Could not read the picture's pixels");
        };
        let (w, h) = img.dimensions();
        let max = pad_gpu::max_side();
        if w > max || h > max {
            return say(self, &format!("Pictures up to {max} pixels a side can be drawn on; this one is {w}×{h}"));
        }
        let Some(frame) = PadFrame::of(&s, [w, h]) else { return false };
        let mut pixels = img.into_raw();
        let lifted = old_pad && pad::is_opaque(&pixels);
        if lifted {
            pad::lift_off_white(&mut pixels);
            paper = Some(pad::WHITE);
        }
        let Some(session) = Session::new(pixels, w, h, paper.is_some()) else {
            return say(self, "The GPU could not open the picture for drawing");
        };
        let prev_tool = self.tool.kind;
        if !matches!(prev_tool, ToolKind::Pen | ToolKind::Eraser) {
            self.tool.kind = ToolKind::Pen;
        }
        self.pad.edit = Some(PadEdit {
            session,
            slide,
            layer,
            shape,
            key,
            original: s,
            frame,
            walk: None,
            prev_tool,
            // A lifted old pad's new pixels must reach the slide even if
            // nothing is drawn.
            saved_edits: if lifted { u64::MAX } else { 0 },
            saved_at: Instant::now(),
            paper,
            paper_changed: false,
        });
        if lifted {
            self.set_pad_paper(pad::WHITE, false);
        }
        self.overlay_cache.set_pad(self.pad.shown());
        self.toast = Some(("Drawing on the picture: pen, pencil, Ctrl to erase. Esc when done".to_string(), Instant::now()));
        true
    }

    /// Put the open pad away: its pixels go into the slide, one undo step
    /// there for the whole time it was open. `restore_tool` gives back the
    /// tool it was opened from (Esc does; a tool picked to leave it stays).
    pub(super) fn close_pad(&mut self, restore_tool: bool) {
        let Some(mut e) = self.pad.edit.take() else { return };
        self.pad.retired.push(e.session.texture_id());
        self.overlay_cache.set_pad(None);
        let saved = self.pad_write(&mut e);
        if e.session.edits > 0 || e.paper_changed {
            match saved {
                Some(at) => {
                    let original = e.original.clone();
                    let layer = e.layer;
                    self.edit_pad_slide(e.slide, |c| {
                        if let Some(now) = c.layers.get(layer).and_then(|l| l.shapes.get(at)).cloned() {
                            c.undo_log.push(crate::canvas::canvas::UndoEntry::ShapeReplaced(layer, at, original, now));
                            c.redo_log.clear();
                        }
                    });
                }
                None => self.toast = Some(("Could not save the drawing into the slide".to_string(), Instant::now())),
            }
        }
        if restore_tool {
            self.tool.kind = e.prev_tool;
        }
    }

    /// Run `f` on slide `slide` (under its own view).
    fn edit_pad_slide<R>(&mut self, slide: usize, f: impl FnOnce(&mut crate::canvas::canvas::Canvas) -> R) -> Option<R> {
        let (pan, zoom) = self.canvases.slide(slide).map(|c| (c.pan, c.zoom))?;
        self.canvases.edit_slide_in_view(slide, pan, zoom, f)
    }

    /// Write the pad's pixels into its picture in the slide, with no undo
    /// step, if they changed since last time. Where the picture is then,
    /// `None` if it could not be done.
    fn pad_write(&mut self, e: &mut PadEdit) -> Option<usize> {
        e.session.end();
        let at = find(self.canvases.slide(e.slide)?.layers.get(e.layer)?, e.key, e.shape)?;
        if e.session.edits == e.saved_edits {
            return Some(at);
        }
        let started = Instant::now();
        let pixels = e.session.read_pixels()?;
        let [w, h] = e.session.size();
        let png = Arc::new(pad::encode_png(&image::RgbaImage::from_raw(w, h, pixels)?)?);
        let key = Arc::as_ptr(&png) as usize;
        let layer = e.layer;
        self.edit_pad_slide(e.slide, |c| {
            let l = c.layers.get_mut(layer)?;
            let Some(Shape::Raster { png: p, px_size, picture, .. }) = l.shapes.get_mut(at) else { return None };
            *p = png;
            *px_size = [w, h];
            // The pixels are the slide's own now; the picture store
            // moves big ones to the media folder again.
            *picture = None;
            l.rev = l.rev.wrapping_add(1);
            Some(())
        })??;
        e.key = key;
        e.shape = at;
        e.saved_edits = e.session.edits;
        e.saved_at = Instant::now();
        log::info!("pad {w}x{h} written into slide {} in {:?}", e.slide + 1, started.elapsed());
        Some(at)
    }

    /// A pen or mouse step, in canvas units. `true` when the pad took it:
    /// a pad is open, or a press outside one just put it away.
    pub(super) fn pad_pointer(&mut self, ev: PenEvent, erase: bool) -> bool {
        if self.pad.swallow {
            if matches!(ev, PenEvent::Up(_)) {
                self.pad.swallow = false;
            }
            return true;
        }
        if self.pad.edit.is_none() {
            return false;
        }
        if let PenEvent::Down(s) = ev {
            if !self.pad.edit.as_ref().is_some_and(|e| e.frame.contains(s.pos)) {
                self.close_pad(false);
                self.pad.swallow = true;
                return true;
            }
        }
        let brush = if erase || self.tool.kind == ToolKind::Eraser {
            Brush::Eraser
        } else {
            match self.tool.style {
                StrokeStyle::Pencil | StrokeStyle::Airbrush => Brush::Pencil,
                _ => Brush::Pen,
            }
        };
        // The tool's size is on screen: a diameter, the eraser's a radius.
        let zoom = self.canvases.active().zoom.max(1e-3);
        let on_screen = if brush == Brush::Eraser { self.tool.size.max(2.0) } else { self.tool.size * 0.5 };
        let color = self.tool.color;
        let curve = crate::canvas::stroke::pressure_curve();
        let Some(e) = self.pad.edit.as_mut() else { return false };
        let r_max = on_screen / zoom * e.frame.px_per_unit();
        let mut stamps = Vec::new();
        match ev {
            PenEvent::Down(s) => {
                let mut walk = Walk::new(brush, r_max);
                e.session.begin(brush, color);
                walk.step(e.frame.to_px(s.pos), crate::canvas::stroke::apply_curve(s.pressure, curve), &mut stamps);
                e.walk = Some(walk);
            }
            PenEvent::Move(s) | PenEvent::Up(s) => {
                if let Some(walk) = e.walk.as_mut() {
                    walk.step(e.frame.to_px(s.pos), crate::canvas::stroke::apply_curve(s.pressure, curve), &mut stamps);
                }
            }
        }
        e.session.push(&stamps);
        if matches!(ev, PenEvent::Up(_)) && e.walk.take().is_some() {
            e.session.end();
            if e.saved_at.elapsed() >= SNAPSHOT_EVERY {
                let mut taken = self.pad.edit.take().expect("open");
                let _ = self.pad_write(&mut taken);
                self.pad.edit = Some(taken);
            }
        }
        true
    }

    /// Draw this frame's stamps (after the pen and mouse were handed out).
    pub(super) fn pad_flush(&mut self) {
        if let Some(e) = self.pad.edit.as_mut() {
            e.session.flush();
        }
    }

    /// Take back (or redo) the open pad's last stroke.
    pub(super) fn pad_undo(&mut self, redo: bool) {
        if let Some(e) = self.pad.edit.as_mut() {
            e.session.undo(redo);
        }
    }

    /// While a pad is open: Esc puts it away, undo and redo step through
    /// its strokes. Taken out of the frame's keys before anything else
    /// sees them.
    pub(super) fn pad_keys(&mut self, ctx: &egui::Context) {
        if self.pad.edit.is_none() || ctx.wants_keyboard_input() {
            return;
        }
        let shortcut = |b: Option<crate::input::hotkeys_config::Binding>| {
            b.is_some_and(|b| ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(b.to_modifiers(), b.key))))
        };
        // Redo first: Ctrl+Z also matches Ctrl+Shift+Z.
        let redo = shortcut(self.hotkeys_cfg.canvas.redo);
        let undo = !redo && shortcut(self.hotkeys_cfg.canvas.undo);
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if redo || undo {
            self.pad_undo(redo);
        }
        if esc {
            self.close_pad(true);
        }
    }

    /// Give the open pad paper `p` (straight RGBA, alpha its opacity), in
    /// its slide at once. `remember` keeps it for the next new pad.
    fn set_pad_paper(&mut self, p: [u8; 4], remember: bool) {
        let Some(e) = self.pad.edit.as_mut() else { return };
        e.paper = Some(p);
        e.paper_changed = true;
        let (slide, layer, key, hint) = (e.slide, e.layer, e.key, e.shape);
        self.edit_pad_slide(slide, |c| {
            let Some(l) = c.layers.get_mut(layer) else { return };
            let Some(i) = find(l, key, hint) else { return };
            if let Shape::Raster { paper, .. } = &mut l.shapes[i] {
                *paper = Some(p);
            }
            l.rev = l.rev.wrapping_add(1);
        });
        if remember {
            self.config.pad_paper = p;
        }
    }

    /// A line round the open pad, so it is clear where drawing goes, and
    /// for a pad the paper bar: its colour (a few one tap away, or any)
    /// and opacity.
    pub(super) fn pad_chrome(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        let Some(e) = self.pad.edit.as_ref() else { return };
        let c = self.canvases.active();
        let to_screen = |p: [f32; 2]| egui::pos2(canvas_rect.min.x + c.pan[0] + p[0] * c.zoom, canvas_rect.min.y + c.pan[1] + p[1] * c.zoom);
        let corners: Vec<egui::Pos2> = e.frame.obb.corners().into_iter().map(to_screen).collect();
        // On the canvas's own layer, after it: over the picture, under the
        // toolbar and panels.
        let painter = ctx.layer_painter(egui::LayerId::background()).with_clip_rect(canvas_rect);
        painter.add(egui::Shape::closed_line(corners.clone(), egui::Stroke::new(3.0, egui::Color32::from_black_alpha(140))));
        painter.add(egui::Shape::closed_line(corners.clone(), egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 176, 32))));

        let Some(paper) = e.paper else { return };
        // Just above the pad's top-left corner, or inside it when the
        // toolbar is in the way; always on screen.
        let top_left = corners.iter().fold(egui::pos2(f32::MAX, f32::MAX), |a, p| egui::pos2(a.x.min(p.x), a.y.min(p.y)));
        let above = egui::pos2(top_left.x, top_left.y - 44.0);
        let mut at = if above.y >= canvas_rect.min.y + 56.0 { above } else { top_left + egui::vec2(8.0, 8.0) };
        at.x = at.x.clamp(canvas_rect.min.x + 8.0, (canvas_rect.max.x - 400.0).max(canvas_rect.min.x + 8.0));
        at.y = at.y.clamp(canvas_rect.min.y + 8.0, (canvas_rect.max.y - 48.0).max(canvas_rect.min.y + 8.0));
        let mut picked: Option<[u8; 4]> = None;
        egui::Area::new(egui::Id::new("osn_pad_paper"))
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
                        ui.label(egui::RichText::new("Paper").color(ui::theme::TEXT_PRIMARY));
                        for (rgb, name) in PAPERS {
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                            let on = rgb == [paper[0], paper[1], paper[2]];
                            let p = ui.painter();
                            p.circle_filled(rect.center(), 7.5, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                            let ring = if on { egui::Color32::from_rgb(255, 176, 32) } else { egui::Color32::from_white_alpha(70) };
                            p.circle_stroke(rect.center(), 8.5, egui::Stroke::new(if on { 2.0 } else { 1.0 }, ring));
                            if resp.on_hover_text(name).clicked() {
                                picked = Some([rgb[0], rgb[1], rgb[2], paper[3]]);
                            }
                        }
                        let mut rgb = [paper[0], paper[1], paper[2]];
                        if egui::widgets::color_picker::color_edit_button_srgb(ui, &mut rgb).on_hover_text("Any colour").changed() {
                            picked = Some([rgb[0], rgb[1], rgb[2], paper[3]]);
                        }
                        ui.spacing_mut().slider_width = 110.0;
                        let mut pct = (paper[3] as f32 / 255.0 * 100.0).round();
                        let slider = egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%").integer();
                        if ui.add(slider).on_hover_text("Paper opacity: 0% is no paper, only the ink").changed() {
                            picked = Some([paper[0], paper[1], paper[2], (pct * 2.55).round().clamp(0.0, 255.0) as u8]);
                        }
                    });
                });
            });
        if let Some(p) = picked {
            self.set_pad_paper(p, true);
        }
    }
}
