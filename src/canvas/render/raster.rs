//! The tiny-skia path, kept for pixels: screenshots (`render_canvas`), layer
//! thumbnails and pins, drawn into an RGBA pixmap.

use super::*;

// =============================================================================
// 2) Tiny-skia path: kept ONLY for the screenshot PNG pipeline.
// =============================================================================

/// Render a single layer into a small RGBA buffer for use as a
/// preview thumbnail.
///
/// Returns straight (un-premultiplied) RGBA so the caller can upload
/// it as an `egui::ColorImage` without further conversion. Returns
/// `None` when the layer has nothing renderable — the caller can
/// then skip the texture upload + show an "empty layer" placeholder
/// instead.
///
/// Bounds are inferred from the union of every stroke / shape / image
/// rect on the layer. The thumbnail is rendered at the largest scale
/// that fits the requested `out_w × out_h` while preserving aspect.
pub fn render_layer_thumbnail(
    layer: &crate::canvas::canvas::Layer,
    out_w: u32,
    out_h: u32,
) -> Option<image::RgbaImage> {
    // 1) Canvas-local bounds of every renderable on the layer.
    let [min_x, min_y, max_x, max_y] = layer.content_bounds()?;
    let bw = (max_x - min_x).max(1.0);
    let bh = (max_y - min_y).max(1.0);

    // 2) Scale to fit out_w × out_h preserving aspect, centred.
    let sx = out_w as f32 / bw;
    let sy = out_h as f32 / bh;
    let scale = sx.min(sy).max(1.0e-3);
    let used_w = bw * scale;
    let used_h = bh * scale;
    let ox = (out_w as f32 - used_w) * 0.5 - min_x * scale;
    let oy = (out_h as f32 - used_h) * 0.5 - min_y * scale;

    let mut pixmap = tiny_skia::Pixmap::new(out_w, out_h)?;
    pixmap.fill(Color::from_rgba8(20, 20, 24, 220));
    let t = Transform::from_translate(ox, oy).post_scale(scale, scale);

    // 3) Reuse the regular draw paths. They are agnostic of the
    // outer canvas's pan/zoom — they operate in whatever transform
    // we hand them.
    if let Some(img) = layer.image.as_ref() {
        if let Ok(decoded) = image::load_from_memory(&img.png) {
            let rgba = decoded.to_rgba8();
            let (iw, ih) = (rgba.width(), rgba.height());
            if let Some(mut src) = tiny_skia::Pixmap::new(iw, ih) {
                for (px, dst) in rgba.pixels().zip(src.pixels_mut()) {
                    let [r, g, b, a] = px.0;
                    let af = a as f32 / 255.0;
                    *dst = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r as f32 * af) as u8,
                        (g as f32 * af) as u8,
                        (b as f32 * af) as u8,
                        a,
                    ).unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0,0,0,0).unwrap());
                }
                let (w, h) = if img.logical_size[0] > 0.0 && img.logical_size[1] > 0.0 {
                    (img.logical_size[0], img.logical_size[1])
                } else {
                    (iw as f32, ih as f32)
                };
                let sx = w / iw as f32;
                let sy = h / ih as f32;
                let draw_t = Transform::from_scale(sx, sy)
                    .post_translate(img.canvas_origin[0], img.canvas_origin[1])
                    .post_concat(t);
                let opts = tiny_skia::PixmapPaint {
                    opacity: 1.0,
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    quality: tiny_skia::FilterQuality::Bilinear,
                };
                pixmap.as_mut().draw_pixmap(0, 0, src.as_ref(), &opts, draw_t, None);
            }
        }
    }
    let mut view = pixmap.as_mut();
    for shape in &layer.shapes {
        draw_shape(&mut view, shape, t, 1.0, None);
    }
    for stroke in &layer.strokes {
        draw_stroke(&mut view, stroke, t);
    }

    // 4) tiny-skia outputs premultiplied; unpremultiply to straight
    //    so the egui ColorImage upload doesn't double-darken.
    let raw = pixmap.data().to_vec();
    let mut img = image::RgbaImage::from_raw(out_w, out_h, raw)?;
    for px in img.pixels_mut() {
        let a = px.0[3];
        if a == 0 || a == 255 { continue; }
        let af = a as f32 / 255.0;
        for c in 0..3 {
            px.0[c] = (px.0[c] as f32 / af).min(255.0) as u8;
        }
    }
    Some(img)
}

