//! Point hit-testing against the ink itself.
//!
//! The eraser and the Select tool both need to know "is this point on
//! that object?". A bounding box is the wrong answer for anything that
//! is mostly empty space — a long diagonal stroke or a big outlined
//! rectangle covers a huge box and almost none of it. These tests
//! measure distance to the drawn line instead.

use crate::canvas::canvas::{point_in_polygon, Canvas};
use crate::canvas::shape::Shape;
use crate::canvas::stroke::{Stroke, StrokeStyle};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One object on a layer, by its index in `Layer::strokes` or
/// `Layer::shapes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjRef {
    Stroke(usize),
    Shape(usize),
}

/// A set of objects on one layer. Both index lists are kept sorted and
/// free of duplicates, so membership is a binary search and the lists
/// can be handed straight to the `Canvas` object-edit methods.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub strokes: Vec<usize>,
    pub shapes: Vec<usize>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.shapes.is_empty()
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.shapes.clear();
    }

    pub fn contains(&self, obj: ObjRef) -> bool {
        match obj {
            ObjRef::Stroke(i) => self.strokes.binary_search(&i).is_ok(),
            ObjRef::Shape(i) => self.shapes.binary_search(&i).is_ok(),
        }
    }

    pub fn insert(&mut self, obj: ObjRef) {
        let (list, i) = match obj {
            ObjRef::Stroke(i) => (&mut self.strokes, i),
            ObjRef::Shape(i) => (&mut self.shapes, i),
        };
        if let Err(at) = list.binary_search(&i) {
            list.insert(at, i);
        }
    }

    /// Add `obj` if absent, drop it if present.
    pub fn toggle(&mut self, obj: ObjRef) {
        let (list, i) = match obj {
            ObjRef::Stroke(i) => (&mut self.strokes, i),
            ObjRef::Shape(i) => (&mut self.shapes, i),
        };
        match list.binary_search(&i) {
            Ok(at) => { list.remove(at); }
            Err(at) => list.insert(at, i),
        }
    }

    /// Replace the whole selection with just `obj`.
    pub fn set_only(&mut self, obj: ObjRef) {
        self.clear();
        self.insert(obj);
    }
}

fn dist_sq_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len_sq = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len_sq > 1e-12 {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let d = [ap[0] - t * ab[0], ap[1] - t * ab[1]];
    d[0] * d[0] + d[1] * d[1]
}

/// True if `p` lies within `tol` of the stroke's ink (its polyline,
/// thickened by the local half-width), or inside it for a filled
/// stroke. `tol` is the eraser radius or the click slop.
pub fn stroke_hit(stroke: &Stroke, p: [f32; 2], tol: f32) -> bool {
    // Cheap reject: `bounds` is already padded by the stroke width.
    let Some(bb) = stroke.bounds() else { return false };
    if p[0] < bb[0] - tol || p[0] > bb[2] + tol || p[1] < bb[1] - tol || p[1] > bb[3] + tol {
        return false;
    }
    // Prefer the render cache: it is the line the user actually sees.
    // A marker paints at constant width, so its cached per-point widths
    // are not what is on screen.
    let cached = stroke.cache.as_ref().filter(|c| !c.points.is_empty());
    let marker = matches!(stroke.style, StrokeStyle::Marker);
    let n = cached.map_or(stroke.samples.len(), |c| c.points.len());
    let point = |i: usize| cached.map_or_else(|| stroke.samples[i].pos, |c| c.points[i]);
    let half_width = |i: usize| match cached {
        Some(c) if !marker => c.widths.get(i).copied().unwrap_or(stroke.base_width) * 0.5,
        _ => stroke.base_width * 0.5,
    };

    if n == 1 {
        let r = half_width(0) + tol;
        return dist_sq_to_segment(p, point(0), point(0)) <= r * r;
    }
    for i in 0..n.saturating_sub(1) {
        let r = half_width(i).max(half_width(i + 1)) + tol;
        if dist_sq_to_segment(p, point(i), point(i + 1)) <= r * r {
            return true;
        }
    }
    if stroke.filled {
        let poly: Vec<[f32; 2]> = (0..n).map(point).collect();
        return point_in_polygon(p, &poly);
    }
    false
}

