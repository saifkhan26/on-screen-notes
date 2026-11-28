//! Painting a slide through egui, the overlay's every-frame path: the canvas,
//! pinned objects, and keeping ink that falls back to this path within budget
//! (culled to the view, simplified when zoomed out, capped).

use super::*;

// =============================================================================
// 1) GPU-accelerated path: egui Painter
// =============================================================================

/// Paint the entire canvas (shapes + strokes + previews) directly via the
/// egui `Painter`. Each line segment becomes two triangles on the GPU.
///
/// `origin` is where the canvas's `(0, 0)` lives on screen, in egui logical
/// coordinates. The caller passes in `canvas_rect.min`. We add the canvas's
/// own `pan` on top of that and scale by `zoom`.
#[allow(clippy::too_many_arguments)]
pub fn paint_canvas_egui(
    painter: &egui::Painter,
    origin: egui::Pos2,
    canvas: &Canvas,
    preview_shape: Option<&Shape>,
    preview_stroke: Option<&Stroke>,
    // Position-smoothing passes for the live preview path (committed
    // strokes already carry a baked cache built with this same value).
    position_passes: usize,
    // Live laser-pointer strokes painted on top of the canvas with a
    // time-decaying alpha. `(Stroke, pen_up_instant)` — renderer
    // computes `1 - elapsed / LASER_FADE_SECS` per entry and passes
    // that through `paint_stroke` as the layer opacity. The app loop
    // prunes faded entries each frame.
    laser_strokes: &[(Stroke, std::time::Instant)],
    // GPU texture handle for each layer's raster image (frozen
    // frame). Indexed by layer position; `None` = no image on that
    // layer. Built by the overlay's texture cache to avoid uploading
    // PNG bytes every frame.
    layer_image_textures: &[Option<egui::TextureId>],
    // Per-shape texture handles for `Shape::Raster` sticker fills.
    // Indexed by `[layer_index][shape_index]`. Same cache pattern
    // as `layer_image_textures` — `None` for non-raster shapes.
    shape_textures: &[Vec<Option<egui::TextureId>>],
    // Optional pan/zoom override. When `None` the canvas's own
    // `pan` and `zoom` drive the screen transform (normal overlay
    // render). When `Some` the renderer uses the supplied values
    // instead — letting callers re-render the canvas magnified
    // into a clipped region (e.g. the live loupe overlay).
    transform_override: Option<([f32; 2], f32)>,
    // In-progress Shift+Space layer gesture: `(layer index, scale,
    // canvas-local offset)`, i.e. the similarity `p → a · p + b` the
    // named layer previews under. A drag only fills `b`; the wheel adds
    // scale about the cursor. Nothing is written back to the canvas
    // until the gesture ends, at which point `Canvas::transform_layer`
    // bakes the same similarity into the layer's geometry. `None`
    // outside a gesture.
    layer_xform: Option<(usize, f32, [f32; 2])>,
    // In-progress Select-tool gesture: `(layer index, selected objects,
    // transform)`. The selected objects on that layer preview through
    // the transform; like `layer_xform`, nothing is written back until
    // the gesture ends and `Canvas::transform_objects` bakes it in.
    sel_preview: Option<(usize, &Selection, Affine)>,
    // A shape left out: `(layer, index)` of the label whose text is being
    // edited, which the caller paints with the new text instead.
    hidden: Option<(usize, usize)>,
) {
    // Pre-compute the pan-zoom transform as a closure so we don't
    // recompute the same maths inside every loop.
    let (view_pan, zoom) = match transform_override {
        Some((p, z)) => (p, z),
        None => (canvas.pan, canvas.zoom),
    };
    let pan_x = origin.x + view_pan[0];
    let pan_y = origin.y + view_pan[1];
    let to_screen = |p: [f32; 2]| egui::pos2(pan_x + p[0] * zoom, pan_y + p[1] * zoom);
    // Strokes wholly outside this rect are not built at all. The margin
    // covers the ribbon's feather and the minimum on-screen radius.
    let view = painter.clip_rect().expand(4.0);
    // Vertices committed ink may still add this frame; see
    // `MAX_INK_VERTICES`.
    let mut ink_budget = MAX_INK_VERTICES;
    let mut over_budget = 0usize;

    // Paint each layer in stack order: index 0 paints first (bottom),
    // last layer paints last (top). Within a layer, shapes first then
    // strokes, mirroring the historical flat-canvas ordering. Hidden
    // layers are skipped entirely. Per-layer opacity is threaded into
    // every paint call so the renderer multiplies each colour's alpha
    // by the layer's setting (Shift+wheel adjusts this live).
    for (li, layer) in canvas.layers.iter().enumerate() {
        if !layer.visible { continue; }
        let op = layer.opacity.clamp(0.0, 1.0);
        if op <= 0.0 { continue; }
        // Shadow the canvas-wide `to_screen` with one that folds in this
        // layer's in-progress gesture transform (identity for every
        // layer but the one being dragged / zoomed). Everything the
        // layer owns — raster plate, shapes, strokes — goes through it,
        // so the layer moves and scales as a rigid unit.
        let (lscale, off) = match layer_xform {
            Some((di, a, b)) if di == li => (a, b),
            _ => (1.0, [0.0, 0.0]),
        };
        let to_screen = |p: [f32; 2]| {
            egui::pos2(
                pan_x + (lscale * p[0] + off[0]) * zoom,
                pan_y + (lscale * p[1] + off[1]) * zoom,
            )
        };
        // Widths, font sizes and dash patterns are scaled by the `zoom`
        // the paint helpers receive rather than by `to_screen`, so the
        // previewed layer needs its gesture scale folded in here too —
        // otherwise a layer shrinking under the wheel would keep
        // full-thickness ink until the gesture committed.
        let view_zoom = zoom;
        let zoom = zoom * lscale;
        // Raster image (frozen frame) goes UNDER the layer's strokes
        // and shapes — it is the "background plate" the user is
        // annotating over.
        if let (Some(img), Some(Some(tex_id))) =
            (layer.image.as_ref(), layer_image_textures.get(li))
        {
            let (mut w, mut h) = (img.logical_size[0], img.logical_size[1]);
            if w <= 0.0 || h <= 0.0 {
                // Older save without logical_size: fall back to the
                // physical pixel dimensions. Mismatched DPI will skew
                // placement but the image still renders.
                w = img.size[0] as f32;
                h = img.size[1] as f32;
            }
            // Anchor at the recorded canvas origin (zero for older
            // saves) so a freeze captured under a panned/zoomed
            // canvas lands at the visible client area, not at the
            // canvas-local origin.
            let o = img.canvas_origin;
            let p0 = to_screen(o);
            let p1 = to_screen([o[0] + w, o[1] + h]);
            let rect = egui::Rect::from_two_pos(p0, p1);
            let tint = egui::Color32::from_rgba_unmultiplied(255, 255, 255, (op * 255.0) as u8);
            painter.image(*tex_id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
        }
        // Objects the Select tool is dragging on this layer, and the
        // transform to preview them under. They keep their place in the
        // paint order — only where they land changes.
        let dragged = sel_preview.filter(|(l, _, _)| *l == li);
        let shape_tex_for_layer = shape_textures.get(li);
        for (si, shape) in layer.shapes.iter().enumerate() {
            if hidden == Some((li, si)) {
                continue;
            }
            let tex = shape_tex_for_layer
                .and_then(|v| v.get(si))
                .copied()
                .flatten();
            match dragged {
                Some((_, sel, m)) if sel.shapes.binary_search(&si).is_ok() => {
                    // A shape's rotation and extents are stored fields,
                    // not points that could be mapped on the way to the
                    // screen, so preview a transformed copy. Cheap:
                    // raster shapes share their PNG.
                    let mut moved = shape.clone();
                    moved.apply_affine(&m);
                    paint_shape(painter, &to_screen, &moved, zoom, op, tex);
                }
                _ => paint_shape(painter, &to_screen, shape, zoom, op, tex),
            }
        }
        let is_dragged = |si: usize| dragged.is_some_and(|(_, sel, _)| sel.strokes.binary_search(&si).is_ok());
        // One stroke through egui, skipped when off screen or past the
        // vertex budget.
        let mut paint_egui = |si: usize, stroke: &Stroke| match dragged {
            Some((_, sel, m)) if sel.strokes.binary_search(&si).is_ok() => {
                // A stroke is all points: map each one on its way
                // to the screen instead of copying the stroke.
                let moved = |p: [f32; 2]| to_screen(m.apply(p));
                let z = zoom * m.linear_scale();
                let cost = stroke_vertex_cost(stroke, z);
                if cost > ink_budget { over_budget += 1; return; }
                ink_budget -= cost;
                paint_stroke(painter, &moved, stroke, z, position_passes, op);
            }
            _ => {
                if !stroke_on_screen(stroke, &to_screen, view) { return; }
                let cost = stroke_vertex_cost(stroke, zoom);
                if cost > ink_budget { over_budget += 1; return; }
                ink_budget -= cost;
                paint_stroke(painter, &to_screen, stroke, zoom, position_passes, op);
            }
        };
        if ink_gpu::enabled() {
            // Committed strokes come from the GPU ink cache in runs;
            // whatever it cannot draw goes through egui in between, so
            // the paint order stays the same.
            let xf = ink_gpu::Xform { pan: [pan_x, pan_y], zoom: view_zoom, scale: lscale, off };
            let mut run = ink_gpu::Run::new(xf);
            for plan in ink_gpu::plan_layer(painter.ctx(), &layer.strokes, op) {
                let range = plan.first..plan.first + plan.len;
                let visible = plan.bbox.is_some_and(|[x0, y0, x1, y1]| {
                    view.intersects(egui::Rect::from_two_pos(to_screen([x0, y0]), to_screen([x1, y1])))
                });
                let any_dragged = dragged.is_some_and(|(_, sel, _)| {
                    let i = sel.strokes.partition_point(|&s| s < range.start);
                    sel.strokes.get(i).is_some_and(|&s| s < range.end)
                });
                if plan.on_gpu && plan.all && !any_dragged {
                    if visible {
                        run.push(plan.key, plan.starts[0], plan.starts[plan.len]);
                    }
                    continue;
                }
                for (j, stroke) in layer.strokes[range].iter().enumerate() {
                    let si = plan.first + j;
                    if plan.on_gpu && ink_gpu::drawable(stroke) && !is_dragged(si) {
                        if visible {
                            run.push(plan.key, plan.starts[j], plan.starts[j + 1]);
                        }
                        continue;
                    }
                    run.flush(painter);
                    paint_egui(si, stroke);
                }
            }
            run.flush(painter);
        } else {
            for (si, stroke) in layer.strokes.iter().enumerate() {
                paint_egui(si, stroke);
            }
        }
    }
    if over_budget > 0 {
        log::debug!("ink over its vertex budget: {over_budget} stroke(s) not drawn this frame");
    }
    // Previews always paint on top of every layer regardless of which
    // is active — the user expects to see their in-progress mark even
    // if the layer they are drawing into sits below others. We use the
    // active layer's opacity so the preview matches what the committed
    // stroke will look like.
    let active_op = canvas
        .layers
        .get(canvas.active_layer)
        .map(|l| l.opacity.clamp(0.0, 1.0))
        .unwrap_or(1.0);
    if let Some(s) = preview_shape {
        paint_shape(painter, &to_screen, s, zoom, active_op, None);
    }
    if let Some(s) = preview_stroke {
        paint_stroke(painter, &to_screen, s, zoom, position_passes, active_op);
    }

    // Laser strokes — painted on top of every layer with a time-based
    // alpha so the trail fades out over `LASER_FADE_SECS`. We multiply
    // the alpha into `layer_opacity` (which `paint_stroke` already
    // honours), so no extra paint plumbing is required.
    if !laser_strokes.is_empty() {
        let now = std::time::Instant::now();
        for (stroke, up_at) in laser_strokes {
            let elapsed = now.duration_since(*up_at).as_secs_f32();
            let alpha = (1.0 - elapsed / LASER_FADE_SECS).clamp(0.0, 1.0);
            if alpha <= 0.0 { continue; }
            paint_stroke(painter, &to_screen, stroke, zoom, position_passes, alpha);
        }
    }
}

/// Paint a pin (see `canvas::pins`): objects `members` of `canvas`,
/// turned and moved by the rigid `m`, then scaled by `scale` about
/// `origin`. A layer's visibility and opacity are not the pin's: hiding
/// or fading a layer on its slide leaves its pinned objects showing as
/// they are. `textures[k]` is the picture of `members[k]` when that is
/// a raster.
#[allow(clippy::too_many_arguments)]
pub fn paint_pinned(
    painter: &egui::Painter,
    origin: egui::Pos2,
    canvas: &Canvas,
    members: &[PinMember],
    m: Affine,
    scale: f32,
    position_passes: usize,
    textures: &[Option<egui::TextureId>],
) {
    let to_screen = |p: [f32; 2]| egui::pos2(origin.x + p[0] * scale, origin.y + p[1] * scale);
    for (k, member) in members.iter().enumerate() {
        let Some(layer) = canvas.layers.get(member.layer) else { continue };
        match member.obj {
            ObjRef::Shape(i) => {
                let Some(shape) = layer.shapes.get(i) else { continue };
                // Rotation and extents are stored fields, so a turned
                // copy is painted (cheap: a raster shares its PNG).
                let mut placed = shape.clone();
                placed.apply_affine(&m);
                paint_shape(painter, &to_screen, &placed, scale, 1.0, textures.get(k).copied().flatten());
            }
            ObjRef::Stroke(i) => {
                let Some(stroke) = layer.strokes.get(i) else { continue };
                let mut sink = RigidSink { m, inner: PainterSink { painter, to_screen: &to_screen, zoom: scale } };
                emit_stroke(&mut sink, stroke, scale, position_passes, 1.0);
            }
        }
    }
}

/// Passes ink to `inner` with every point moved by the rigid `m` first.
/// A ribbon's edges are offset along normals worked out from its points,
/// so the points must be turned before the ribbon is built — mapping
/// them on the way to the screen would leave the edges facing the old
/// way.
pub(super) struct RigidSink<S> {
    m: Affine,
    inner: S,
}

impl<S: InkSink> InkSink for RigidSink<S> {
    fn disc(&mut self, c: [f32; 2], rc: f32, rule: DiscRule, color: egui::Color32) {
        self.inner.disc(self.m.apply(c), rc, rule, color);
    }

    fn ribbon(&mut self, points: &[[f32; 2]], widths: &[f32], style: StrokeStyle, color: egui::Color32) {
        let moved: Vec<[f32; 2]> = points.iter().map(|&p| self.m.apply(p)).collect();
        self.inner.ribbon(&moved, widths, style, color);
    }

    fn fill(&mut self, points: &[[f32; 2]], color: egui::Color32) {
        let moved: Vec<[f32; 2]> = points.iter().map(|&p| self.m.apply(p)).collect();
        self.inner.fill(&moved, color);
    }
}

/// Most vertices committed ink may add in one frame. egui puts a frame's
/// vertices and indices into one GPU buffer each, and wgpu treats a
/// buffer over its 256 MB limit as fatal, which aborts the app. Four
/// million vertices is about 80 MB, and the ribbon's 4.5 indices per
/// vertex about 72 MB, leaving room for everything else on screen.
/// Strokes past the budget are skipped for that frame.
pub(super) const MAX_INK_VERTICES: usize = 4_000_000;

/// Zoomed out, ribbon nodes closer than this on screen are merged.
pub(super) const LOD_MIN_GAP_PX: f32 = 0.5;

/// Whether any of `stroke` can land inside `view` (screen space).
/// `to_screen` is a pan plus a positive uniform scale, so the bounding
/// box's corners map to the screen box's corners.
pub(super) fn stroke_on_screen(
    stroke: &Stroke,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    view: egui::Rect,
) -> bool {
    let Some([x0, y0, x1, y1]) = stroke.bounds() else { return false };
    view.intersects(egui::Rect::from_two_pos(to_screen([x0, y0]), to_screen([x1, y1])))
}

/// Most vertices egui emits for one filled disc of radius `r` points:
/// small discs are often a 4-vertex quad from the font atlas, otherwise
/// a feathered polygon (`Path::add_circle`) with two vertices per point.
pub(super) fn disc_vertex_cost(r: f32) -> usize {
    if r <= 2.0 { 18 } else if r <= 5.0 { 34 } else if r < 18.0 { 66 } else if r < 50.0 { 130 } else { 258 }
}

/// Upper estimate of the vertices `paint_stroke` emits for `stroke` at
/// `zoom`, including the zoomed-out level of detail it applies.
pub(super) fn stroke_vertex_cost(stroke: &Stroke, zoom: f32) -> usize {
    let Some(cache) = &stroke.cache else {
        return stroke.samples.len().saturating_mul(32);
    };
    let n = cache.points.len();
    let max_w = cache.widths.iter().copied().fold(stroke.base_width, f32::max);
    let disc = disc_vertex_cost(max_w * 0.5 * zoom * 1.1);
    if n <= 1 {
        return disc;
    }
    let mut arc = 0.0_f32;
    let mut stamps = 1.0_f32;
    let k = match stroke.style {
        StrokeStyle::Pencil => 0.20,
        StrokeStyle::Airbrush => 0.10,
        _ => 0.0,
    };
    for i in 0..n - 1 {
        let (a, b) = (cache.points[i], cache.points[i + 1]);
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        arc += len;
        if k > 0.0 {
            stamps += len / (0.5 * (cache.widths[i] + cache.widths[i + 1]) * k).max(0.5 * stroke.px);
        }
    }
    let fill = if stroke.filled { n * 3 } else { 0 };
    match stroke.style {
        StrokeStyle::Pencil | StrokeStyle::Airbrush => {
            (stamps.ceil() as usize + 1).saturating_mul(disc) + fill
        }
        StrokeStyle::Default | StrokeStyle::Marker => {
            let nodes = if zoom < 1.0 {
                n.min((arc * zoom / LOD_MIN_GAP_PX).ceil() as usize + 2)
            } else {
                n
            };
            let fans = if FAN_CORNERS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
                nodes * disc
            } else {
                0
            };
            nodes * 4 + 2 * disc + fans + fill
        }
    }
}

