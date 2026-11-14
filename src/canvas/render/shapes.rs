//! Shapes and text through egui: rectangles, ellipses, lines, arrows, pictures,
//! labels and their boxes, and dashed borders.

use super::*;

/// Number of seconds a laser-pointer stroke takes to fade from full
/// opacity to zero after the user lifts the pen. Tuned for "trail
/// visible long enough to point at a thing across the room, gone
/// before the next sentence starts" — Sticky Notes / Krita /
/// PowerPoint use a similar 0.8–1.5 s window.
pub const LASER_FADE_SECS: f32 = 1.0;

/// Build an `egui::Color32` from raw RGBA bytes, scaling the alpha
/// channel by `layer_opacity` (clamped 0..=1). Every paint helper goes
/// through this function so layer-opacity application is centralised.
pub(super) fn col(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    let op = layer_opacity.clamp(0.0, 1.0);
    let a = ((c[3] as f32) * op).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

pub(super) fn paint_shape(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    shape: &Shape,
    zoom: f32,
    layer_opacity: f32,
    raster_tex: Option<egui::TextureId>,
) {
    match shape {
        // A turned rectangle is no longer an `egui::Rect`, so it is
        // drawn as an outline through its four turned corners instead.
        // Unrotated rectangles fall through to the arm below and paint
        // exactly as they always have.
        Shape::Rect { color, stroke_width, border, rotation, .. } if *rotation != 0.0 => {
            let o = shape.obb();
            let stroke = egui::Stroke::new(*stroke_width * zoom, col(*color, layer_opacity));
            match border {
                BorderStyle::Solid => {
                    // Same corner radius rule as the unrotated arm,
                    // worked out in canvas units and traced as arcs.
                    let min_side = o.half[0].min(o.half[1]) * 2.0;
                    let corner = (*stroke_width * 2.5).max(0.0).min((min_side * 0.45).max(0.0));
                    let pts: Vec<egui::Pos2> = rounded_box_outline(o.half, corner)
                        .into_iter()
                        .map(|q| to_screen(o.to_canvas(q)))
                        .collect();
                    painter.add(egui::Shape::closed_line(pts, stroke));
                }
                BorderStyle::DashedAnimated => {
                    let c = o.corners().map(to_screen);
                    paint_dashed_polyline(painter, &[c[0], c[1], c[2], c[3], c[0]], stroke, zoom);
                }
            }
        }
        Shape::Rect { a, b, color, stroke_width, border, .. } => {
            let r = egui::Rect::from_two_pos(to_screen(*a), to_screen(*b));
            // Corner radius scales gently with stroke width, capped at
            // ~half the shorter side so a tiny rectangle stays a
            // rectangle. `f32::clamp` would panic when the preview
            // rect is near-zero (drag just started, width=0): compute
            // the max first and use plain `min` so we degrade
            // gracefully to a sharp corner on tiny boxes.
            let min_side    = r.width().min(r.height()).abs();
            let want_corner = (*stroke_width * 2.5 * zoom).max(0.0);
            let max_corner  = (min_side * 0.45).max(0.0);
            let corner      = want_corner.min(max_corner);
            let stroke = egui::Stroke::new(*stroke_width * zoom, col(*color, layer_opacity));
            match border {
                BorderStyle::Solid => {
                    painter.rect_stroke(r, egui::Rounding::same(corner), stroke);
                }
                BorderStyle::DashedAnimated => {
                    // egui has no rounded-rect dashed stroke; emit four
                    // dashed sides instead. Corner rounding is dropped
                    // for the dashed variant — animated dashes look odd
                    // wrapping a curved corner, so sharp corners are
                    // cleaner and standard for marching-ants outlines.
                    paint_dashed_rect(painter, r, stroke, zoom);
                }
            }
        }
        Shape::Ellipse { a, b, color, stroke_width, border, rotation, .. } => {
            // egui has no native ellipse stroke; approximate with a
            // 64-segment polyline.
            let cx = (a[0] + b[0]) * 0.5;
            let cy = (a[1] + b[1]) * 0.5;
            let rx = ((b[0] - a[0]) * 0.5).abs();
            let ry = ((b[1] - a[1]) * 0.5).abs();
            // A turned ellipse is the same polyline with every point
            // swung about the centre.
            let (rs, rc) = rotation.sin_cos();
            const N: usize = 64;
            let pts: Vec<egui::Pos2> = (0..=N)
                .map(|i| {
                    let t = (i as f32 / N as f32) * std::f32::consts::TAU;
                    let (x, y) = (rx * t.cos(), ry * t.sin());
                    if *rotation == 0.0 {
                        to_screen([cx + x, cy + y])
                    } else {
                        to_screen([cx + rc * x - rs * y, cy + rs * x + rc * y])
                    }
                })
                .collect();
            let stroke = egui::Stroke::new(*stroke_width * zoom, col(*color, layer_opacity));
            match border {
                BorderStyle::Solid => {
                    painter.add(egui::Shape::line(pts, stroke));
                }
                BorderStyle::DashedAnimated => {
                    paint_dashed_polyline(painter, &pts, stroke, zoom);
                }
            }
        }
        Shape::Line { a, b, color, stroke_width, border, .. } => {
            let sa = to_screen(*a);
            let sb = to_screen(*b);
            let c = col(*color, layer_opacity);
            match border {
                BorderStyle::Solid => {
                    paint_round_line(painter, sa, sb, *stroke_width * zoom, c);
                }
                BorderStyle::DashedAnimated => {
                    let stroke = egui::Stroke::new(*stroke_width * zoom, c);
                    paint_dashed_polyline(painter, &[sa, sb], stroke, zoom);
                }
            }
        }
        Shape::Arrow { a, b, color, stroke_width, border, .. } => {
            let w = *stroke_width * zoom;
            let c = col(*color, layer_opacity);
            let sa = to_screen(*a);
            let sb = to_screen(*b);

            // Shaft: solid round-capped, or dashed-animated.
            match border {
                BorderStyle::Solid => {
                    paint_round_line(painter, sa, sb, w, c);
                }
                BorderStyle::DashedAnimated => {
                    let stroke = egui::Stroke::new(w, c);
                    paint_dashed_polyline(painter, &[sa, sb], stroke, zoom);
                }
            }

            // Two head lines fan out from `b`, ±25° from the shaft, length
            // proportional to stroke width so the arrowhead always looks
            // in scale with the line. Both head lines use round caps and
            // the joint at `b` is covered by the shaft's end disc.
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let ux = dx / len;
            let uy = dy / len;
            let head_len = (stroke_width * 4.5).max(10.0);
            let theta: f32 = 25.0_f32.to_radians();
            let (s1, c1) = (theta.sin(), theta.cos());
            let h1 = [b[0] - (ux * c1 - uy * s1) * head_len,
                     b[1] - (ux * s1 + uy * c1) * head_len];
            let h2 = [b[0] - (ux * c1 + uy * s1) * head_len,
                     b[1] - (-ux * s1 + uy * c1) * head_len];
            paint_round_line(painter, sb, to_screen(h1), w, c);
            paint_round_line(painter, sb, to_screen(h2), w, c);
        }
        // A picture still loading (or unreadable) shows as a faint box,
        // so the slide's layout is there at once; a fill shows nothing
        // until it is in.
        Shape::Raster { kind, .. } if raster_tex.is_none() => {
            if *kind == crate::canvas::shape::RasterKind::Image {
                let a = (layer_opacity * 255.0).clamp(0.0, 255.0) / 255.0;
                let fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, (18.0 * a) as u8);
                let edge = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, (48.0 * a) as u8));
                let corners: Vec<egui::Pos2> = shape.obb().corners().into_iter().map(to_screen).collect();
                painter.add(egui::Shape::convex_polygon(corners, fill, edge));
            }
        }
        // `Painter::image` only takes an axis-aligned rect, so a turned
        // image goes out as its own two-triangle textured quad. So does a
        // flipped one: its texture coordinates run right to left.
        Shape::Raster { rotation, flip, .. } if *rotation != 0.0 || *flip => {
            if let Some(tex_id) = raster_tex {
                let tint = egui::Color32::from_rgba_unmultiplied(
                    255, 255, 255,
                    (layer_opacity * 255.0).clamp(0.0, 255.0) as u8,
                );
                let uvs = if *flip {
                    [[1.0, 0.0], [0.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
                } else {
                    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
                };
                let mut mesh = egui::Mesh::with_texture(tex_id);
                for (p, uv) in shape.obb().corners().into_iter().zip(uvs) {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: to_screen(p),
                        uv: egui::pos2(uv[0], uv[1]),
                        color: tint,
                    });
                }
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(egui::Shape::mesh(mesh));
            }
        }
        Shape::Raster { pos, size, .. } => {
            if let Some(tex_id) = raster_tex {
                let p0 = to_screen(*pos);
                let p1 = to_screen([pos[0] + size[0], pos[1] + size[1]]);
                let rect = egui::Rect::from_two_pos(p0, p1);
                let tint = egui::Color32::from_rgba_unmultiplied(
                    255, 255, 255,
                    (layer_opacity * 255.0).clamp(0.0, 255.0) as u8,
                );
                painter.image(
                    tex_id,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    tint,
                );
            }
        }
        Shape::Text { pos, content, font_size, color, rotation, background, .. } => {
            let c = col(*color, layer_opacity);
            // The shape's `font_size` is in canvas-local logical px;
            // multiply by zoom so the on-screen height honours the
            // canvas's pan/zoom transform like every other shape.
            let size = (*font_size * zoom).max(2.0);
            // Prefer a "handwritten" family if the app registered one
            // at startup; egui silently falls back to Proportional
            // when the family is unknown, so this is safe regardless
            // of whether a font file is bundled.
            let family = egui::FontFamily::Name("handwritten".into());
            // 1.25 × font_size is the standard "comfortable reading"
            // line height; matches what most browsers default to.
            let line_h = size * 1.25;
            let top_left = to_screen(*pos);
            // Off screen: skip it. Laying it out would still rasterise its
            // glyphs at the zoomed size; zoomed far in, labels nowhere near
            // the view filled the font atlas and stalled every frame.
            // `reach` is generous: no glyph is wider than its size, and the
            // label may be turned any way about its top-left corner.
            let longest = content.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f32;
            let reach = longest * size + content.split('\n').count() as f32 * line_h;
            if !painter.clip_rect().expand(reach).contains(top_left) {
                return;
            }
            if let Some(bg) = background {
                let widest = content
                    .split('\n')
                    .map(|l| text_line_width(painter, l, size, &family))
                    .fold(0.0_f32, f32::max);
                let lines = content.split('\n').count();
                paint_text_box(painter, top_left, [widest, line_h * lines as f32], size, *rotation, col(*bg, layer_opacity));
            }
            // Each line is its own galley turned about its top-left,
            // stepped down the label's turned "down" direction so the
            // lines stay stacked.
            let (rs, rc) = rotation.sin_cos();
            for (i, line) in content.split('\n').enumerate() {
                let d = i as f32 * line_h;
                let lp = egui::pos2(top_left.x - rs * d, top_left.y + rc * d);
                paint_text_line(painter, lp, line, size, &family, c, *rotation);
            }
        }
    }
}