/// Rasterise a canvas into the given mutable pixmap.
///
/// `pixels_per_point` scales canvas-local logical coordinates into the
/// pixmap's physical pixel coordinates. The pixmap dimensions usually
/// match the monitor's physical resolution (xcap returns physical px),
/// while the canvas itself stores positions in egui logical pixels — at
/// 125/150/200 % DPI the two diverge, so without this scale strokes
/// land in the wrong region of the saved PNG. Pass `1.0` from any
/// caller that operates entirely in logical space.
///
/// `media_frames` is passed by screenshots only: it supplies the frame
/// each video / GIF is showing, and also makes stored pictures draw from
/// their full-size files rather than their previews.
pub fn render_canvas(
    pixmap: &mut PixmapMut<'_>,
    canvas: &Canvas,
    preview_shape: Option<&Shape>,
    preview_stroke: Option<&Stroke>,
    pixels_per_point: f32,
    media_frames: Option<&MediaFrames>,
) {
    // Clear to fully transparent — we composite onto the screen, and any
    // unpainted pixel must let the pixels behind show through.
    pixmap.fill(Color::TRANSPARENT);

    // canvas-local → logical screen px (pan + zoom) → physical pixel (× ppp).
    // `post_scale` applies *after* the prior transform, which is the
    // outer DPI step we want. tiny-skia's stroker tessellates strokes
    // into a filled path in source-space BEFORE the transform applies,
    // so stroke widths scale with this transform automatically — no
    // separate width-scale plumbing needed.
    let ppp = pixels_per_point.max(0.01);
    let t = Transform::from_translate(canvas.pan[0], canvas.pan[1])
        .post_scale(canvas.zoom, canvas.zoom)
        .post_scale(ppp, ppp);

    for layer in &canvas.layers {
        if !layer.visible { continue; }
        // Raster background: decode PNG to a Pixmap and blit it into
        // the same transform stack the strokes use, so a screenshot
        // preserves the frozen frame underneath the user's ink.
        // Decode is O(image) — only happens on save, not per frame.
        if let Some(img) = layer.image.as_ref() {
            if let Ok(decoded) = image::load_from_memory(&img.png) {
                let rgba = decoded.to_rgba8();
                let (iw, ih) = (rgba.width(), rgba.height());
                if let Some(mut src) = tiny_skia::Pixmap::new(iw, ih) {
                    // tiny-skia expects premultiplied RGBA.
                    for (px, dst) in rgba.pixels().zip(src.pixels_mut()) {
                        let [r, g, b, a] = px.0;
                        let af = a as f32 / 255.0;
                        *dst = tiny_skia::PremultipliedColorU8::from_rgba(
                            (r as f32 * af) as u8,
                            (g as f32 * af) as u8,
                            (b as f32 * af) as u8,
                            a,
                        ).unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0,0,0,0).unwrap());
                    }
                    // Source image is in physical pixels; the active
                    // transform `t` operates in canvas-local coords
                    // and post-scales by ppp into physical pixels.
                    // Logical-size width gives the canvas-local rect
                    // the renderer should hit, so divide image
                    // dimensions by `(iw / logical_w)` via a scale
                    // baked into the draw transform.
                    let lw = if img.logical_size[0] > 0.0 { img.logical_size[0] } else { iw as f32 };
                    let lh = if img.logical_size[1] > 0.0 { img.logical_size[1] } else { ih as f32 };
                    let sx = lw / iw as f32;
                    let sy = lh / ih as f32;
                    // `canvas_origin` is the image's canvas-local anchor —
                    // set at capture time and shifted further once the user
                    // pans this layer. Apply it after the physical→logical
                    // scale but before `t`, so it is interpreted in
                    // canvas-local pixels like every other coordinate the
                    // transform stack consumes, and screenshots line up the
                    // same way the live overlay does.
                    let draw_t = Transform::from_scale(sx, sy)
                        .post_translate(img.canvas_origin[0], img.canvas_origin[1])
                        .post_concat(t);
                    let opts = tiny_skia::PixmapPaint {
                        opacity: layer.opacity.clamp(0.0, 1.0),
                        blend_mode: tiny_skia::BlendMode::SourceOver,
                        quality: tiny_skia::FilterQuality::Bilinear,
                    };
                    pixmap.draw_pixmap(0, 0, src.as_ref(), &opts, draw_t, None);
                }
            }
        }
        for shape in &layer.shapes {
            draw_shape(pixmap, shape, t, layer.opacity, media_frames);
        }
        for stroke in &layer.strokes {
            draw_stroke(pixmap, stroke, t);
        }
    }
    if let Some(s) = preview_shape {
        draw_shape(pixmap, s, t, 1.0, None);
    }
    if let Some(s) = preview_stroke {
        draw_stroke(pixmap, s, t);
    }
}

