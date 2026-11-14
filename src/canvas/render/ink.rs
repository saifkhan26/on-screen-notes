//! Ink through egui: a stroke's ribbon, end caps and corner fans, fills, and the
//! pencil, marker and airbrush looks, built once by `emit_stroke` for any
//! `InkSink`.

use super::*;

/// Paint a single stroke through `painter` at `zoom`. What it draws is
/// `emit_stroke`; `PainterSink` turns that into egui shapes.
pub(super) fn paint_stroke(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    stroke: &Stroke,
    zoom: f32,
    position_passes: usize,
    layer_opacity: f32,
) {
    let mut sink = PainterSink { painter, to_screen, zoom };
    emit_stroke(&mut sink, stroke, zoom, position_passes, layer_opacity);
}

/// Width of the soft edge on each side of a ribbon, in screen points.
pub(crate) const RIBBON_FEATHER: f32 = 1.0;

/// What ink is drawn from: discs, variable-width ribbons and filled
/// polygons, all in canvas coordinates. `PainterSink` turns them into
/// egui shapes at the current zoom. The GPU ink cache
/// (`canvas::ink_gpu`) stores them once and applies the same zoom rules
/// in its shader, so both share this one description of every style.
pub(crate) trait InkSink {
    /// A filled disc of canvas radius `rc` at `c`; `rule` says how its
    /// on-screen radius follows from `rc` and the zoom.
    fn disc(&mut self, c: [f32; 2], rc: f32, rule: DiscRule, color: egui::Color32);
    /// A ribbon through `points`, `ribbon_half(widths, style, i)` canvas
    /// units either side of `points[i]` along `ribbon_normal(points, i)`,
    /// with a `RIBBON_FEATHER` soft edge. `points` has at least two
    /// entries.
    fn ribbon(&mut self, points: &[[f32; 2]], widths: &[f32], style: StrokeStyle, color: egui::Color32);
    /// The polygon through `points`, closed back to the start, filled.
    fn fill(&mut self, points: &[[f32; 2]], color: egui::Color32);
}

/// How a disc's on-screen radius follows from its canvas radius `rc` at
/// a zoom. It is data rather than code so the GPU ink shader can apply
/// the same rule; `ink_gpu`'s `disc_radius` mirrors `DiscRule::radius`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiscRule {
    /// Ribbon end caps and fan corners: shrunk by half the feather so
    /// the disc's own soft edge lands inside the ribbon, and never under
    /// 0.4 px.
    Cap = 0,
    /// Stamps and lone polyline points: left out under 0.4 px.
    Stamp = 1,
    /// A one-sample dot: always drawn.
    Plain = 2,
}

impl DiscRule {
    /// On-screen radius at `zoom`, or `None` when the disc is not drawn.
    pub(crate) fn radius(self, rc: f32, zoom: f32) -> Option<f32> {
        let r = rc * zoom;
        match self {
            DiscRule::Cap => Some((r - RIBBON_FEATHER * 0.5).max(0.4)),
            DiscRule::Stamp => (r >= 0.4).then_some(r),
            DiscRule::Plain => Some(r),
        }
    }
}

/// Unit normal of the ribbon at `points[i]`: the tangent (from the
/// neighbouring points) turned 90° counter-clockwise.
pub(crate) fn ribbon_normal(points: &[[f32; 2]], i: usize) -> [f32; 2] {
    let n = points.len();
    let dir = if i == 0 {
        [points[1][0] - points[0][0], points[1][1] - points[0][1]]
    } else if i == n - 1 {
        [points[n - 1][0] - points[n - 2][0], points[n - 1][1] - points[n - 2][1]]
    } else {
        [points[i + 1][0] - points[i - 1][0], points[i + 1][1] - points[i - 1][1]]
    };
    let len = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt().max(1.0e-6);
    [-dir[1] / len, dir[0] / len]
}

