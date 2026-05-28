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

/// Per-frame state retained across frames. Holds GPU texture handles
/// for image-backed layers (frozen frames) keyed by
/// `(canvas_idx, layer_idx)`; the cached PNG length is a cheap
/// fingerprint that triggers a re-upload when the user re-freezes a
/// new frame into the same layer slot.
#[derive(Default)]
pub struct OverlayCache {
    images: HashMap<(usize, usize), (usize, egui::TextureHandle)>,
    /// Per-shape texture cache for `Shape::Raster` fills. Keyed by
    /// `(canvas, layer, shape_index)`; value carries the cached PNG
    /// length as a fingerprint for invalidation.
    shape_images: HashMap<(usize, usize, usize), (usize, egui::TextureHandle)>,
}

impl OverlayCache {
    pub fn new() -> Self { Self::default() }

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
            Some((cached_len, _)) => *cached_len != image.png.len(),
            None => true,
        };
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
            self.images.insert(key, (image.png.len(), handle));
        }
        self.images.get(&key).map(|(_, h)| h.id())
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
        png: &[u8],
    ) -> Option<egui::TextureId> {
        let key = (canvas_idx, layer_idx, shape_idx);
        let needs_upload = match self.shape_images.get(&key) {
            Some((cached_len, _)) => *cached_len != png.len(),
            None => true,
        };
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
            self.shape_images.insert(key, (png.len(), handle));
        }
        self.shape_images.get(&key).map(|(_, h)| h.id())
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
) -> egui::Rect {
    // Pre-build per-layer image-texture IDs for the active canvas
    // before opening the panel so the borrow on `cache` is released
    // before the renderer runs. Scope the temporary so the `active`
    // borrow of `manager` ends before the closure below re-borrows.
    let canvas_idx = manager.active_index();
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
    let shape_textures: Vec<Vec<Option<egui::TextureId>>> = {
        let active = manager.active();
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
                        crate::canvas::shape::Shape::Raster { png, .. } => {
                            cache.ensure_shape_texture(ctx, canvas_idx, li, si, png)
                        }
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
                // Repaint each frame so the spotlight tracks live.
                ctx.request_repaint();
            }

            // Lasso-erase preview — dashed grey loop tracking the
            // user's in-progress drag. We paint in screen-space, so
            // we map each canvas-local sample through the active
            // canvas's pan/zoom. The first sample is also appended at
            // the end so the loop reads as closed during the drag —
            // gives the user a clear "what you'll cut" preview.
            if matches!(tool.kind, ToolKind::LassoErase) {
                if let Some(poly) = tool.in_progress_lasso.as_ref() {
                    if poly.len() >= 2 {
                        let canvas = manager.active();
                        let zoom = canvas.zoom;
                        let ox = rect.min.x + canvas.pan[0];
                        let oy = rect.min.y + canvas.pan[1];
                        let mut pts: Vec<egui::Pos2> = poly
                            .iter()
                            .map(|p| egui::pos2(ox + p[0] * zoom, oy + p[1] * zoom))
                            .collect();
                        // Close the loop visually.
                        pts.push(pts[0]);
                        let stroke = egui::Stroke::new(
                            1.5,
                            egui::Color32::from_rgba_unmultiplied(220, 222, 230, 220),
                        );
                        render::paint_dashed_polyline(ui.painter(), &pts, stroke, zoom);
                        // Animated dashes need repaint each frame.
                        ctx.request_repaint();
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