/// True if `p` lies within `tol` of what the shape draws: the outline
/// for a rectangle or ellipse (they are not filled), the shaft for a
/// line or arrow, anywhere in the box for text and rasters.
pub fn shape_hit(shape: &Shape, p: [f32; 2], tol: f32) -> bool {
    match shape {
        Shape::Line { a, b, stroke_width, .. } | Shape::Arrow { a, b, stroke_width, .. } => {
            let r = stroke_width * 0.5 + tol;
            dist_sq_to_segment(p, *a, *b) <= r * r
        }
        Shape::Rect { stroke_width, .. } => {
            let o = shape.obb();
            let q = o.to_local(p);
            let dx = q[0].abs() - o.half[0];
            let dy = q[1].abs() - o.half[1];
            // Distance to the rectangle's edge, from either side.
            let d = if dx > 0.0 || dy > 0.0 {
                (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt()
            } else {
                -dx.max(dy)
            };
            d <= stroke_width * 0.5 + tol
        }
        Shape::Ellipse { stroke_width, .. } => {
            let o = shape.obb();
            let q = o.to_local(p);
            let rx = o.half[0].max(1e-3);
            let ry = o.half[1].max(1e-3);
            // `k` is 1 on the outline. Scaling `|k − 1|` by the radius in
            // the direction of `q` turns it into an approximate distance —
            // exact for a circle, close enough for click slop otherwise.
            let k = ((q[0] / rx).powi(2) + (q[1] / ry).powi(2)).sqrt();
            let radius = if k > 1e-6 {
                (q[0] * q[0] + q[1] * q[1]).sqrt() / k
            } else {
                rx.min(ry)
            };
            (k - 1.0).abs() * radius <= stroke_width * 0.5 + tol
        }
        Shape::Text { .. } | Shape::Raster { .. } => {
            let o = shape.obb();
            let q = o.to_local(p);
            q[0].abs() <= o.half[0] + tol && q[1].abs() <= o.half[1] + tol
        }
    }
}

// ---- Image transparency ------------------------------------------------
//
// A lasso capture is a freeform cut-out stored in its bounding box, so
// most of the box's corners are fully transparent. Picking by the box
// alone would let the invisible corner of one capture swallow clicks
// meant for the picture visibly underneath it — and slides built from
// many overlapping captures are exactly where that happens. So a click
// only lands on an image where the image has something drawn.

/// Coarse map of where an image is not transparent.
struct AlphaMask {
    w: usize,
    h: usize,
    solid: Vec<bool>,
}

/// Longest side of a mask, in cells. Plenty for click accuracy, and it
/// keeps a full-screen capture's mask to a few hundred kilobytes.
const MASK_MAX_SIDE: usize = 512;
/// Masks kept before the cache is emptied and rebuilt on demand.
const MASK_CACHE_LIMIT: usize = 256;

type MaskCache = Mutex<HashMap<(usize, u64), Option<Arc<AlphaMask>>>>;

/// Masks by `(length, fingerprint)` of the PNG they were built from, so
/// no id needs threading through the save format and copies of one
/// image share a mask. `None` records a PNG that would not decode.
fn mask_cache() -> &'static MaskCache {
    static CACHE: OnceLock<MaskCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Cheap identity for a PNG: FNV-1a over a few hundred bytes spread
/// evenly through it plus its tail. Keyed on content rather than on the
/// buffer's address, because an address is reused as soon as an image
/// is deleted and another is allocated in its place.
fn png_fingerprint(png: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let step = (png.len() / 256).max(1);
    png.iter().step_by(step).for_each(|b| eat(*b));
    png.iter().rev().take(64).for_each(|b| eat(*b));
    h
}

fn build_mask(png: &[u8]) -> Option<AlphaMask> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 { return None; }
    let scale = (MASK_MAX_SIDE as f32 / w.max(h) as f32).min(1.0);
    let mw = ((w as f32 * scale).ceil() as usize).max(1);
    let mh = ((h as f32 * scale).ceil() as usize).max(1);
    let mut solid = vec![false; mw * mh];
    // A cell is solid if any pixel that falls in it is, so thin
    // features survive the downsample.
    for (x, y, px) in img.enumerate_pixels() {
        if px.0[3] > 8 {
            let mx = (x as usize * mw / w).min(mw - 1);
            let my = (y as usize * mh / h).min(mh - 1);
            solid[my * mw + mx] = true;
        }
    }
    Some(AlphaMask { w: mw, h: mh, solid })
}

