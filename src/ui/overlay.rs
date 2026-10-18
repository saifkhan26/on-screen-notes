//! Main overlay: a transparent borderless central panel that paints the
//! current canvas's strokes + shapes via the GPU and accepts pen / mouse
//! input.
//!
//! Render path: `canvas::render::paint_canvas_egui` — direct egui Painter
//! calls, zero CPU rasterisation, zero per-frame texture upload. This is
//! the change that fixed the laggy-pen issue: previously we rasterised
//! into a tiny-skia pixmap and re-uploaded a multi-megabyte texture every
//! frame.

use crate::canvas::CanvasManager;
use crate::canvas::render;
use crate::canvas::stroke::{PenSample, Stroke};
use crate::tools::{ActiveTool, ToolKind};
use crate::ui::loupe::LoupeState;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-frame state retained across frames. Holds GPU texture handles
/// for image-backed layers (frozen frames) keyed by
/// `(canvas_idx, layer_idx)`; the cached PNG length is a cheap
/// fingerprint that triggers a re-upload when the user re-freezes a
/// new frame into the same layer slot.
#[derive(Default)]
pub struct OverlayCache {
    /// Each texture here also carries when, and in which `show` call, it
    /// was last drawn; see `evict_idle`.
    images: HashMap<(usize, usize), (usize, egui::TextureHandle, Used)>,
    /// Per-shape texture cache for `Shape::Raster` fills. Keyed by
    /// `(canvas, layer, shape_index)`; value carries `(PNG length,
    /// address of the shared PNG buffer)` as a fingerprint. The address
    /// is what catches a *different* image arriving at the same index —
    /// deleting, re-ordering or pasting shapes shifts indices, and two
    /// images can easily differ without differing in byte length.
    shape_images: HashMap<(usize, usize, usize), ((usize, usize), egui::TextureHandle, Used)>,
    /// Number of `show` calls so far.
    shows: u64,
    /// Per-layer preview thumbnails — keyed by `(canvas, layer)`. The
    /// fingerprint is `(strokes_len, shapes_len, image_png_len,
    /// layer.rev)` and is cheap to recompute every frame; mismatch
    /// triggers a re-render + re-upload. `rev` covers edits that move
    /// or restyle objects without changing how many there are.
    layer_thumbs: HashMap<(usize, usize), ((usize, usize, usize, u64), egui::TextureHandle)>,
    /// Full-size pixels of stored pictures, loaded at the size shown.
    pictures: crate::ui::picture_cache::PictureCache,
    /// Where the canvas was drawn last frame, to tell which pictures are
    /// on screen before this frame's layout runs.
    last_canvas_rect: Option<egui::Rect>,
}

/// When a texture was last drawn: the `show` call and the time.
#[derive(Clone, Copy)]
struct Used(u64, Instant);

/// A picture or frozen frame of a slide not drawn for this long (and for
/// `KEEP_SHOWS` frames) gives its texture back. Without this, every slide
/// visited kept its full-size pictures on the GPU for the rest of the
/// run — one media-heavy slide alone holds close to 1 GB.
const KEEP_FOR: Duration = Duration::from_secs(60);
/// Frames count too, so hiding the overlay for a while (no frames) does
/// not throw away the active slide's pictures.
const KEEP_SHOWS: u64 = 600;

impl OverlayCache {
    pub fn new() -> Self { Self::default() }

    /// Drop the textures of pictures and frozen frames that have not been
    /// drawn for a while. Runs before anything is drawn in a frame, so a
    /// texture is never made and freed in the same frame (egui-wgpu
    /// aborts on that; see `PictureCache::begin_frame`).
    fn evict_idle(&mut self) {
        self.shows += 1;
        let (shows, now) = (self.shows, Instant::now());
        let keep = |u: &Used| shows.saturating_sub(u.0) <= KEEP_SHOWS || now.saturating_duration_since(u.1) <= KEEP_FOR;
        self.images.retain(|_, (_, _, u)| keep(u));
        self.shape_images.retain(|_, (_, _, u)| keep(u));
    }