/// Canvas half-width of the ribbon at `points[i]`.
pub(crate) fn ribbon_half(widths: &[f32], style: StrokeStyle, i: usize) -> f32 {
    // Pencil style: jitter each polyline point's half-width with a
    // deterministic hash so the ribbon edge gains a graphite-like
    // graininess instead of reading as a perfect ribbon.
    let jitter = match style {
        StrokeStyle::Default | StrokeStyle::Marker => 1.0,
        StrokeStyle::Pencil  => 0.78 + 0.32 * hash01(i as u32),
        // Airbrush never reaches the ribbon path — stamp walk
        // bailed out above. Keep a sane default for completeness.
        StrokeStyle::Airbrush => 1.0,
    };
    widths[i] * 0.5 * jitter
}

/// Draws ink straight away through an egui `Painter`.
pub(super) struct PainterSink<'a, F> {
    pub(super) painter: &'a egui::Painter,
    pub(super) to_screen: &'a F,
    pub(super) zoom: f32,
}

impl<F: Fn([f32; 2]) -> egui::Pos2> InkSink for PainterSink<'_, F> {
    fn disc(&mut self, c: [f32; 2], rc: f32, rule: DiscRule, color: egui::Color32) {
        if let Some(r) = rule.radius(rc, self.zoom) {
            self.painter.circle_filled((self.to_screen)(c), r, color);
        }
    }

    // Emit one mesh with a built-in feather band along the left and
    // right edges. For each polyline point we generate 4 vertices,
    // outer→inner→inner→outer, where the *outer* vertices carry
    // alpha=0. Linear vertex-colour interpolation gives a soft 1 px
    // fringe along both edges → proper anti-aliasing for raw meshes
    // (egui only auto-feathers `Shape::line` / `circle_filled`, not
    // user-built meshes).
    fn ribbon(&mut self, points: &[[f32; 2]], widths: &[f32], style: StrokeStyle, color: egui::Color32) {
        let n = points.len();
        let trans = egui::Color32::from_rgba_premultiplied(0, 0, 0, 0);
        let mut mesh = egui::Mesh::default();
        mesh.vertices.reserve(n * 4);
        mesh.indices.reserve((n - 1) * 18);

        for i in 0..n {
            let [nx, ny] = ribbon_normal(points, i);
            let half = (ribbon_half(widths, style, i) * self.zoom).max(0.4);
            let p = (self.to_screen)(points[i]);
            // Four vertices per point: outer_left, inner_left,
            // inner_right, outer_right.
            let outer_l = egui::pos2(p.x + nx * (half + RIBBON_FEATHER), p.y + ny * (half + RIBBON_FEATHER));
            let inner_l = egui::pos2(p.x + nx * half,                    p.y + ny * half);
            let inner_r = egui::pos2(p.x - nx * half,                    p.y - ny * half);
            let outer_r = egui::pos2(p.x - nx * (half + RIBBON_FEATHER), p.y - ny * (half + RIBBON_FEATHER));
            mesh.colored_vertex(outer_l, trans);
            mesh.colored_vertex(inner_l, color);
            mesh.colored_vertex(inner_r, color);
            mesh.colored_vertex(outer_r, trans);
        }
        for i in 0..(n - 1) {
            let base = (i * 4)       as u32;
            let next = ((i + 1) * 4) as u32;
            // Left feather strip (outer_l ↔ inner_l).
            mesh.add_triangle(base, base + 1, next);
            mesh.add_triangle(base + 1, next + 1, next);
            // Interior (inner_l ↔ inner_r).
            mesh.add_triangle(base + 1, base + 2, next + 1);
            mesh.add_triangle(base + 2, next + 2, next + 1);
            // Right feather strip (inner_r ↔ outer_r).
            mesh.add_triangle(base + 2, base + 3, next + 2);
            mesh.add_triangle(base + 3, next + 3, next + 2);
        }
        self.painter.add(egui::Shape::mesh(mesh));
    }

    fn fill(&mut self, points: &[[f32; 2]], color: egui::Color32) {
        paint_fill_from_polyline(self.painter, self.to_screen, points, color);
    }
}

/// The colour a stroke's ribbon or stamps are drawn in.
pub(super) fn stroke_paint_color(stroke: &Stroke, layer_opacity: f32) -> egui::Color32 {
    // Pencil look: drop the colour alpha to ~55 % so the stroke reads as
    // translucent graphite. Width jitter (added inside the mesh build)
    // contributes the grainy edge.
    match stroke.style {
        StrokeStyle::Default => col(stroke.color, layer_opacity),
        StrokeStyle::Pencil  => pencil_color(stroke.color, layer_opacity),
        StrokeStyle::Marker  => marker_color(stroke.color, layer_opacity),
        StrokeStyle::Airbrush => airbrush_color(stroke.color, layer_opacity),
    }
}