/// Largest text, in physical pixels, that glyphs are rasterised at.
/// egui draws each glyph into its font atlas at the size it is shown and
/// panics on one wider than the atlas (about 8k pixels), which zooming
/// far into a label would reach; bigger text is laid out at this size
/// and its mesh stretched, so it is a little soft but the right size.
/// Kept modest: zooming in rasterises a label once per step up to here,
/// and at 512 the atlas grew to 8192×4096 in one zoom, a stall of
/// almost half a second while it was re-uploaded.
pub(super) const MAX_GLYPH_PX: f32 = 256.0;

/// Glyphs bigger than this (physical px) are rasterised at the next of a
/// few fixed sizes up, and the mesh shrunk to fit. Zooming changes the
/// size every frame; rasterising at each exact size filled egui's font
/// atlas until it was cleared and re-uploaded whole, again and again.
pub(super) const EXACT_GLYPH_PX: f32 = 24.0;
/// Fixed sizes are this far apart (a factor of 2^¼, so a glyph is at most
/// ~16 % bigger than shown).
pub(super) const GLYPH_STEPS_PER_OCTAVE: f32 = 4.0;

/// The size (physical px) to rasterise glyphs at to show them `px` tall.
pub(super) fn glyph_px(px: f32) -> f32 {
    if px <= EXACT_GLYPH_PX {
        return px;
    }
    let steps = (px.log2() * GLYPH_STEPS_PER_OCTAVE).ceil();
    (steps / GLYPH_STEPS_PER_OCTAVE).exp2().min(MAX_GLYPH_PX)
}

