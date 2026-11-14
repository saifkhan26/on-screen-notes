//! Erasing: the eraser's tip, and the lasso eraser, which cuts strokes and
//! pictures along its path.

use super::*;

impl Canvas {
    /// Erase every stroke / shape on the **active** layer whose ink the
    /// eraser disc at `point` (canvas-local) actually touches. Returns
    /// true if anything was erased. Strokes / shapes on other layers
    /// are left alone, even when they sit visually under the eraser tip.
    ///
    /// "Touches the ink" is a distance test against the drawn line (see
    /// `canvas::hit`), not against the bounding box — a long diagonal
    /// stroke is no longer erased by a tip passing through the empty
    /// corner of its box. Two raster cases differ:
    /// * an `Image` (screen capture, pasted picture) is never erased —
    ///   here or by the lasso eraser. The user is rubbing out ink drawn
    ///   over it; the picture itself is removed with the Select tool.
    /// * a `Fill` sticker keeps the bounding-box test: telling which of
    ///   its pixels are transparent would mean decoding the PNG on every
    ///   pen sample.
    pub fn erase_at(&mut self, point: [f32; 2], radius: f32) -> bool {
        let li = self.active_layer;
        let mut erased_any = false;
        let layer = &mut self.layers[li];
        // Strokes: walk in reverse so indices stay valid as we remove.
        let mut i = layer.strokes.len();
        while i > 0 {
            i -= 1;
            if hit::stroke_hit(&layer.strokes[i], point, radius) {
                let s = layer.strokes.remove(i);
                self.undo_log.push(UndoEntry::StrokeErased(li, i, s));
                erased_any = true;
            }
        }
        let mut i = layer.shapes.len();
        while i > 0 {
            i -= 1;
            let touched = match &layer.shapes[i] {
                Shape::Raster { kind: RasterKind::Image, .. } => false,
                s @ Shape::Raster { .. } => rect_contains_circle(shape_bounds(s), point, radius),
                s => hit::shape_hit(s, point, radius),
            };
            if touched {
                let s = layer.shapes.remove(i);
                self.undo_log.push(UndoEntry::ShapeErased(li, i, s));
                erased_any = true;
            }
        }
        if erased_any {
            self.redo_log.clear();
        }
        erased_any
    }

    /// Lasso-erase. Walks every stroke / shape on the active layer
    /// and removes those whose centroid lies inside `polygon` (a
    /// closed loop of canvas-local points). Each removal records an
    /// undo entry so a stray loop is one Ctrl+Z away from recovery.
    /// `polygon` is assumed to have at least 3 vertices — callers
    /// should gate on that before invoking.
    pub fn erase_in_polygon(&mut self, polygon: &[[f32; 2]]) -> bool {
        if polygon.len() < 3 { return false; }
        let li = self.active_layer;
        let mut erased_any = false;
        let layer = &mut self.layers[li];
        // Walk strokes in reverse so we can splice fragments back at
        // each removed slot without index shifts breaking the rest
        // of the walk.
        let mut i = layer.strokes.len();
        while i > 0 {
            i -= 1;
            if layer.strokes[i].samples.is_empty() { continue; }
            // Quick reject: if no sample sits inside the polygon,
            // the stroke is untouched and we can skip the split.
            let any_inside = layer.strokes[i]
                .samples
                .iter()
                .any(|s| point_in_polygon(s.pos, polygon));
            if !any_inside { continue; }
            // Split: build runs of consecutive *outside* samples
            // into fragment strokes. Single-sample runs are dropped
            // — a one-point fragment would render as a stray dot.
            let pieces = split_stroke_outside_polygon(&layer.strokes[i], polygon);
            let original = layer.strokes.remove(i);
            for (k, mut piece) in pieces.iter().cloned().enumerate() {
                // Pre-build the cache so the next paint is cheap.
                piece.build_cache();
                layer.strokes.insert(i + k, piece);
            }
            self.undo_log
                .push(UndoEntry::StrokePartiallyErased(li, i, original, pieces));
            erased_any = true;
        }
        let mut i = layer.shapes.len();
        while i > 0 {
            i -= 1;
            // Pictures are what the user is annotating, not ink: neither
            // eraser touches them (see `erase_at`). Looping ink drawn
            // over a screenshot must not punch a hole in the screenshot.
            if let Shape::Raster { kind: RasterKind::Image, .. } = &layer.shapes[i] {
                continue;
            }
            // Fill stickers get a per-pixel partial erase so the
            // lasso clips out only the bit of the fill the user
            // looped, leaving the rest intact.
            if let Shape::Raster { .. } = &layer.shapes[i] {
                match raster_partial_erase(&layer.shapes[i], polygon) {
                    RasterEraseOutcome::Skip => {}
                    RasterEraseOutcome::Delete => {
                        let removed = layer.shapes.remove(i);
                        self.undo_log.push(UndoEntry::ShapeErased(li, i, removed));
                        erased_any = true;
                    }
                    RasterEraseOutcome::Replace(new_shape) => {
                        let old = std::mem::replace(&mut layer.shapes[i], new_shape.clone());
                        self.undo_log
                            .push(UndoEntry::ShapeReplaced(li, i, old, new_shape));
                        erased_any = true;
                    }
                }
                continue;
            }
            let center = layer.shapes[i].obb().center;
            if point_in_polygon(center, polygon) {
                let removed = layer.shapes.remove(i);
                self.undo_log.push(UndoEntry::ShapeErased(li, i, removed));
                erased_any = true;
            }
        }
        if erased_any {
            self.redo_log.clear();
        }
        erased_any
    }
}