/// Draw pins (see `canvas::pins`) over a picture `render_canvas` made,
/// where and as the overlay shows them: screenshots re-draw the slide
/// rather than capture the overlay, so its pins must be drawn too.
pub fn render_pins(
    pixmap: &mut PixmapMut<'_>,
    pins: &[crate::canvas::pins::Pinned],
    pixels_per_point: f32,
    media_frames: Option<&MediaFrames>,
) {
    let ppp = pixels_per_point.max(0.01);
    for p in pins {
        let m = p.pin.rigid(p.anchor());
        let s = p.pin.scale * ppp;
        let t = Transform::from_row(m.a, m.b, m.c, m.d, m.tx, m.ty).post_scale(s, s);
        for member in p.members {
            let Some(layer) = p.canvas.layers.get(member.layer) else { continue };
            match member.obj {
                // Whatever the layer's visibility and opacity, as on screen.
                ObjRef::Shape(i) => {
                    if let Some(shape) = layer.shapes.get(i) {
                        draw_shape(pixmap, shape, t, 1.0, media_frames);
                    }
                }
                ObjRef::Stroke(i) => {
                    if let Some(stroke) = layer.strokes.get(i) {
                        draw_stroke(pixmap, stroke, t);
                    }
                }
            }
        }
    }
}

pub(super) fn rgba_paint(color: [u8; 4]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(color[0], color[1], color[2], color[3]);
    // Anti-alias is on by default; spell it out for clarity.
    p.anti_alias = true;
    p
}

/// A rectangle with corners rounded by `r`, as a tiny-skia path.
pub(super) fn rounded_rect_path(rect: tiny_skia::Rect, r: f32) -> Option<tiny_skia::Path> {
    let (x0, y0, x1, y1) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let r = r.max(0.0).min(rect.width() * 0.5).min(rect.height() * 0.5);
    // Cubic quarter circles: control points k·r along each side.
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    pb.finish()
}

/// `t` with a turn of `rotation` radians about canvas point `(cx, cy)`
/// applied first. Returns `t` untouched for an unrotated shape, so those
/// rasterise exactly as they did before shapes could turn.
pub(super) fn turned(t: Transform, rotation: f32, cx: f32, cy: f32) -> Transform {
    if rotation == 0.0 {
        t
    } else {
        t.pre_concat(Transform::from_rotate_at(rotation.to_degrees(), cx, cy))
    }
}

/// Current frames of the videos and GIFs on screen, by `MediaRef::id`.
pub type MediaFrames = std::collections::HashMap<u64, std::sync::Arc<egui::ColorImage>>;

/// A PNG as a premultiplied tiny-skia pixmap.
pub(super) fn png_pixmap(png: &[u8]) -> Option<tiny_skia::Pixmap> {
    rgba_pixmap(&image::load_from_memory(png).ok()?.to_rgba8())
}