/// Whether the image has anything visible at `(u, v)` — its own
/// coordinates, 0..1 across and down — or within one mask cell of it.
///
/// Building a mask means decoding the PNG, tens of milliseconds for a
/// large capture. `may_decode` says whether that is acceptable right
/// now: yes for a click, no for the per-frame hover check. An image
/// with no mask yet (or one that will not decode) counts as solid
/// everywhere.
pub fn raster_solid_at(png: &[u8], u: f32, v: f32, may_decode: bool) -> bool {
    let key = (png.len(), png_fingerprint(png));
    let mask = {
        let mut cache = mask_cache().lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(&key) {
            Some(m) => m.clone(),
            None if may_decode => {
                if cache.len() >= MASK_CACHE_LIMIT { cache.clear(); }
                let m = build_mask(png).map(Arc::new);
                cache.insert(key, m.clone());
                m
            }
            None => return true,
        }
    };
    let Some(mask) = mask else { return true };
    let cx = ((u.clamp(0.0, 1.0) * mask.w as f32) as isize).min(mask.w as isize - 1);
    let cy = ((v.clamp(0.0, 1.0) * mask.h as f32) as isize).min(mask.h as isize - 1);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (x, y) = (cx + dx, cy + dy);
            if x >= 0 && y >= 0 && (x as usize) < mask.w && (y as usize) < mask.h
                && mask.solid[y as usize * mask.w + x as usize]
            {
                return true;
            }
        }
    }
    false
}

/// For a raster shape already known to contain `p` in its box: is the
/// image actually visible there? Always true for every other shape, and
/// for videos and GIFs, whose `png` is only the first frame — a GIF
/// that starts transparent would otherwise be unclickable there.
fn shape_solid_at(shape: &Shape, p: [f32; 2], may_decode: bool) -> bool {
    let Shape::Raster { png, media: None, .. } = shape else { return true };
    let o = shape.obb();
    let q = o.to_local(p);
    let (hw, hh) = (o.half[0].max(1e-3), o.half[1].max(1e-3));
    raster_solid_at(png, (q[0] + hw) / (2.0 * hw), (q[1] + hh) / (2.0 * hh), may_decode)
}

/// Something under the pointer, on any layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasHit {
    /// `(layer, object)`.
    Object(usize, ObjRef),
    /// The background plate image of this layer (a frozen frame, or a
    /// lasso capture made before captures became objects).
    Plate(usize),
}