    /// Stored pictures are still loading at their on-screen size.
    pub fn pictures_busy(&self) -> bool {
        self.pictures.busy()
    }

    /// For the perf log: picture cache state, and the per-slide textures
    /// kept for inline pictures and frozen frames.
    pub fn stats(&self) -> String {
        let shape_mb: usize = self.shape_images.values().map(|(_, t, _)| t.byte_size()).sum::<usize>() >> 20;
        let plate_mb: usize = self.images.values().map(|(_, t, _)| t.byte_size()).sum::<usize>() >> 20;
        format!("{} | inline {} tex {shape_mb} MB, plates {} tex {plate_mb} MB", self.pictures.stats(), self.shape_images.len(), self.images.len())
    }

    /// Where the canvas was drawn last frame (the whole screen before the
    /// first frame).
    pub fn canvas_rect(&self, ctx: &egui::Context) -> egui::Rect {
        self.last_canvas_rect.unwrap_or_else(|| ctx.screen_rect())
    }

    /// Ensure a thumbnail texture exists for the given layer. Returns
    /// `None` for empty layers (no strokes / shapes / image) so the
    /// hover popup can render a placeholder instead. Fingerprint =
    /// (strokes_len, shapes_len, image_png_len) — cheap to compute
    /// each frame and re-renders only when the layer actually changed.
    pub fn ensure_layer_thumb(
        &mut self,
        ctx: &egui::Context,
        canvas_idx: usize,
        layer_idx: usize,
        layer: &crate::canvas::canvas::Layer,
    ) -> Option<egui::TextureId> {
        let fp = (
            layer.strokes.len(),
            layer.shapes.len(),
            layer.image.as_ref().map(|i| i.png.len()).unwrap_or(0),
            layer.rev,
        );
        let key = (canvas_idx, layer_idx);
        let cached_ok = match self.layer_thumbs.get(&key) {
            Some((cached_fp, _)) => *cached_fp == fp,
            None => false,
        };
        if cached_ok {
            return Some(self.layer_thumbs[&key].1.id());
        }
        // 240×180 = 4:3, big enough to read, small enough to render
        // in <5 ms on the UI thread.
        let img = crate::canvas::render::render_layer_thumbnail(layer, 240, 180)?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let pixels: Vec<egui::Color32> = img
            .into_raw()
            .chunks_exact(4)
            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
            .collect();
        let tex = ctx.load_texture(
            format!("osn_layer_thumb_{canvas_idx}_{layer_idx}"),
            egui::ColorImage { size: [w, h], pixels },
            egui::TextureOptions::LINEAR,
        );
        let id = tex.id();
        self.layer_thumbs.insert(key, (fp, tex));
        Some(id)
    }