/// True if axis-aligned rect `[x0,y0,x1,y1]` overlaps the disc at `c` with
/// `radius`. Cheap conservative test (we treat it as rect-vs-rect with the
/// disc's bounding box), which is fine for an eraser.
pub(super) fn rect_contains_circle(rect: [f32; 4], c: [f32; 2], radius: f32) -> bool {
    let r = radius;
    !(rect[2] < c[0] - r || rect[0] > c[0] + r || rect[3] < c[1] - r || rect[1] > c[1] + r)
}

/// Build fragment strokes from the runs of consecutive samples in
/// `stroke` that lie OUTSIDE `polygon`. Each fragment inherits the
/// source stroke's colour, width, style, and `filled` flag.
/// Single-sample runs are skipped so we do not leave stray dots
/// behind. May return zero fragments if every sample sits inside
/// the lasso.
pub(super) fn split_stroke_outside_polygon(stroke: &Stroke, polygon: &[[f32; 2]]) -> Vec<Stroke> {
    let mut out = Vec::new();
    let mut current: Vec<crate::canvas::stroke::PenSample> = Vec::new();
    let flush = |runs: &mut Vec<Stroke>, run: &mut Vec<crate::canvas::stroke::PenSample>, src: &Stroke| {
        if run.len() >= 2 {
            let mut s = Stroke::with_style(src.color, src.base_width, src.style);
            s.filled = src.filled;
            s.pin = src.pin;
            s.px = src.px;
            s.samples = std::mem::take(run);
            s.cache = None;
            runs.push(s);
        } else {
            run.clear();
        }
    };
    for sp in &stroke.samples {
        if point_in_polygon(sp.pos, polygon) {
            flush(&mut out, &mut current, stroke);
        } else {
            current.push(*sp);
        }
    }
    flush(&mut out, &mut current, stroke);
    out
}

/// Result of running the lasso polygon against a Raster shape.
pub(super) enum RasterEraseOutcome {
    /// Polygon and raster don't overlap — leave the shape alone.
    Skip,
    /// Every visible pixel of the raster sits inside the polygon —
    /// caller should drop the shape entirely.
    Delete,
    /// Partial overlap — `Shape::Raster` with cleared pixels;
    /// caller should swap it in.
    Replace(Shape),
}