/// Everything one stroke draws, in paint order. Two cases:
///
/// 1. **Committed strokes** carry a pre-built `cache` (set by
///    `Stroke::build_cache` in `Canvas::add_stroke`), so no Catmull-Rom
///    work happens here.
///
/// 2. **Live in-progress strokes** lack a cache (their geometry changes
///    every pen sample), so the spine is rebuilt each frame through the
///    same `build_spine` the cache uses: the live preview matches the
///    committed stroke exactly (WYSIWYG on pen-up). It is one stroke at
///    most, so this stays cheap.
///
/// `zoom` only picks the level of detail: below 1, a committed ribbon
/// drops nodes closer than `LOD_MIN_GAP_PX` on screen. The GPU ink
/// cache passes 1, keeping every node.
pub(crate) fn emit_stroke(
    sink: &mut impl InkSink,
    stroke: &Stroke,
    zoom: f32,
    position_passes: usize,
    layer_opacity: f32,
) {
    if stroke.samples.is_empty() {
        return;
    }
    let paint_color = stroke_paint_color(stroke, layer_opacity);

    if stroke.samples.len() == 1 {
        let s = stroke.samples[0];
        let pressure = if matches!(stroke.style, StrokeStyle::Marker) { 1.0 } else { s.pressure.max(0.1) };
        sink.disc(s.pos, (stroke.base_width * 0.5 * pressure).max(0.4 * stroke.px), DiscRule::Plain, paint_color);
        return;
    }

    let live;
    let cache = match &stroke.cache {
        Some(cache) => cache,
        None => {
            let position_passes = stroke.smoothing.cache_passes(position_passes);
            live = crate::canvas::stroke::build_spine(&stroke.samples, stroke.base_width, stroke.px, position_passes);
            &live
        }
    };
    // Fill goes UNDER the outline so the user-drawn line still reads as
    // the boundary on top of the colour blob. For the live Fill-tool
    // stroke it grows every frame as the user drags; lyon's sweep-line
    // tessellator handles self-intersection, so the preview never
    // flashes holes when the polyline briefly crosses itself.
    if stroke.filled {
        sink.fill(&cache.points, fill_color(stroke.color, layer_opacity));
    }
    // Marker = constant-width ribbon (no pressure modulation).
    // Airbrush = soft-disc stamp walk (handled inside the polyline
    // helper via the style discriminant).
    let const_widths: Vec<f32>;
    let widths_for_paint: &[f32] = if matches!(stroke.style, StrokeStyle::Marker) {
        const_widths = vec![stroke.base_width; cache.widths.len()];
        &const_widths
    } else {
        &cache.widths
    };
    // Zoomed out, many cached nodes land within half a pixel of each
    // other on screen; the ribbon styles drop those. At zoom 1 and
    // above nothing changes. Pencil and airbrush stamp by canvas arc
    // length, so their nodes are left alone.
    let lod: (Vec<[f32; 2]>, Vec<f32>);
    let (pts, ws): (&[[f32; 2]], &[f32]) = if zoom < 1.0
        && stroke.cache.is_some()
        && matches!(stroke.style, StrokeStyle::Default | StrokeStyle::Marker)
    {
        lod = decimate(&cache.points, widths_for_paint, LOD_MIN_GAP_PX / zoom);
        (&lod.0, &lod.1)
    } else {
        (&cache.points, widths_for_paint)
    };
    emit_polyline(sink, pts, ws, paint_color, stroke.style, stroke.px);
}