pub(super) fn rgba_pixmap(rgba: &image::RgbaImage) -> Option<tiny_skia::Pixmap> {
    let mut src = tiny_skia::Pixmap::new(rgba.width(), rgba.height())?;
    for (px, dst) in rgba.pixels().zip(src.pixels_mut()) {
        let [r, g, b, a] = px.0;
        let af = a as f32 / 255.0;
        *dst = tiny_skia::PremultipliedColorU8::from_rgba(
            (r as f32 * af) as u8,
            (g as f32 * af) as u8,
            (b as f32 * af) as u8,
            a,
        )
        .unwrap_or_else(|| {
            tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap()
        });
    }
    Some(src)
}

/// An egui image (already premultiplied) as a tiny-skia pixmap.
pub(super) fn color_image_pixmap(img: &egui::ColorImage) -> Option<tiny_skia::Pixmap> {
    let mut src = tiny_skia::Pixmap::new(img.size[0] as u32, img.size[1] as u32)?;
    for (px, dst) in img.pixels.iter().zip(src.pixels_mut()) {
        let [r, g, b, a] = px.to_array();
        // `from_rgba` rejects a colour channel above alpha.
        *dst = tiny_skia::PremultipliedColorU8::from_rgba(r.min(a), g.min(a), b.min(a), a)
            .unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap());
    }
    Some(src)
}