/// Build a partial-erase variant of `shape` (must be `Shape::Raster`)
/// by zeroing the alpha of every pixel whose canvas-local centre
/// lies inside `polygon`. Returns one of `Skip` / `Delete` /
/// `Replace`. Decodes + re-encodes the PNG once.
pub(super) fn raster_partial_erase(shape: &Shape, polygon: &[[f32; 2]]) -> RasterEraseOutcome {
    let (pos, size, png, color, rotation, kind) = match shape {
        Shape::Raster { pos, size, png, color, rotation, kind, .. } => {
            (*pos, *size, png, *color, *rotation, *kind)
        }
        _ => return RasterEraseOutcome::Skip,
    };
    // The pixel grid below is axis-aligned in the image's own frame. For
    // a turned image, turn the lasso back by the same angle about the
    // image centre instead, so the rest of this function can keep
    // treating the raster as unrotated.
    let unturned: Vec<[f32; 2]>;
    let polygon: &[[f32; 2]] = if rotation != 0.0 {
        let o = shape.obb();
        unturned = polygon
            .iter()
            .map(|p| {
                let q = o.to_local(*p);
                [o.center[0] + q[0], o.center[1] + q[1]]
            })
            .collect();
        &unturned
    } else {
        polygon
    };
    let decoded = match image::load_from_memory(png) {
        Ok(d) => d.to_rgba8(),
        Err(_) => return RasterEraseOutcome::Skip,
    };
    let iw = decoded.width();
    let ih = decoded.height();
    if iw == 0 || ih == 0 || size[0] <= 0.0 || size[1] <= 0.0 {
        return RasterEraseOutcome::Skip;
    }

    // Polygon bounding box in canvas-local coords.
    let mut pxmin = f32::INFINITY;
    let mut pymin = f32::INFINITY;
    let mut pxmax = f32::NEG_INFINITY;
    let mut pymax = f32::NEG_INFINITY;
    for p in polygon {
        if p[0] < pxmin { pxmin = p[0]; }
        if p[1] < pymin { pymin = p[1]; }
        if p[0] > pxmax { pxmax = p[0]; }
        if p[1] > pymax { pymax = p[1]; }
    }

    // Intersect polygon bbox with raster bbox; bail early if empty.
    let cx0 = pxmin.max(pos[0]);
    let cy0 = pymin.max(pos[1]);
    let cx1 = pxmax.min(pos[0] + size[0]);
    let cy1 = pymax.min(pos[1] + size[1]);
    if cx0 >= cx1 || cy0 >= cy1 {
        return RasterEraseOutcome::Skip;
    }

    let sx = iw as f32 / size[0];
    let sy = ih as f32 / size[1];
    let x0 = (((cx0 - pos[0]) * sx).floor().max(0.0) as u32).min(iw);
    let y0 = (((cy0 - pos[1]) * sy).floor().max(0.0) as u32).min(ih);
    let x1 = (((cx1 - pos[0]) * sx).ceil()  as u32).min(iw);
    let y1 = (((cy1 - pos[1]) * sy).ceil()  as u32).min(ih);

    let mut output = decoded.clone();
    let mut any_modified = false;
    for py in y0..y1 {
        let cp_y = pos[1] + (py as f32 + 0.5) / sy;
        for px in x0..x1 {
            let cp_x = pos[0] + (px as f32 + 0.5) / sx;
            if point_in_polygon([cp_x, cp_y], polygon) {
                let pixel = output.get_pixel_mut(px, py);
                if pixel.0[3] != 0 {
                    pixel.0 = [0, 0, 0, 0];
                    any_modified = true;
                }
            }
        }
    }
    if !any_modified {
        return RasterEraseOutcome::Skip;
    }
    let any_remaining = output.pixels().any(|p| p.0[3] != 0);
    if !any_remaining {
        return RasterEraseOutcome::Delete;
    }
    let mut new_png: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut new_png);
        if output.write_to(&mut cursor, image::ImageFormat::Png).is_err() {
            return RasterEraseOutcome::Skip;
        }
    }
    RasterEraseOutcome::Replace(Shape::Raster {
        pos,
        size,
        png: std::sync::Arc::new(new_png),
        px_size: [iw, ih],
        color,
        rotation,
        kind,
        media: None,
        picture: None,
        // What is left of a pinned fill stays in its pin.
        pin: shape.pin(),
        flip: false,
        group: 0,
        tags: Vec::new(),
        turn: 0.0,
    })
}

/// Classic horizontal-ray crossing test. Returns true if `p` lies
/// inside `polygon` (treated as a closed loop — the last vertex
/// implicitly connects back to the first). Stable for any vertex
/// winding order. Robust for lasso-erase hit-testing where the user
/// path is rough; we don't try to handle exact-on-edge ties because
/// real centroids almost never land on a path edge.
pub(crate) fn point_in_polygon(p: [f32; 2], polygon: &[[f32; 2]]) -> bool {
    let n = polygon.len();
    if n < 3 { return false; }
    let (x, y) = (p[0], p[1]);
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (polygon[i][0], polygon[i][1]);
        let (xj, yj) = (polygon[j][0], polygon[j][1]);
        let crosses = (yi > y) != (yj > y)
            && x < (xj - xi) * (y - yi) / (yj - yi + f32::EPSILON) + xi;
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    inside
}