/// A variable-width centerline as a **single triangle-strip ribbon
/// mesh** plus round caps at each endpoint (or, for pencil and
/// airbrush, a walk of disc stamps).
///
/// Why a custom mesh instead of `egui::Shape::line` + disc stamps:
///
///   * `Shape::line` cannot vary width along its length.
///   * Disc stamping (our previous approach) paints N overlapping
///     anti-aliased circles. Egui feathers every disc edge (1.5 px),
///     and at high disc density the feathered fringes accumulate into
///     a visible scale-mail pattern *inside* the stroke — exactly the
///     "circle edges visible" artifact the user saw.
///
/// One mesh = one feathered outline. The interior is a flat triangle
/// fill with no internal seams, so no fringe stacking is possible.
pub(super) fn emit_polyline(
    sink: &mut impl InkSink,
    points: &[[f32; 2]],
    widths: &[f32],
    color: egui::Color32,
    style: StrokeStyle,
    px: f32,
) {
    let n = points.len();
    if n == 0 { return; }
    if n == 1 {
        sink.disc(points[0], widths[0] * 0.5, DiscRule::Stamp, color);
        return;
    }

    // Pencil = disc-stamp rendering.
    //
    // The default ribbon-mesh strip suffers from "bowtie" self-
    // overlap on tight curves: at a sharp inner turn, two adjacent
    // strip quads fold over each other on the inner side. With a
    // fully opaque colour this is invisible (same colour stacks =
    // same colour), but a translucent pencil colour stacks alpha →
    // visible dark scaly bands, exactly the artefact in the report.
    //
    // Stamping a circle at constant arc-length intervals dodges the
    // problem entirely — there is no per-segment geometry that can
    // fold, only a sequence of overlapping discs. Density variation
    // from the per-stamp width jitter is what gives the graphite
    // texture.
    if let StrokeStyle::Pencil = style {
        emit_pencil_stamps(sink, points, widths, color, px);
        return;
    }
    if let StrokeStyle::Airbrush = style {
        emit_airbrush_stamps(sink, points, widths, color, px);
        return;
    }

    // ---- Round caps painted BEFORE the ribbon mesh -------------
    // Painting discs first then the ribbon on top ensures the ribbon's
    // anti-aliased fringe overdraws the disc's auto-AA halo cleanly.
    // The disc's center half is invisible under the ribbon's solid
    // interior; only its backward hemisphere remains visible — which
    // is exactly the round-cap silhouette we want.
    //
    // `DiscRule::Cap` also shrinks each disc by half the feather so its
    // halo lands INSIDE the ribbon's solid interior instead of stacking
    // with the ribbon's outer feather strip. Without that shrink, the
    // user sees a darker bead at every endpoint because the two AA
    // gradients overlay in the same perpendicular pixel band.
    let (j0, jn) = match style {
        StrokeStyle::Default | StrokeStyle::Marker => (1.0_f32, 1.0_f32),
        StrokeStyle::Pencil  => (
            0.78 + 0.32 * hash01(0),
            0.78 + 0.32 * hash01((n - 1) as u32),
        ),
        StrokeStyle::Airbrush => (1.0_f32, 1.0_f32),
    };
    sink.disc(points[0], widths[0] * 0.5 * j0, DiscRule::Cap, color);
    sink.disc(points[n - 1], widths[n - 1] * 0.5 * jn, DiscRule::Cap, color);

    sink.ribbon(points, widths, style, color);

    // Fan-corner discs — Krita's `paintFan` analogue. At each
    // interior polyline vertex whose tangent rotates by more than
    // `fan_corners_step` radians, stamp a filled disc the size of
    // the local stroke width. The disc fills the outer wedge that
    // the miter joint would otherwise leave behind and covers the
    // inner-side bowtie self-overlap. Cheaper than synthesising
    // angular triangle fans and visually identical to Krita's
    // own implementation. Only fires when the caller opted in.
    //
    // Same `DiscRule::Cap` shrink as the endpoint caps: keeps the
    // disc's AA halo inside the ribbon body so the user doesn't see a
    // bead at every painted corner.
    if FAN_CORNERS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        let step = f32::from_bits(
            FAN_CORNERS_STEP_BITS.load(std::sync::atomic::Ordering::Relaxed),
        );
        for i in 1..(n - 1) {
            let pa = points[i - 1];
            let pb = points[i];
            let pc = points[i + 1];
            let d1 = [pb[0] - pa[0], pb[1] - pa[1]];
            let d2 = [pc[0] - pb[0], pc[1] - pb[1]];
            let l1 = (d1[0] * d1[0] + d1[1] * d1[1]).sqrt().max(1.0e-6);
            let l2 = (d2[0] * d2[0] + d2[1] * d2[1]).sqrt().max(1.0e-6);
            let cos_t = (d1[0] * d2[0] + d1[1] * d2[1]) / (l1 * l2);
            let theta = cos_t.clamp(-1.0, 1.0).acos();
            if theta > step {
                sink.disc(points[i], widths[i] * 0.5, DiscRule::Cap, color);
            }
        }
    }
}