    /// Ensure a texture exists for the given layer's image. Uploads
    /// (or re-uploads on size mismatch) when needed. Returns a
    /// `TextureId` suitable for `Painter::image`.
    fn ensure_texture(
        &mut self,
        ctx: &egui::Context,
        canvas_idx: usize,
        layer_idx: usize,
        image: &crate::canvas::canvas::LayerImage,
    ) -> Option<egui::TextureId> {
        let key = (canvas_idx, layer_idx);
        let needs_upload = match self.images.get(&key) {
            Some((cached_len, _, _)) => *cached_len != image.png.len(),
            None => true,
        };
        let used = Used(self.shows, Instant::now());
        if needs_upload {
            let decoded = match image::load_from_memory(&image.png) {
                Ok(d) => d.to_rgba8(),
                Err(e) => {
                    log::warn!("decode layer image failed: {e}");
                    return None;
                }
            };
            let (w, h) = (decoded.width() as usize, decoded.height() as usize);
            let pixels: Vec<egui::Color32> = decoded
                .into_raw()
                .chunks_exact(4)
                .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                .collect();
            let color_image = egui::ColorImage { size: [w, h], pixels };
            let handle = ctx.load_texture(
                format!("osn_layer_image_{canvas_idx}_{layer_idx}"),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            self.images.insert(key, (image.png.len(), handle, used));
        }
        self.images.get_mut(&key).map(|(_, h, u)| {
            *u = used;
            h.id()
        })
    }

    /// Same upload-on-mismatch pattern for `Shape::Raster` PNG bytes.
    /// Cache key includes the shape's position in the layer, so
    /// reordering or deletion can leak stale entries — acceptable
    /// for the rare flood-fill action.
    fn ensure_shape_texture(
        &mut self,
        ctx: &egui::Context,
        canvas_idx: usize,
        layer_idx: usize,
        shape_idx: usize,
        png: &std::sync::Arc<Vec<u8>>,
    ) -> Option<egui::TextureId> {
        let key = (canvas_idx, layer_idx, shape_idx);
        let fingerprint = (png.len(), std::sync::Arc::as_ptr(png) as usize);
        let needs_upload = match self.shape_images.get(&key) {
            Some((cached, _, _)) => *cached != fingerprint,
            None => true,
        };
        let used = Used(self.shows, Instant::now());
        if needs_upload {
            let decoded = match image::load_from_memory(png) {
                Ok(d) => d.to_rgba8(),
                Err(e) => {
                    log::warn!("decode raster shape failed: {e}");
                    return None;
                }
            };
            let (w, h) = (decoded.width() as usize, decoded.height() as usize);
            let pixels: Vec<egui::Color32> = decoded
                .into_raw()
                .chunks_exact(4)
                .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                .collect();
            let color_image = egui::ColorImage { size: [w, h], pixels };
            let handle = ctx.load_texture(
                format!("osn_raster_{canvas_idx}_{layer_idx}_{shape_idx}"),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            self.shape_images.insert(key, (fingerprint, handle, used));
        }
        self.shape_images.get_mut(&key).map(|(_, h, u)| {
            *u = used;
            h.id()
        })
    }
}

/// Render the central canvas area. Returns the rect we drew into so callers
/// (mouse-fallback logic, scroll handling) can convert cursor coords.
pub fn show(
    ctx: &egui::Context,
    cache: &mut OverlayCache,
    manager: &CanvasManager,
    tool: &ActiveTool,
    position_passes: usize,
    chrome_visible: bool,
    loupe: Option<&LoupeState>,
    // In-progress Shift+Space layer gesture — `(layer index, scale,
    // canvas-local offset)`. Forwarded straight to the renderer so the
    // dragged / zoomed layer previews at its new position and size
    // before the transform is committed.
    layer_xform: Option<(usize, f32, [f32; 2])>,
    // Whether to draw the Select tool's frame and handles. False while
    // a screenshot / capture / recording is grabbing the screen, so the
    // handles never end up in the picture.
    show_selection: bool,
    // Live frames for videos and GIFs; their poster frame stands in
    // until a player has one.
    media: &crate::media::MediaHub,
) -> egui::Rect {
    // Select-tool gesture in flight: the selected objects preview under
    // its transform until pen-up commits it.
    let sel_preview = if tool.kind == ToolKind::Select {
        let canvas = manager.active();
        tool.select
            .preview_affine(canvas.zoom)
            .map(|m| (canvas.active_layer, &tool.select.sel, m))
    } else {
        None
    };
    // Pre-build per-layer image-texture IDs for the active canvas
    // before opening the panel so the borrow on `cache` is released
    // before the renderer runs. Scope the temporary so the `active`
    // borrow of `manager` ends before the closure below re-borrows.
    let canvas_idx = manager.active_index();
    cache.evict_idle();
    let layer_image_textures: Vec<Option<egui::TextureId>> = {
        let active = manager.active();
        active
            .layers
            .iter()
            .enumerate()
            .map(|(li, layer)| {
                layer
                    .image
                    .as_ref()
                    .and_then(|img| cache.ensure_texture(ctx, canvas_idx, li, img))
            })
            .collect()
    };
    cache.pictures.begin_frame(ctx);
    let shape_textures: Vec<Vec<Option<egui::TextureId>>> = {
        let active = manager.active();
        // On-screen footprint of a shape, for stored pictures: how big
        // to load it, and whether to load it at all.
        let view = cache.last_canvas_rect.unwrap_or_else(|| ctx.screen_rect());
        let ppp = ctx.pixels_per_point();
        let (pan, zoom) = (active.pan, active.zoom);
        let footprint = |shape: &crate::canvas::shape::Shape| {
            let obb = shape.obb();
            let mut r = egui::Rect::NOTHING;
            for p in obb.corners() {
                r.extend_with(egui::pos2(view.min.x + pan[0] + p[0] * zoom, view.min.y + pan[1] + p[1] * zoom));
            }
            let long = obb.half[0].max(obb.half[1]) * 2.0 * zoom * ppp;
            (long, r.intersects(view.expand(view.width().max(view.height()) * 0.25)))
        };
        active
            .layers
            .iter()
            .enumerate()
            .map(|(li, layer)| {
                layer
                    .shapes
                    .iter()
                    .enumerate()
                    .map(|(si, shape)| match shape {
                        crate::canvas::shape::Shape::Raster { png, media: m, picture, .. } => match (m, picture) {
                            (Some(m), _) => media.texture(m.id).or_else(|| cache.ensure_shape_texture(ctx, canvas_idx, li, si, png)),
                            (None, Some(p)) => {
                                let (long, on_screen) = footprint(shape);
                                cache.pictures.texture(ctx, p, png, long, on_screen && layer.visible)
                            }
                            (None, None) => cache.ensure_shape_texture(ctx, canvas_idx, li, si, png),
                        },
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    };
    let mut canvas_rect = egui::Rect::NOTHING;
    egui::CentralPanel::default()
        .frame(egui::Frame::none())
        .show(ctx, |ui| {
            // Allocate the full panel area so we get a stable rect.
            let avail = ui.available_size();
            let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
            canvas_rect = rect;
            cache.last_canvas_rect = Some(rect);

            // Empty-canvas hint — appears only when the canvas has no
            // strokes / no shapes. Disappears the moment the user starts
            // drawing. Subtle so it doesn't compete with their work.
            let active = manager.active();
            let any_content = active
                .layers
                .iter()
                .any(|l| !l.strokes.is_empty() || !l.shapes.is_empty());
            if !any_content && chrome_visible {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Start drawing — Alt to pass clicks through · ←/→ to switch canvas",
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 80),
                );
            }

            // Paint canvas content directly via egui Painter (GPU triangles).
            // Origin = top-left of the canvas rect; canvas's own pan/zoom is
            // applied inside `paint_canvas_egui`.
            let preview_shape = tool.preview_shape();

            // Pen prediction: build a "look-ahead" extension of the live
            // stroke so the visible ink reaches slightly *past* the most
            // recent pen sample. This compensates for the one-frame display
            // delay and is what makes Sticky Notes / OneNote feel zero-lag.
            //
            // We clone the in-progress stroke (cheap — a few hundred
            // samples) and append 1-2 extrapolated points based on the
            // recent velocity. The original stroke (in `tool.in_progress`)
            // is untouched, so committed samples remain pure user input.
            let predicted = tool.in_progress_stroke.as_ref().map(predict_ahead);
            let preview_stroke_ref = predicted.as_ref();

            render::paint_canvas_egui(
                ui.painter(),
                rect.min,
                manager.active(),
                preview_shape.as_ref(),
                preview_stroke_ref,
                position_passes,
                &tool.laser_strokes,
                &layer_image_textures,
                &shape_textures,
                None,
                layer_xform,
                sel_preview,
            );

            // Live preview of the in-progress text edit. Painted via
            // egui after canvas content so it always sits on top, and
            // because the screenshot path can't render glyphs yet the
            // user only sees their typing here, not in saved PNGs.
            if let Some(text_edit) = tool.in_progress_text.as_ref() {
                let canvas = manager.active();
                let zoom = canvas.zoom;
                let pan_x = rect.min.x + canvas.pan[0];
                let pan_y = rect.min.y + canvas.pan[1];
                let to_screen = |p: [f32; 2]| {
                    egui::pos2(pan_x + p[0] * zoom, pan_y + p[1] * zoom)
                };
                let size = (text_edit.font_size * zoom).max(2.0);
                let line_h = size * 1.25;
                let color = egui::Color32::from_rgba_unmultiplied(
                    text_edit.color[0], text_edit.color[1], text_edit.color[2], text_edit.color[3],
                );
                let font_id = egui::FontId::new(size, egui::FontFamily::Name("handwritten".into()));

                let top_left = to_screen(text_edit.pos);
                let lines: Vec<&str> = text_edit.buffer.split('\n').collect();

                // Translucent caret guide background so the user
                // sees exactly where typing will land before they
                // start.
                let guide_alpha = 32;
                let guide_color = egui::Color32::from_rgba_unmultiplied(
                    text_edit.color[0], text_edit.color[1], text_edit.color[2], guide_alpha,
                );
                let guide_w = ((lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32)
                    * size * 0.55)
                    .max(size * 0.6);
                let guide_h = line_h * lines.len().max(1) as f32;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(top_left, egui::vec2(guide_w, guide_h)),
                    egui::Rounding::same(2.0),
                    guide_color,
                );

                // Paint the typed lines.
                for (i, line) in lines.iter().enumerate() {
                    let lp = egui::pos2(top_left.x, top_left.y + i as f32 * line_h);
                    ui.painter().text(lp, egui::Align2::LEFT_TOP, *line, font_id.clone(), color);
                }

                // Blinking caret at the end of the last line. Use a
                // 500-ms duty cycle reading the system clock so the
                // blink doesn't rely on per-frame state.
                let visible = {
                    use std::time::Instant;
                    use std::sync::OnceLock;
                    static BOOT: OnceLock<Instant> = OnceLock::new();
                    let boot = *BOOT.get_or_init(Instant::now);
                    let ms = boot.elapsed().as_millis();
                    (ms / 500) % 2 == 0
                };
                if visible {
                    let last_line = lines.last().copied().unwrap_or("");
                    // Approximate caret x via character count × 0.55 ×
                    // font size — close enough for a blinking bar.
                    let caret_x = top_left.x + last_line.chars().count() as f32 * size * 0.55;
                    let caret_y = top_left.y + (lines.len().saturating_sub(1)) as f32 * line_h;
                    ui.painter().line_segment(
                        [
                            egui::pos2(caret_x, caret_y),
                            egui::pos2(caret_x, caret_y + size),
                        ],
                        egui::Stroke::new(1.6, color),
                    );
                }
            }

            // Spotlight tool — dim every region of the overlay except
            // a circular area around the pointer. egui has no
            // "subtract" compositing; we build a ring mesh whose
            // inner edge sits at the spotlight radius and whose
            // outer edge extends far beyond the canvas. Clipped to
            // the canvas rect so the overflow does not bleed onto
            // other panels.
            if matches!(tool.kind, ToolKind::Spotlight) && chrome_visible {
                let dim = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 170);
                let cursor = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
                let r_in = tool.spotlight_size.max(20.0);
                // Outer radius covers the whole canvas no matter
                // where the cursor sits inside it.
                let r_out = (rect.width().abs() + rect.height().abs()) * 2.0;
                const N: usize = 64;
                let mut mesh = egui::Mesh::default();
                for i in 0..=N {
                    let theta = (i as f32 / N as f32) * std::f32::consts::TAU;
                    let (s, c) = theta.sin_cos();
                    let inner = egui::pos2(cursor.x + r_in * c, cursor.y + r_in * s);
                    let outer = egui::pos2(cursor.x + r_out * c, cursor.y + r_out * s);
                    mesh.colored_vertex(inner, dim);
                    mesh.colored_vertex(outer, dim);
                }
                for i in 0..N {
                    let base = (i * 2) as u32;
                    let next = ((i + 1) * 2) as u32;
                    mesh.add_triangle(base, base + 1, next);
                    mesh.add_triangle(base + 1, next + 1, next);
                }
                let p = ui.painter_at(rect);
                p.add(egui::Shape::mesh(mesh));
                // No repaint request needed to track the pointer: every
                // pointer move is an input event, which repaints at once.
            }

            // Lasso preview — dashed grey loop tracking the user's
            // in-progress path. We paint in screen-space, so we map each
            // canvas-local point through the active canvas's pan/zoom.
            // The first point is also appended at the end so the loop
            // reads as closed — gives the user a clear "what you'll cut"
            // preview. Between the clicks of a polygon the next edge
            // follows the pointer and the corners are dotted; when a
            // click would close the shape, the corner it would close on
            // is ringed and the loop shows the finished shape. The app
            // keeps frames coming for the marching dashes while a path
            // is open.
            if matches!(tool.kind, ToolKind::LassoErase | ToolKind::LassoSelect) {
                let lasso = &tool.lasso;
                if !lasso.points().is_empty() {
                    let canvas = manager.active();
                    let zoom = canvas.zoom;
                    let ox = rect.min.x + canvas.pan[0];
                    let oy = rect.min.y + canvas.pan[1];
                    let to_screen = |p: [f32; 2]| egui::pos2(ox + p[0] * zoom, oy + p[1] * zoom);
                    let mut pts: Vec<egui::Pos2> = lasso.points().iter().map(|p| to_screen(*p)).collect();
                    let mut ring = None;
                    if lasso.is_polygon() && !lasso.pressing() {
                        if let Some(h) = ctx.input(|i| i.pointer.hover_pos()) {
                            ring = lasso.closes_at([(h.x - ox) / zoom, (h.y - oy) / zoom], zoom);
                            if ring.is_none() {
                                pts.push(h);
                            }
                        }
                    }
                    let light = egui::Color32::from_rgba_unmultiplied(220, 222, 230, 220);
                    let under = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150);
                    let painter = ui.painter();
                    if pts.len() >= 2 {
                        // Close the loop visually.
                        pts.push(pts[0]);
                        render::paint_dashed_polyline(painter, &pts, egui::Stroke::new(1.5, light), zoom);
                    }
                    for c in lasso.corners() {
                        let c = to_screen(c);
                        painter.circle_filled(c, 3.5, under);
                        painter.circle_filled(c, 2.5, light);
                    }
                    if let Some(c) = ring {
                        let c = to_screen(c);
                        painter.circle_stroke(c, 7.0, egui::Stroke::new(3.0, under));
                        painter.circle_stroke(c, 7.0, egui::Stroke::new(1.5, light));
                    }
                }
            }

            // Select tool — frame, resize handles, rotate knob and the
            // rubber-band box. Drawn in screen space at a fixed size so
            // the handles stay the same size to grab at any zoom.
            if matches!(tool.kind, ToolKind::Select) && show_selection {
                let canvas = manager.active();
                let zoom = canvas.zoom;
                let ox = rect.min.x + canvas.pan[0];
                let oy = rect.min.y + canvas.pan[1];
                let to_screen = |p: [f32; 2]| egui::pos2(ox + p[0] * zoom, oy + p[1] * zoom);
                let chrome = tool.select.chrome(canvas);
                let painter = ui.painter();
                // The overlay sits on arbitrary desktop content, so every
                // line is a light stroke over a dark one: it reads on
                // both bright and dark backgrounds.
                let line = egui::Color32::from_rgb(86, 160, 255);
                let under = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150);

                if let Some(m) = chrome.marquee {
                    let r = egui::Rect::from_two_pos(to_screen([m[0], m[1]]), to_screen([m[2], m[3]]));
                    painter.rect_filled(r, egui::Rounding::ZERO, egui::Color32::from_rgba_unmultiplied(86, 160, 255, 28));
                    painter.rect_stroke(r, egui::Rounding::ZERO, egui::Stroke::new(1.0, line));
                }
                if let Some(frame) = chrome.frame {
                    let pts: Vec<egui::Pos2> = frame.iter().map(|p| to_screen(*p)).collect();
                    painter.add(egui::Shape::closed_line(pts.clone(), egui::Stroke::new(3.0, under)));
                    painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.4, line)));
                    if let Some((top, knob)) = chrome.rotate_stem {
                        let seg = [to_screen(top), to_screen(knob)];
                        painter.line_segment(seg, egui::Stroke::new(3.0, under));
                        painter.line_segment(seg, egui::Stroke::new(1.4, line));
                    }
                    for (p, handle) in &chrome.handles {
                        let p = to_screen(*p);
                        match handle {
                            crate::tools::select_tool::Handle::Rotate => {
                                painter.circle_filled(p, 6.5, under);
                                painter.circle_filled(p, 5.0, egui::Color32::WHITE);
                                painter.circle_stroke(p, 5.0, egui::Stroke::new(1.4, line));
                            }
                            crate::tools::select_tool::Handle::Scale { .. } => {
                                let outer = egui::Rect::from_center_size(p, egui::vec2(11.0, 11.0));
                                let inner = egui::Rect::from_center_size(p, egui::vec2(8.0, 8.0));
                                painter.rect_filled(outer, egui::Rounding::same(2.0), under);
                                painter.rect_filled(inner, egui::Rounding::same(1.0), egui::Color32::WHITE);
                                painter.rect_stroke(inner, egui::Rounding::same(1.0), egui::Stroke::new(1.4, line));
                            }
                        }
                    }
                }
            }

            // Magnifier loupe — captured desktop image painted into
            // a square around the cursor, with the canvas content
            // re-rendered on top at the matching magnification so
            // the user sees both the desktop and their own ink in
            // the zoomed view. The captured texture (worker thread)
            // and the magnified canvas paint are kept in sync by
            // sharing the same pan/zoom override path.
            if let Some(loupe) = loupe {
                let cursor = ctx.input(|i| i.pointer.hover_pos());
                if let Some(c) = cursor {
                    let ppp = ctx.pixels_per_point();
                    let r = loupe.radius_px;
                    let dest = egui::Rect::from_min_max(
                        egui::pos2(c.x - r, c.y - r),
                        egui::pos2(c.x + r, c.y + r),
                    )
                    .intersect(rect);
                    if dest.width() > 0.0 && dest.height() > 0.0 {
                        let clipped = ui.painter_at(dest);
                        crate::ui::loupe::paint(&clipped, rect, loupe, c, ppp);
                        let zoom_m = loupe.zoom.max(1.0);
                        let active = manager.active();
                        let new_zoom = active.zoom * zoom_m;
                        // Cursor logical pos in canvas-viewport-local
                        // coords (i.e. relative to `rect.min`).
                        let rel_x = c.x - rect.min.x;
                        let rel_y = c.y - rect.min.y;
                        // Solve for new pan so that the canvas pixel
                        // currently under the cursor stays under the
                        // cursor after magnification.
                        let pan_x_new = rel_x * (1.0 - zoom_m) + active.pan[0] * zoom_m;
                        let pan_y_new = rel_y * (1.0 - zoom_m) + active.pan[1] * zoom_m;
                        render::paint_canvas_egui(
                            &clipped,
                            rect.min,
                            active,
                            None,
                            None,
                            position_passes,
                            &[],
                            &layer_image_textures,
                            &shape_textures,
                            Some(([pan_x_new, pan_y_new], new_zoom)),
                            layer_xform,
                            sel_preview,
                        );
                    }
                    ctx.request_repaint();
                }
            }

            // Cursor preview ring — shows the current pen size + colour at
            // the pointer position. Drawn only when the pointer is inside
            // the canvas, the user is *not* mid-stroke (so we don't double
            // up with the in-progress ink), and the active tool is one
            // where size/colour preview makes sense (pen / eraser /
            // freehand-arrow). For shape tools, a ring would be misleading.
            let pointer = ctx.input(|i| i.pointer.hover_pos());
            let preview_relevant = matches!(
                tool.kind,
                ToolKind::Pen | ToolKind::Eraser | ToolKind::FreehandArrow | ToolKind::Laser
            );
            // Ring paints regardless of `chrome_visible` so the
            // pointer indicator the user sees is identical whether
            // the UI is Tab-hidden or a recording is in flight —
            // matches the visible-UI experience.
            if preview_relevant && tool.in_progress_stroke.is_none() {
                if let Some(p) = pointer {
                    if rect.contains(p) {
                        let color = if tool.kind == ToolKind::Eraser {
                            // Eraser ring is grey-ish so it reads as "remove".
                            egui::Color32::from_rgb(220, 222, 230)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(
                                tool.color[0], tool.color[1], tool.color[2], 220,
                            )
                        };
                        let zoom = manager.active().zoom;
                        let r = (tool.size * 0.5).max(2.0) * zoom;
                        ui.painter().circle_stroke(p, r, egui::Stroke::new(1.2, color));
                    }
                }
            }
        });
    canvas_rect
}