/// `image_opacity` is the opacity to draw picture rasters at — the
/// layer's, so a plate image that has been turned into an object still
/// fades with its layer in a screenshot the way the plate did. Nothing
/// else reads it: drawn shapes and fill stickers rasterise at full
/// strength here, as they always have.
/// `media_frames` supplies the live frame for videos and GIFs.
pub(super) fn draw_shape(
    pixmap: &mut PixmapMut<'_>,
    shape: &Shape,
    t: Transform,
    image_opacity: f32,
    media_frames: Option<&MediaFrames>,
) {
    match shape {
        Shape::Rect { a, b, color, stroke_width, border, rotation, filled, .. } => {
            let t = turned(t, *rotation, (a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5);
            let mut pb = PathBuilder::new();
            // Normalise corners so width/height are positive.
            let x0 = a[0].min(b[0]);
            let y0 = a[1].min(b[1]);
            let x1 = a[0].max(b[0]);
            let y1 = a[1].max(b[1]);
            pb.move_to(x0, y0);
            pb.line_to(x1, y0);
            pb.line_to(x1, y1);
            pb.line_to(x0, y1);
            pb.close();
            if let Some(path) = pb.finish() {
                let paint = rgba_paint(*color);
                if *filled {
                    pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None);
                }
                let mut sk = TsStroke { width: *stroke_width, ..TsStroke::default() };
                if let BorderStyle::DashedAnimated = border {
                    sk.line_cap = tiny_skia::LineCap::Round;
                    // Same on/off pattern + phase the egui side uses,
                    // so a screenshot taken at frame N matches the
                    // live overlay at frame N.
                    if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                        sk.dash = Some(d);
                    }
                }
                pixmap.stroke_path(&path, &paint, &sk, t, None);
            }
        }
        Shape::Ellipse { a, b, color, stroke_width, border, rotation, filled, .. } => {
            let cx = (a[0] + b[0]) * 0.5;
            let cy = (a[1] + b[1]) * 0.5;
            let rx = ((b[0] - a[0]) * 0.5).abs().max(1.0);
            let ry = ((b[1] - a[1]) * 0.5).abs().max(1.0);
            let t = turned(t, *rotation, cx, cy);
            // tiny_skia's PathBuilder has `push_oval` from a Rect.
            if let Some(rect) = tiny_skia::Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0) {
                let mut pb = PathBuilder::new();
                pb.push_oval(rect);
                if let Some(path) = pb.finish() {
                    let paint = rgba_paint(*color);
                    if *filled {
                        pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None);
                    }
                    let mut sk = TsStroke { width: *stroke_width, ..TsStroke::default() };
                    if let BorderStyle::DashedAnimated = border {
                        sk.line_cap = tiny_skia::LineCap::Round;
                        if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                            sk.dash = Some(d);
                        }
                    }
                    pixmap.stroke_path(&path, &paint, &sk, t, None);
                }
            }
        }
        Shape::Line { a, b, color, stroke_width, border, .. } => {
            match border {
                BorderStyle::Solid => {
                    draw_line(pixmap, *a, *b, *color, *stroke_width, t);
                }
                BorderStyle::DashedAnimated => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(a[0], a[1]);
                    pb.line_to(b[0], b[1]);
                    if let Some(path) = pb.finish() {
                        let paint = rgba_paint(*color);
                        let mut sk = TsStroke { width: *stroke_width, line_cap: tiny_skia::LineCap::Round, ..TsStroke::default() };
                        if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                            sk.dash = Some(d);
                        }
                        pixmap.stroke_path(&path, &paint, &sk, t, None);
                    }
                }
            }
        }
        Shape::Arrow { a, b, color, stroke_width, border, .. } => {
            // 1) The shaft (dashed or solid).
            match border {
                BorderStyle::Solid => {
                    draw_line(pixmap, *a, *b, *color, *stroke_width, t);
                }
                BorderStyle::DashedAnimated => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(a[0], a[1]);
                    pb.line_to(b[0], b[1]);
                    if let Some(path) = pb.finish() {
                        let paint = rgba_paint(*color);
                        let mut sk = TsStroke { width: *stroke_width, line_cap: tiny_skia::LineCap::Round, ..TsStroke::default() };
                        if let Some(d) = StrokeDash::new(vec![DASH_ON, DASH_OFF], dash_phase()) {
                            sk.dash = Some(d);
                        }
                        pixmap.stroke_path(&path, &paint, &sk, t, None);
                    }
                }
            }
            // 2) Two arrowhead lines, each rotated ±25° from the shaft direction.
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let ux = dx / len;
            let uy = dy / len;
            let head_len = (stroke_width * 4.0).max(8.0);
            // Rotate (ux,uy) by ±150° (i.e. 180° back, then ±30° splay).
            let theta: f32 = 25.0_f32.to_radians();
            let (s1, c1) = (theta.sin(), theta.cos());
            // head1: rotate by +(180-25)°
            let h1x = -(ux * c1 - uy * s1) * head_len;
            let h1y = -(ux * s1 + uy * c1) * head_len;
            // head2: rotate by -(180-25)°
            let h2x = -(ux * c1 + uy * s1) * head_len;
            let h2y = -(-ux * s1 + uy * c1) * head_len;
            draw_line(pixmap, *b, [b[0] + h1x, b[1] + h1y], *color, *stroke_width, t);
            draw_line(pixmap, *b, [b[0] + h2x, b[1] + h2y], *color, *stroke_width, t);
        }
        Shape::Raster { pos, size, png, rotation, kind, media, picture, flip, paper, .. } => {
            let t = turned(t, *rotation, pos[0] + size[0] * 0.5, pos[1] + size[1] * 0.5);
            let opacity = match kind {
                crate::canvas::shape::RasterKind::Image => image_opacity.clamp(0.0, 1.0),
                crate::canvas::shape::RasterKind::Fill => 1.0,
            };
            // A drawing pad's paper goes under its pixels.
            if let Some(p) = paper.filter(|p| p[3] > 0) {
                let a = (p[3] as f32 * opacity).round() as u8;
                if let Some(rect) = tiny_skia::Rect::from_xywh(pos[0], pos[1], size[0], size[1]) {
                    pixmap.fill_rect(rect, &rgba_paint([p[0], p[1], p[2], a]), t, None);
                }
            }
            // A video or GIF draws the frame it is showing when there is
            // one; otherwise (and for every still picture) the PNG is
            // decoded inline. Slow per call but the screenshot path only
            // fires once per save, not per frame. A screenshot (the only
            // caller passing `media_frames`) draws a stored picture from
            // its full-size file; thumbnails and flood fill make do with
            // its preview.
            let live = media.as_ref().and_then(|m| media_frames?.get(&m.id));
            let full = picture.as_ref().filter(|_| media_frames.is_some()).and_then(|p| {
                let path = crate::persistence::paths::media_dir().ok()?.join(&p.file);
                let img = image::open(path).ok()?.to_rgba8();
                rgba_pixmap(&p.view(&img, u32::MAX))
            });
            let src = match (live, full) {
                (Some(frame), _) => color_image_pixmap(frame),
                (None, Some(full)) => Some(full),
                (None, None) => png_pixmap(png),
            };
            if let Some(src) = src {
                let (iw, ih) = (src.width(), src.height());
                let sx = size[0] / iw as f32;
                let sy = size[1] / ih as f32;
                // Flipped: the image's left edge lands on the box's right.
                let draw_t = if *flip {
                    Transform::from_row(-sx, 0.0, 0.0, sy, pos[0] + size[0], pos[1])
                } else {
                    Transform::from_scale(sx, sy).post_translate(pos[0], pos[1])
                }
                .post_concat(t);
                let opts = tiny_skia::PixmapPaint {
                    opacity,
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    quality: tiny_skia::FilterQuality::Bilinear,
                };
                pixmap.draw_pixmap(0, 0, src.as_ref(), &opts, draw_t, None);
            }
        }
        Shape::Text { pos, content, font_size, color, rotation, background, .. } => {
            let t = turned(t, *rotation, pos[0], pos[1]);
            // The box, at the coarse size hit-testing uses: there is no
            // font here to measure the lines with.
            if let Some(bg) = background {
                let [w, h] = crate::canvas::shape::text_extent(content, *font_size);
                let pad = crate::canvas::shape::text_box_pad(*font_size);
                let r = (*font_size * 0.3).min(w * 0.5 + pad[0]).min(h * 0.5 + pad[1]);
                if let Some(rect) = tiny_skia::Rect::from_xywh(pos[0] - pad[0], pos[1] - pad[1], w + 2.0 * pad[0], h + 2.0 * pad[1]) {
                    let path = rounded_rect_path(rect, r);
                    if let Some(path) = path {
                        pixmap.fill_path(&path, &rgba_paint(*bg), FillRule::Winding, t, None);
                    }
                }
            }
            // tiny-skia has no built-in font rasteriser, so the
            // screenshot pipeline can't show glyphs yet. We paint a
            // coloured underline at the top-left of each line and a
            // semi-transparent fill so the user sees *something*
            // where their text sits — a placeholder until we bundle
            // a font + fontdue. Once that lands, replace this arm
            // with a real glyph blit.
            let lines: Vec<&str> = content.split('\n').collect();
            let line_h = font_size * 1.25;
            let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
            let w = (font_size * 0.55 * max_chars).max(*font_size);
            let h = line_h * lines.len() as f32;
            let mut fill = rgba_paint(*color);
            // Light underlay so the placeholder reads as "text was
            // here" without pretending to be the glyphs themselves.
            fill.set_color_rgba8(color[0], color[1], color[2], color[3] / 3);
            if let Some(rect) = tiny_skia::Rect::from_xywh(pos[0], pos[1], w, h) {
                let mut pb = PathBuilder::new();
                pb.push_rect(rect);
                if let Some(path) = pb.finish() {
                    pixmap.fill_path(&path, &fill, FillRule::Winding, t, None);
                }
            }
        }
    }
}