/// The fan-corner setting as `(enabled, step in radians)`, for the GPU
/// ink cache to key its stored strokes on.
pub(crate) fn fan_corners() -> (bool, f32) {
    (
        FAN_CORNERS_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
        f32::from_bits(FAN_CORNERS_STEP_BITS.load(std::sync::atomic::Ordering::Relaxed)),
    )
}

/// Fill colour for the interior of a Fill-tool stroke. The auto-closing
/// edge is invisible regardless — only the user-drawn outline is
/// stroked — so we pass the stroke colour through with its own alpha
/// (typical RGBA = 0xFF → opaque fill), pre-scaled by the layer's
/// opacity.
pub(super) fn fill_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    col(c, layer_opacity)
}

/// Triangulate the closed polygon `[points[0], ..., points[n-1], points[0]]`
/// and paint it as a filled mesh.
///
/// Why lyon? egui's built-in fill is a centroid-fan that only handles
/// convex polygons. Ear-clipping (earcutr, our previous tessellator)
/// handles non-convex polygons but its behaviour is **undefined on
/// self-intersecting input** — when the user's Fill stroke crosses
/// itself (a loop-inside-a-loop, a figure-8, an accidental cross-over
/// while tracing) ear-clipping skips regions and leaves visible holes
/// inside the fill. Lyon's `FillTessellator` uses a sweep-line
/// algorithm with proper edge-crossing detection and honours the
/// **non-zero winding rule**, so self-intersection regions fill
/// correctly — no holes.
pub(super) fn paint_fill_from_polyline(
    painter: &egui::Painter,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
    points: &[[f32; 2]],
    color: egui::Color32,
) {
    use lyon_path::Path;
    use lyon_path::math::point;
    use lyon_tessellation::{
        BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex,
        VertexBuffers,
    };

    let n = points.len();
    if n < 3 { return; }

    // Build a closed lyon path in screen space so the resulting
    // vertices drop straight into an egui Mesh with no further
    // transform.
    let mut pb = Path::builder();
    let p0 = to_screen(points[0]);
    pb.begin(point(p0.x, p0.y));
    for p in points.iter().skip(1) {
        let s = to_screen(*p);
        pb.line_to(point(s.x, s.y));
    }
    pb.end(true); // close: lyon auto-joins last vertex → first
    let path = pb.build();

    // Output buffers: per-vertex screen position + triangle index list.
    let mut buffers: VertexBuffers<egui::Pos2, u32> = VertexBuffers::new();
    let mut tess = FillTessellator::new();
    // Non-zero winding is what most paint apps default to and matches
    // user intuition: an inner loop drawn in the same direction fills
    // as one connected blob; only opposite-winding inner loops cut
    // holes (rare in freehand pen input).
    let options = FillOptions::default()
        .with_fill_rule(FillRule::NonZero);
    let result = tess.tessellate_path(
        &path,
        &options,
        &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| {
            let p = v.position();
            egui::pos2(p.x, p.y)
        }),
    );
    if result.is_err() || buffers.indices.is_empty() {
        return;
    }

    let mut mesh = egui::Mesh::default();
    mesh.vertices.reserve(buffers.vertices.len());
    mesh.indices.reserve(buffers.indices.len());
    for p in buffers.vertices {
        mesh.colored_vertex(p, color);
    }
    for tri in buffers.indices.chunks_exact(3) {
        mesh.add_triangle(tri[0], tri[1], tri[2]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// RGBA bytes → translucent egui colour for pencil rendering.
///
/// Lower alpha than for the ribbon mesh because the stamp path covers
/// each pixel multiple times (dense disc stamps with ~30 % spacing
/// overlap each pixel ~2-3 times). A per-stamp alpha around 0.30
/// composites to ≈0.55 on the painted line — same on-screen weight as
/// before, but without the bowtie self-overlap that the mesh produced.
pub(super) fn pencil_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    // Per-stamp alpha is low because the stamp path covers each pixel
    // ~5× (dense spacing at ~20 % of stroke diameter). Composite
    // weight = 1 - (1-a)^5 ≈ 0.67 at a = 0.20 — pencil-pale, uniform.
    //
    // We *cannot* use `Color32::from_rgba_unmultiplied` here: that
    // helper premultiplies in linear (gamma-correct) space, which at
    // low alpha collapses src.rgb to a tiny value. Egui's shader then
    // blends in sRGB (gamma-encoded) space — the tiny src.rgb gets
    // overwhelmed by `dst.rgb * (1 - a)` and a mid-grey pencil over a
    // white desktop washes out to near-white.
    //
    // Premultiplying in display space (plain `src.rgb * a`) keeps the
    // colour contribution intact through the sRGB-space blend, so a
    // grey pencil reads as grey regardless of what is behind the
    // overlay.
    let af = (c[3] as f32 / 255.0) * 0.20 * layer_opacity.clamp(0.0, 1.0);
    let r = (c[0] as f32 * af) as u8;
    let g = (c[1] as f32 * af) as u8;
    let b = (c[2] as f32 * af) as u8;
    let a = (af * 255.0) as u8;
    egui::Color32::from_rgba_premultiplied(r, g, b, a)
}

/// Highlighter-style colour: roughly 45 % alpha so a single pass
/// reads as translucent ink and a second pass darkens visibly. We
/// stay in `from_rgba_unmultiplied` (not the gamma-trick path the
/// pencil uses) because at 45 % alpha there is no risk of the
/// premultiplied RGB collapsing to near-zero under sRGB blending.
pub(super) fn marker_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    let a = ((c[3] as f32 / 255.0) * 0.45 * layer_opacity.clamp(0.0, 1.0) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

/// Airbrush per-stamp colour: very low alpha (~7 %) so that
/// overlapping stamps composite into a soft cloud. We need the
/// premultiplied-in-display-space treatment for the same reason the
/// pencil does: at this alpha, the gamma-correct premultiply path
/// would wash the colour out completely.
pub(super) fn airbrush_color(c: [u8; 4], layer_opacity: f32) -> egui::Color32 {
    let af = (c[3] as f32 / 255.0) * 0.07 * layer_opacity.clamp(0.0, 1.0);
    let r = (c[0] as f32 * af) as u8;
    let g = (c[1] as f32 * af) as u8;
    let b = (c[2] as f32 * af) as u8;
    let a = (af * 255.0) as u8;
    egui::Color32::from_rgba_premultiplied(r, g, b, a)
}

/// Airbrush renderer: dense disc-stamp walk along the polyline with
/// uniform per-stamp radius (no jitter — the soft cloud effect
/// comes from low per-stamp alpha + tight spacing, not radius
/// variation). Spacing is ~10 % of the average stamp diameter so a
/// hold-still pen accumulates many overlapping stamps in the same
/// place and the user sees the colour build toward solid.
pub(super) fn emit_airbrush_stamps(
    sink: &mut impl InkSink,
    points: &[[f32; 2]],
    widths: &[f32],
    color: egui::Color32,
    px: f32,
) {
    let n = points.len();
    if n == 0 { return; }

    // First stamp anchors the head of the stroke at the pen-down
    // position so the user always sees ink land where they pressed.
    sink.disc(points[0], widths[0] * 0.5, DiscRule::Stamp, color);
    if n == 1 { return; }

    let mut residual = 0.0_f32;
    for i in 0..(n - 1) {
        let p0 = points[i];
        let p1 = points[i + 1];
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1.0e-4 { continue; }
        let avg_w = 0.5 * (widths[i] + widths[i + 1]);
        // 10 % of diameter → ~10 stamps stack on each pixel. With
        // 7 % per-stamp alpha that composites to ≈ 1 - 0.93^10 ≈
        // 52 % cover on a single pass; doubling back over the same
        // path takes it toward solid.
        let spacing = (avg_w * 0.10).max(0.5 * px);
        let mut d = spacing - residual;
        while d < seg_len {
            let t   = d / seg_len;
            let pos = [p0[0] + dx * t, p0[1] + dy * t];
            let w   = widths[i] + (widths[i + 1] - widths[i]) * t;
            sink.disc(pos, w * 0.5, DiscRule::Stamp, color);
            d += spacing;
        }
        residual = (seg_len - (d - spacing)).max(0.0);
    }
}

/// Pencil renderer: walk the polyline at constant arc-length intervals
/// and stamp a filled disc at each step. Disc radius is per-stamp
/// jittered for graphite-like edge irregularity.
pub(super) fn emit_pencil_stamps(
    sink: &mut impl InkSink,
    points: &[[f32; 2]],
    widths: &[f32],
    color: egui::Color32,
    px: f32,
) {
    let n = points.len();
    if n == 0 { return; }
    let mut stamp_id: u32 = 0;

    // First stamp at the start point so the head of the stroke is
    // anchored exactly where the pen landed.
    let mut last_pos = points[0];
    {
        let j = 0.78 + 0.32 * hash01(stamp_id);
        sink.disc(points[0], widths[0] * 0.5 * j, DiscRule::Stamp, color);
        stamp_id += 1;
    }

    // Carry-over of leftover arc length from the previous segment so
    // stamp spacing is continuous across joins (no doubled stamp at
    // shared vertices).
    //
    // **Slow-pen wobble guard.** Arc-length stepping alone is not
    // enough: at slow pen speeds the smoothed polyline meanders inside
    // a sub-pixel region, doubling back on itself in tiny zig-zags.
    // Arc-length stepping faithfully traces every micro-wobble and
    // ends up stamping discs on top of each other → a splotchy,
    // ink-bleed look instead of a continuous pencil line.
    //
    // The cure is a second gate: candidate stamps must also sit at
    // least 85 % of the nominal spacing away from the **previously
    // placed stamp** in Euclidean (straight-line) distance. A backtrack
    // along the polyline accumulates arc-length but not Euclidean
    // distance from the last stamp, so the rejection naturally fires
    // on wobble and never on a clean forward-moving line.
    let mut residual = 0.0_f32;
    for i in 0..(n - 1) {
        let p0 = points[i];
        let p1 = points[i + 1];
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 1.0e-4 { continue; }
        let w0 = widths[i];
        let w1 = widths[i + 1];
        let avg_w = 0.5 * (w0 + w1);
        // Spacing ≈ 20 % of the diameter (≈ 40 % of the radius) → each
        // pixel is hit by ~5 stamps, giving a continuous line with
        // visible per-stamp jitter at the edges. Floor at 0.5 px so a
        // hairline stroke still emits enough stamps to overlap.
        let spacing = (avg_w * 0.20).max(0.5 * px);
        // 85 % of nominal spacing is the rejection threshold. Lower
        // than the spacing itself so jitter between consecutive stamps
        // does not accidentally trip the guard on a perfectly straight
        // segment.
        let min_euc = spacing * 0.85;
        let mut d = spacing - residual;
        while d < seg_len {
            let t   = d / seg_len;
            let pos = [p0[0] + dx * t, p0[1] + dy * t];

            // Reject candidate if too close to the last stamp.
            let edx = pos[0] - last_pos[0];
            let edy = pos[1] - last_pos[1];
            let euc = (edx * edx + edy * edy).sqrt();
            if euc >= min_euc {
                let w   = w0 + (w1 - w0) * t;
                let j   = 0.78 + 0.32 * hash01(stamp_id);
                stamp_id += 1;
                sink.disc(pos, w * 0.5 * j, DiscRule::Stamp, color);
                last_pos = pos;
            }
            d += spacing;
        }
        residual = seg_len - (d - spacing);
    }
}

/// Deterministic [0, 1) hash from a u32 index. Used by the pencil
/// renderer to jitter the per-point width — same index always returns
/// the same value, so a stroke does not shimmer between frames.
#[inline]
pub(super) fn hash01(i: u32) -> f32 {
    let mut x = i.wrapping_mul(2654435761);
    x ^= x >> 16;
    x = x.wrapping_mul(2246822519);
    x ^= x >> 13;
    x = x.wrapping_mul(3266489917);
    x ^= x >> 16;
    (x & 0xFFFF) as f32 / 65535.0
}

// Centripetal Catmull-Rom shared with the committed-stroke path —
// see `crate::canvas::stroke::catmull_rom`.