/// Topmost visible thing under `p` across the whole canvas: layers from
/// the top of the stack down, and within a layer its strokes, then its
/// shapes, then its background plate — the reverse of paint order.
/// Hidden and fully transparent layers are skipped. Images only count
/// where they are not transparent (see `raster_solid_at`).
pub fn pick(canvas: &Canvas, p: [f32; 2], tol: f32, may_decode: bool) -> Option<CanvasHit> {
    for (li, layer) in canvas.layers.iter().enumerate().rev() {
        if !layer.visible || layer.opacity <= 0.0 { continue; }
        if let Some(i) = layer.strokes.iter().rposition(|s| stroke_hit(s, p, tol)) {
            return Some(CanvasHit::Object(li, ObjRef::Stroke(i)));
        }
        let shape = layer
            .shapes
            .iter()
            .rposition(|s| shape_hit(s, p, tol) && shape_solid_at(s, p, may_decode));
        if let Some(i) = shape {
            return Some(CanvasHit::Object(li, ObjRef::Shape(i)));
        }
        if let Some(img) = layer.image.as_ref() {
            let [w, h] = img.extent();
            let (u, v) = ((p[0] - img.canvas_origin[0]) / w.max(1e-3), (p[1] - img.canvas_origin[1]) / h.max(1e-3));
            if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)
                && raster_solid_at(&img.png, u, v, may_decode)
            {
                return Some(CanvasHit::Plate(li));
            }
        }
    }
    None
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::affine::Affine;
    use crate::canvas::canvas::Layer;
    use crate::canvas::shape::BorderStyle;
    use crate::canvas::stroke::PenSample;

    fn diagonal() -> Stroke {
        let mut s = Stroke::with_style([0, 0, 0, 255], 4.0, StrokeStyle::Default);
        for i in 0..=10 {
            let v = i as f32 * 10.0;
            s.samples.push(PenSample { pos: [v, v], pressure: 1.0, tilt: [0.0, 0.0] });
        }
        s.build_cache();
        s
    }

    #[test]
    fn stroke_is_hit_on_the_ink_but_not_in_its_empty_bounding_box() {
        let s = diagonal();
        assert!(stroke_hit(&s, [50.0, 50.0], 2.0));
        // Inside the bounding box, far from the line.
        assert!(!stroke_hit(&s, [90.0, 10.0], 2.0));
        assert!(!stroke_hit(&s, [10.0, 90.0], 2.0));
    }

    #[test]
    fn rect_is_hit_on_its_outline_only() {
        let r = Shape::Rect {
            a: [0.0, 0.0], b: [100.0, 60.0], color: [0; 4], stroke_width: 2.0,
            border: BorderStyle::Solid, rotation: 0.0,
        };
        assert!(shape_hit(&r, [50.0, 0.5], 2.0));
        assert!(shape_hit(&r, [100.0, 30.0], 2.0));
        assert!(!shape_hit(&r, [50.0, 30.0], 2.0));
        assert!(!shape_hit(&r, [120.0, 30.0], 2.0));
    }

    #[test]
    fn rotated_rect_is_hit_where_it_is_drawn() {
        let mut r = Shape::Rect {
            a: [0.0, 0.0], b: [100.0, 20.0], color: [0; 4], stroke_width: 2.0,
            border: BorderStyle::Solid, rotation: 0.0,
        };
        // Quarter turn about the centre (50, 10): the long side now runs
        // vertically through x = 40 and x = 60.
        r.apply_affine(&Affine::rotate_about([50.0, 10.0], std::f32::consts::FRAC_PI_2));
        assert!(shape_hit(&r, [60.0, 10.0], 2.0));
        assert!(shape_hit(&r, [50.0, 60.0], 2.0));
        // Where the unrotated outline used to be.
        assert!(!shape_hit(&r, [5.0, 0.0], 2.0));
    }

    #[test]
    fn ellipse_is_hit_on_its_outline_only() {
        let e = Shape::Ellipse {
            a: [0.0, 0.0], b: [100.0, 50.0], color: [0; 4], stroke_width: 2.0,
            border: BorderStyle::Solid, rotation: 0.0,
        };
        assert!(shape_hit(&e, [100.0, 25.0], 2.0));
        assert!(shape_hit(&e, [50.0, 0.0], 2.0));
        assert!(!shape_hit(&e, [50.0, 25.0], 2.0));
    }

    /// A `w × h` PNG that is opaque only where `solid(x, y)` says so.
    fn png(w: u32, h: u32, solid: impl Fn(u32, u32) -> bool) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba(if solid(x, y) { [200, 30, 30, 255] } else { [0, 0, 0, 0] })
        });
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    fn image(pos: [f32; 2], size: [f32; 2], png: Vec<u8>) -> Shape {
        Shape::Raster {
            pos, size, png: Arc::new(png), px_size: [1, 1], color: [0; 4],
            rotation: 0.0, kind: crate::canvas::shape::RasterKind::Image, media: None,
        }
    }

    #[test]
    fn within_a_layer_ink_is_above_shapes() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(Shape::Line {
            a: [0.0, 0.0], b: [100.0, 100.0], color: [0; 4], stroke_width: 4.0,
            border: BorderStyle::Solid,
        });
        assert_eq!(pick(&c, [50.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Shape(0))));
        c.layers[0].strokes.push(diagonal());
        assert_eq!(pick(&c, [50.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Stroke(0))));
        assert_eq!(pick(&c, [90.0, 10.0], 2.0, true), None);
    }

    #[test]
    fn picking_goes_through_every_visible_layer_top_first() {
        let mut c = Canvas::default();
        c.layers[0].strokes.push(diagonal());
        c.layers.push(Layer::default());
        c.layers.push(Layer::default());
        c.active_layer = 2;
        // Nothing on the upper layers: the stroke on layer 0 is found.
        assert_eq!(pick(&c, [50.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Stroke(0))));
        // Something above it wins.
        c.layers[1].strokes.push(diagonal());
        assert_eq!(pick(&c, [50.0, 50.0], 2.0, true), Some(CanvasHit::Object(1, ObjRef::Stroke(0))));
        // A hidden layer is not pickable.
        c.layers[1].visible = false;
        assert_eq!(pick(&c, [50.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Stroke(0))));
    }

    #[test]
    fn a_layers_background_plate_is_pickable_under_its_ink() {
        let mut c = Canvas::default();
        c.layers[0].image = Some(crate::canvas::canvas::LayerImage {
            png: png(4, 4, |_, _| true),
            size: [4, 4],
            logical_size: [200.0, 200.0],
            canvas_origin: [0.0, 0.0],
            capture_zoom: 1.0,
        });
        c.layers[0].strokes.push(diagonal());
        assert_eq!(pick(&c, [50.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Stroke(0))));
        assert_eq!(pick(&c, [150.0, 20.0], 2.0, true), Some(CanvasHit::Plate(0)));
        assert_eq!(pick(&c, [250.0, 20.0], 2.0, true), None);
    }

    #[test]
    fn a_click_passes_through_the_transparent_part_of_an_image() {
        let mut c = Canvas::default();
        // Below: fully opaque. Above: only its right half is drawn.
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 100.0], png(64, 64, |_, _| true)));
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 100.0], png(64, 64, |x, _| x >= 32)));
        assert_eq!(pick(&c, [80.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Shape(1))));
        assert_eq!(pick(&c, [20.0, 50.0], 2.0, true), Some(CanvasHit::Object(0, ObjRef::Shape(0))));
    }

    #[test]
    fn hover_never_decodes_and_treats_an_unseen_image_as_solid() {
        let mut c = Canvas::default();
        // Its own dimensions: masks are shared by content across the
        // process, and this test needs one nothing else has built.
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 100.0], png(80, 48, |x, _| x >= 40)));
        // Not decoded yet: the whole box counts.
        assert_eq!(pick(&c, [20.0, 50.0], 2.0, false), Some(CanvasHit::Object(0, ObjRef::Shape(0))));
        // A click decodes it; from then on hover knows too.
        assert_eq!(pick(&c, [20.0, 50.0], 2.0, true), None);
        assert_eq!(pick(&c, [20.0, 50.0], 2.0, false), None);
    }
}