/// How wide one line of a text label `size` logical px tall is drawn,
/// in logical px — the width `paint_text_line` gives it.
pub(super) fn text_line_width(painter: &egui::Painter, line: &str, size: f32, family: &egui::FontFamily) -> f32 {
    let ppp = painter.ctx().pixels_per_point();
    let px = size * ppp;
    let glyph = glyph_px(px);
    let drawn = if glyph == px { size } else { glyph / ppp };
    let galley = painter.layout_no_wrap(line.to_string(), egui::FontId::new(drawn, family.clone()), egui::Color32::WHITE);
    galley.size().x * size / drawn
}

/// The rounded box behind a text label: `text` is the size of its lines
/// (`[widest, all lines' height]`, screen points) from `top_left`, the
/// corner the label turns about by `angle`; `size` is its text height,
/// which sets the padding and the corner radius.
pub(crate) fn paint_text_box(
    painter: &egui::Painter,
    top_left: egui::Pos2,
    text: [f32; 2],
    size: f32,
    angle: f32,
    fill: egui::Color32,
) {
    let pad = crate::canvas::shape::text_box_pad(size);
    let half = [text[0] * 0.5 + pad[0], text[1] * 0.5 + pad[1]];
    let centre = [text[0] * 0.5, text[1] * 0.5];
    let (s, c) = angle.sin_cos();
    let pts: Vec<egui::Pos2> = rounded_box_outline(half, size * 0.3)
        .into_iter()
        .map(|q| {
            let (x, y) = (centre[0] + q[0], centre[1] + q[1]);
            egui::pos2(top_left.x + c * x - s * y, top_left.y + s * x + c * y)
        })
        .collect();
    painter.add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::NONE));
}