pub(super) fn draw_line(
    pixmap: &mut PixmapMut<'_>,
    a: [f32; 2],
    b: [f32; 2],
    color: [u8; 4],
    width: f32,
    t: Transform,
) {
    let mut pb = PathBuilder::new();
    pb.move_to(a[0], a[1]);
    pb.line_to(b[0], b[1]);
    if let Some(path) = pb.finish() {
        let paint = rgba_paint(color);
        let sk = TsStroke {
            width,
            line_cap: tiny_skia::LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..TsStroke::default()
        };
        pixmap.stroke_path(&path, &paint, &sk, t, None);
    }
}

pub(super) fn draw_stroke(pixmap: &mut PixmapMut<'_>, stroke: &Stroke, t: Transform) {
    if stroke.samples.is_empty() {
        return;
    }
    let paint = rgba_paint(stroke.color);
    // Thinnest mark, in screen pixels at the zoom the stroke was drawn at.
    let min_r = 0.4 * stroke.px;

    // Prefer the cached subdivided polyline (built once at commit time and
    // re-used by both the live egui paint and the screenshot path). Without
    // this, the screenshot would walk the *raw* filtered samples and draw
    // straight segments between them — visible kinks where the live egui
    // render shows a smooth Catmull-Rom-subdivided curve.
    if let Some(cache) = &stroke.cache {
        if cache.points.is_empty() { return; }
        if cache.points.len() == 1 {
            let r = (cache.widths[0] * 0.5).max(min_r);
            stamp_circle(pixmap, cache.points[0], r, &paint, t);
            return;
        }
        // Stamp filled discs along the dense cache polyline. Cache points
        // are spaced ~40 % of the stroke's half-width apart, which is
        // already dense enough that disc overlap leaves no scallops. Each
        // disc's radius comes from the matching per-point width in the
        // cache, so pressure-tapered strokes taper correctly.
        let pts = &cache.points;
        let ws  = &cache.widths;
        let n   = pts.len();
        let r0 = (ws[0] * 0.5).max(min_r);
        stamp_circle(pixmap, pts[0], r0, &paint, t);
        for i in 1..n {
            let prev = pts[i - 1];
            let cur  = pts[i];
            let dx = cur[0] - prev[0];
            let dy = cur[1] - prev[1];
            let dist = (dx * dx + dy * dy).sqrt();
            // Sub-step only when the cache spacing is larger than half a
            // pixel (rare — cache is already dense). Capped to bound work
            // on degenerate input.
            let steps = (dist * 2.0).ceil() as usize;
            let steps = steps.clamp(1, 16);
            let w0 = ws[i - 1];
            let w1 = ws[i];
            for s in 1..=steps {
                let u = s as f32 / steps as f32;
                let pos = [prev[0] + dx * u, prev[1] + dy * u];
                let w   = w0 + (w1 - w0) * u;
                stamp_circle(pixmap, pos, (w * 0.5).max(min_r), &paint, t);
            }
        }
        return;
    }

    // Fallback path used only when the cache is missing (in-progress live
    // stroke caught mid-frame, or some future caller that hasn't built
    // the cache yet). Walks raw filtered samples with linear interp
    // between them. The screenshot pipeline always sees committed strokes
    // (which carry a cache), so this is exercised rarely.
    let mut prev = stroke.samples[0];
    stamp_circle(pixmap, prev.pos, half_width(prev.pressure, stroke.base_width, min_r), &paint, t);
    for &cur in stroke.samples.iter().skip(1) {
        let dx = cur.pos[0] - prev.pos[0];
        let dy = cur.pos[1] - prev.pos[1];
        let dist = (dx * dx + dy * dy).sqrt();
        let steps = ((dist * 2.0).ceil() as usize).clamp(1, 256);
        for i in 1..=steps {
            let t_ = i as f32 / steps as f32;
            let pos  = [prev.pos[0] + dx * t_, prev.pos[1] + dy * t_];
            let pres = prev.pressure + (cur.pressure - prev.pressure) * t_;
            stamp_circle(pixmap, pos, half_width(pres, stroke.base_width, min_r), &paint, t);
        }
        prev = cur;
    }
}

#[inline]
pub(super) fn half_width(pressure: f32, base: f32, min_r: f32) -> f32 {
    // Clamp to [min_r, base/2]: even at "zero pressure" we still want a
    // hair-thin mark so the user sees their input. The floor avoids
    // invisible dots.
    let p = pressure.clamp(0.0, 1.0);
    (base * 0.5 * p).max(min_r)
}

pub(super) fn stamp_circle(
    pixmap: &mut PixmapMut<'_>,
    center: [f32; 2],
    radius: f32,
    paint: &Paint<'_>,
    t: Transform,
) {
    if let Some(rect) = tiny_skia::Rect::from_xywh(
        center[0] - radius,
        center[1] - radius,
        radius * 2.0,
        radius * 2.0,
    ) {
        let mut pb = PathBuilder::new();
        pb.push_oval(rect);
        if let Some(path) = pb.finish() {
            pixmap.fill_path(&path, paint, FillRule::Winding, t, None);
        }
    }
}