/// Build a "predicted" extension of the live stroke by extrapolating from
/// the recent samples' velocity. Returns a clone of the input stroke with
/// 1-2 predicted samples appended.
///
/// We use a damped linear extrapolation: each predicted sample is the last
/// real sample plus the average velocity of the trailing window times a
/// dampening factor (so we don't over-shoot if the user suddenly stops).
fn predict_ahead(stroke: &Stroke) -> Stroke {
    let n = stroke.samples.len();
    let mut out = stroke.clone();

    if n < 2 {
        return out;
    }

    // Average velocity over the last `LOOKBACK` samples for stability.
    const LOOKBACK: usize = 3;
    let start = n.saturating_sub(LOOKBACK + 1);
    let mut vx_sum = 0.0;
    let mut vy_sum = 0.0;
    let mut count = 0;
    for i in start..(n - 1) {
        vx_sum += stroke.samples[i + 1].pos[0] - stroke.samples[i].pos[0];
        vy_sum += stroke.samples[i + 1].pos[1] - stroke.samples[i].pos[1];
        count += 1;
    }
    if count == 0 {
        return out;
    }
    let vx = vx_sum / count as f32;
    let vy = vy_sum / count as f32;

    // Dampening: 0.6 → predict ~0.6 sample-period ahead. With Wintab's
    // ~200Hz sample rate that's ~3ms, which roughly cancels one display
    // frame's worth of latency without overshooting on direction changes.
    let damp = 0.6_f32;

    let last = stroke.samples[n - 1];
    out.samples.push(PenSample {
        pos: [last.pos[0] + vx * damp, last.pos[1] + vy * damp],
        pressure: last.pressure,
        tilt: last.tilt,
    });
    // A second farther-out predicted sample: half-weight, so the predicted
    // tail tapers naturally into the unknown future.
    out.samples.push(PenSample {
        pos: [last.pos[0] + vx * damp * 1.6, last.pos[1] + vy * damp * 1.6],
        pressure: last.pressure * 0.85,
        tilt: last.tilt,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> std::sync::Arc<Vec<u8>> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(2, 2).write_to(&mut out, image::ImageFormat::Png).unwrap();
        std::sync::Arc::new(out.into_inner())
    }

    #[test]
    fn picture_textures_go_only_after_frames_and_time_both_pass() {
        let ctx = egui::Context::default();
        let mut cache = OverlayCache::new();
        let (a, b) = (png(), png());
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            cache.evict_idle();
            cache.ensure_shape_texture(ctx, 0, 0, 0, &a);
            cache.ensure_shape_texture(ctx, 1, 0, 0, &b);
        });
        let long_ago = Instant::now().checked_sub(KEEP_FOR * 2).expect("clock far enough along");
        // Slide 0: drawn long ago and many frames back. Slide 1: many
        // frames back but recently (the overlay was hidden meanwhile).
        cache.shape_images.get_mut(&(0, 0, 0)).unwrap().2 = Used(0, long_ago);
        cache.shape_images.get_mut(&(1, 0, 0)).unwrap().2 = Used(0, Instant::now());
        cache.shows = KEEP_SHOWS * 2;
        cache.evict_idle();
        assert!(!cache.shape_images.contains_key(&(0, 0, 0)), "idle slide's picture is let go");
        assert!(cache.shape_images.contains_key(&(1, 0, 0)), "recently drawn picture stays");
    }
}