/// A rounded box round `centre` (screen points), `half` its half
/// size, turned by `angle`; `corner` its corners' radius.
pub(crate) fn box_ring(centre: egui::Pos2, half: [f32; 2], angle: f32, corner: f32) -> Vec<egui::Pos2> {
    let (s, c) = angle.sin_cos();
    rounded_box_outline(half, corner)
        .into_iter()
        .map(|[x, y]| egui::pos2(centre.x + c * x - s * y, centre.y + s * x + c * y))
        .collect()
}

/// The outline of the box a text label is drawn in, in screen points:
/// its lines `size` points tall from `top_left` (the corner it turns
/// about by `angle`), plus its background box's padding when `boxed`,
/// grown by `extra` all round. The shape `paint_text_box` fills, measured
/// the way `paint_text_line` lays the lines out.
pub(crate) fn text_box_ring(
    painter: &egui::Painter,
    content: &str,
    size: f32,
    boxed: bool,
    angle: f32,
    top_left: egui::Pos2,
    extra: f32,
) -> Vec<egui::Pos2> {
    let family = egui::FontFamily::Name("handwritten".into());
    let widest = content.split('\n').map(|l| text_line_width(painter, l, size, &family)).fold(0.0_f32, f32::max);
    let text = [widest, size * 1.25 * content.split('\n').count() as f32];
    let pad = if boxed { crate::canvas::shape::text_box_pad(size) } else { [0.0, 0.0] };
    let half = [text[0] * 0.5 + pad[0] + extra, text[1] * 0.5 + pad[1] + extra];
    let corner = if boxed { size * 0.3 } else { 0.0 } + extra;
    let centre = [text[0] * 0.5, text[1] * 0.5];
    let (s, c) = angle.sin_cos();
    rounded_box_outline(half, corner)
        .into_iter()
        .map(|q| {
            let (x, y) = (centre[0] + q[0], centre[1] + q[1]);
            egui::pos2(top_left.x + c * x - s * y, top_left.y + s * x + c * y)
        })
        .collect()
}