/// Drop nodes closer than `min_gap` (canvas units) to the last node
/// kept. The first and last nodes always stay, so the stroke still
/// starts and ends where it did.
pub(super) fn decimate(points: &[[f32; 2]], widths: &[f32], min_gap: f32) -> (Vec<[f32; 2]>, Vec<f32>) {
    let n = points.len();
    if n <= 2 {
        return (points.to_vec(), widths.to_vec());
    }
    let gap2 = min_gap * min_gap;
    let mut pts = Vec::with_capacity(n);
    let mut ws = Vec::with_capacity(n);
    pts.push(points[0]);
    ws.push(widths[0]);
    for i in 1..n - 1 {
        let l = pts[pts.len() - 1];
        let p = points[i];
        if (p[0] - l[0]).powi(2) + (p[1] - l[1]).powi(2) >= gap2 {
            pts.push(p);
            ws.push(widths[i]);
        }
    }
    // Keep the end node; if the last interior node kept sits right on
    // top of it, the end replaces it.
    if pts.len() > 1 {
        let l = pts[pts.len() - 1];
        let e = points[n - 1];
        if (e[0] - l[0]).powi(2) + (e[1] - l[1]).powi(2) < gap2 {
            pts.pop();
            ws.pop();
        }
    }
    pts.push(points[n - 1]);
    ws.push(widths[n - 1]);
    (pts, ws)
}