/// One line of a text label, `size` logical px tall, its top-left at
/// `lp`, turned by `angle` about that corner.
pub(super) fn paint_text_line(
    painter: &egui::Painter,
    lp: egui::Pos2,
    line: &str,
    size: f32,
    family: &egui::FontFamily,
    color: egui::Color32,
    angle: f32,
) {
    let ctx = painter.ctx();
    let ppp = ctx.pixels_per_point();
    let px = size * ppp;
    let glyph = glyph_px(px);
    let drawn = if glyph == px { size } else { glyph / ppp };
    let galley = painter.layout_no_wrap(line.to_string(), egui::FontId::new(drawn, family.clone()), color);
    let shape = egui::epaint::TextShape::new(lp, galley, color).with_angle(angle);
    if drawn == size {
        painter.add(shape);
        return;
    }
    let (tex_size, discs) = ctx.fonts(|f| (f.font_image_size(), f.texture_atlas().lock().prepared_discs()));
    let options = ctx.tessellation_options(|o| *o);
    let mut mesh = egui::Mesh::default();
    egui::epaint::Tessellator::new(ppp, options, tex_size, discs).tessellate_text(&shape, &mut mesh);
    let k = size / drawn;
    for v in &mut mesh.vertices {
        v.pos = lp + (v.pos - lp) * k;
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Outline of a rounded box centred on the origin, as points in the
/// box's own frame, clockwise from the top edge. `corner` is the corner
/// radius; zero gives the four plain corners.
pub(super) fn rounded_box_outline(half: [f32; 2], corner: f32) -> Vec<[f32; 2]> {
    let [hw, hh] = half;
    let r = corner.min(hw).min(hh).max(0.0);
    if r < 0.01 {
        return vec![[-hw, -hh], [hw, -hh], [hw, hh], [-hw, hh]];
    }
    // Arc centres with the angle each quarter-turn starts from:
    // top-right, bottom-right, bottom-left, top-left.
    let arcs = [
        ([hw - r, -hh + r], -std::f32::consts::FRAC_PI_2),
        ([hw - r, hh - r], 0.0),
        ([-hw + r, hh - r], std::f32::consts::FRAC_PI_2),
        ([-hw + r, -hh + r], std::f32::consts::PI),
    ];
    const STEPS: usize = 6;
    let mut pts = Vec::with_capacity(arcs.len() * (STEPS + 1));
    for (c, start) in arcs {
        for k in 0..=STEPS {
            let t = start + std::f32::consts::FRAC_PI_2 * (k as f32 / STEPS as f32);
            pts.push([c[0] + r * t.cos(), c[1] + r * t.sin()]);
        }
    }
    pts
}

/// Walk `pts` and emit dashed line segments using the global
/// `dash_phase()` so consecutive frames march in lock-step. `zoom`
/// scales the dash pattern so dashes stay readable at low canvas
/// zoom and don't turn into one long block at high zoom.
pub(crate) fn paint_dashed_polyline(
    painter: &egui::Painter,
    pts: &[egui::Pos2],
    stroke: egui::Stroke,
    zoom: f32,
) {
    if pts.len() < 2 { return; }
    // Scale the dash pattern by zoom so it reads at a consistent size
    // in screen space. Floor at 2 px so dashes are never sub-pixel.
    let on  = (DASH_ON  * zoom).max(2.0);
    let off = (DASH_OFF * zoom).max(1.0);
    let period = on + off;
    let phase = dash_phase() * zoom;

    // We walk the polyline in arc-length, alternating "draw" and "skip"
    // chunks. `cursor` tracks distance along the polyline modulo
    // `period`; if we're inside `[0, on)` we paint, otherwise we skip.
    // To make this concrete: shift cursor by `-phase` so animation
    // advances the pattern forward in time (positive phase = dashes
    // appear to move along the path).
    let mut cursor = -phase;
    // Normalise into [0, period) for cleaner arithmetic below.
    while cursor < 0.0 { cursor += period; }

    for win in pts.windows(2) {
        let p0 = win[0];
        let p1 = win[1];
        let dx = p1.x - p0.x;
        let dy = p1.y - p0.y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1e-4 { continue; }
        let mut walked = 0.0;
        while walked < seg_len {
            // Distance from cursor to next phase boundary.
            let in_on = cursor < on;
            let remaining_in_phase = if in_on { on - cursor } else { period - cursor };
            let chunk = remaining_in_phase.min(seg_len - walked);
            if in_on {
                let t0 = walked / seg_len;
                let t1 = (walked + chunk) / seg_len;
                let a = egui::pos2(p0.x + dx * t0, p0.y + dy * t0);
                let b = egui::pos2(p0.x + dx * t1, p0.y + dy * t1);
                paint_dash_pill(painter, a, b, stroke.width, stroke.color);
            }
            walked += chunk;
            cursor += chunk;
            if cursor >= period { cursor -= period; }
        }
    }
}

/// Specialise the polyline dash walker for an axis-aligned rectangle.
/// We walk the four sides in clockwise order so dashes flow around
/// the perimeter rather than restarting on each side.
pub(super) fn paint_dashed_rect(
    painter: &egui::Painter,
    r: egui::Rect,
    stroke: egui::Stroke,
    zoom: f32,
) {
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    paint_dashed_polyline(painter, &pts, stroke, zoom);
}

/// Paint a thick line with **round caps** by stroking the line and
/// stamping a filled disc of the same radius at both endpoints.
/// epaint 0.29 has no round-cap option on `Stroke`; this is the
/// cheapest portable round-cap.
pub(super) fn paint_round_line(
    painter: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    width: f32,
    color: egui::Color32,
) {
    painter.line_segment([a, b], egui::Stroke::new(width, color));
    let r = width * 0.5;
    if r >= 0.5 {
        painter.circle_filled(a, r, color);
        painter.circle_filled(b, r, color);
    }
}

/// Round-capped dash. Tighter caps than `paint_round_line` because
/// `circle_filled` adds ~1 px of AA halo that makes a half-stroke
/// disc visibly larger than the line's feathered ends — at short
/// dash lengths the caps end up reading as "dots with a thread
/// between" rather than a pill. Scaling the cap radius by 0.42
/// matches the line's apparent width while keeping the dash's
/// rounded silhouette.
pub(super) fn paint_dash_pill(
    painter: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    width: f32,
    color: egui::Color32,
) {
    painter.line_segment([a, b], egui::Stroke::new(width, color));
    let r = width * 0.42;
    if r >= 0.5 {
        painter.circle_filled(a, r, color);
        painter.circle_filled(b, r, color);
    }
}

#[cfg(test)]
mod glyph_tests {
    use super::*;

    #[test]
    fn glyph_sizes_snap_to_a_few_steps_above_small_text() {
        // Small text stays exact, so it stays crisp.
        assert_eq!(glyph_px(13.0), 13.0);
        // Above, every size within a step rasterises at the same size,
        // never smaller than shown, within a quarter octave.
        let a = glyph_px(100.0);
        assert_eq!(glyph_px(101.0), a);
        assert!(a >= 100.0 && a <= 100.0 * 2f32.powf(0.25) + 1e-3);
        // A zoom from 25 px to far past the cap touches only a few sizes.
        let mut seen: Vec<f32> = (25..=4000).map(|p| glyph_px(p as f32)).collect();
        seen.dedup();
        assert!(seen.len() <= 15, "{} sizes", seen.len());
        assert_eq!(glyph_px(5000.0), MAX_GLYPH_PX);
    }
}
