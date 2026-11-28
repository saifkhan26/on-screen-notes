//! Cropping a picture: what part of it to keep, and the picture that
//! keeps it (`app::crop` is the mode that drags it).
//!
//! A crop is a rectangle in the picture as it shows (turned with it, the
//! way round it is shown), as shares 0..1 of its width and height. A press
//! on an edge or corner of it moves those edges; inside it, the whole
//! rectangle. Applying it:
//!   * a stored picture (`PictureRef`) keeps its file and only narrows its
//!     `crop`: nothing is lost, and `whole` brings the rest back;
//!   * a picture kept in the slide, and a drawing pad, has its pixels cut
//!     down (one Ctrl+Z gives them back).
//!
//! The box shrinks to the part kept, which stays where it was on screen.

use crate::canvas::shape::{Obb, Shape};
use std::sync::Arc;

/// The part of a picture to keep: shares of its shown width (`u`) and
/// height (`v`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

impl CropRect {
    pub const WHOLE: Self = Self { u0: 0.0, v0: 0.0, u1: 1.0, v1: 1.0 };

    /// Keeps all of the picture.
    pub fn is_whole(&self) -> bool {
        self.u0 <= 1e-4 && self.v0 <= 1e-4 && self.u1 >= 1.0 - 1e-4 && self.v1 >= 1.0 - 1e-4
    }

    /// `[x0, y0, x1, y1]` in the picture's own frame (canvas units from its
    /// centre, along its sides) for a picture `half` big.
    pub fn local(&self, half: [f32; 2]) -> [f32; 4] {
        let (w, h) = (2.0 * half[0], 2.0 * half[1]);
        [-half[0] + self.u0 * w, -half[1] + self.v0 * h, -half[0] + self.u1 * w, -half[1] + self.v1 * h]
    }
}

/// What a press took hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// These edges move.
    Edges { left: bool, top: bool, right: bool, bottom: bool },
    /// The whole rectangle moves.
    Move,
}

/// What a press at canvas point `p` takes hold of, on a picture in box
/// `obb` cropped to `rect`. `reach` is how far from an edge still counts
/// as on it, in canvas units. `None` off the rectangle.
pub fn grab_at(obb: Obb, rect: CropRect, p: [f32; 2], reach: f32) -> Option<Grab> {
    let q = obb.to_local(p);
    let [x0, y0, x1, y1] = rect.local(obb.half);
    if q[0] < x0 - reach || q[0] > x1 + reach || q[1] < y0 - reach || q[1] > y1 + reach {
        return None;
    }
    // On a small rectangle both edges may be in reach: the nearer wins.
    let (dl, dr, dt, db) = ((q[0] - x0).abs(), (q[0] - x1).abs(), (q[1] - y0).abs(), (q[1] - y1).abs());
    let left = dl <= reach && dl <= dr;
    let right = dr <= reach && dr < dl;
    let top = dt <= reach && dt <= db;
    let bottom = db <= reach && db < dt;
    if left || right || top || bottom {
        Some(Grab::Edges { left, top, right, bottom })
    } else {
        Some(Grab::Move)
    }
}

/// `start` with what `grab` holds moved by `delta` (canvas units in the
/// picture's own frame), for a picture `half` big: kept inside the
/// picture and at least `min` (shares) wide and high.
pub fn drag(grab: Grab, start: CropRect, delta: [f32; 2], half: [f32; 2], min: [f32; 2]) -> CropRect {
    let du = delta[0] / (2.0 * half[0]).max(1e-6);
    let dv = delta[1] / (2.0 * half[1]).max(1e-6);
    // `clamp` that never panics when the room is smaller than `min`.
    let within = |x: f32, lo: f32, hi: f32| x.min(hi).max(lo.min(hi));
    let mut r = start;
    match grab {
        Grab::Move => {
            let (w, h) = (start.u1 - start.u0, start.v1 - start.v0);
            r.u0 = within(start.u0 + du, 0.0, 1.0 - w);
            r.v0 = within(start.v0 + dv, 0.0, 1.0 - h);
            r.u1 = r.u0 + w;
            r.v1 = r.v0 + h;
        }
        Grab::Edges { left, top, right, bottom } => {
            if left {
                r.u0 = within(start.u0 + du, 0.0, start.u1 - min[0]);
            }
            if right {
                r.u1 = within(start.u1 + du, start.u0 + min[0], 1.0).max(start.u0 + min[0]).min(1.0);
            }
            if top {
                r.v0 = within(start.v0 + dv, 0.0, start.v1 - min[1]);
            }
            if bottom {
                r.v1 = within(start.v1 + dv, start.v0 + min[1], 1.0).max(start.v0 + min[1]).min(1.0);
            }
        }
    }
    r
}

/// The pixel size of what picture `shape` shows: a stored one's view, or
/// the pixels kept in the slide.
pub fn shown_px(shape: &Shape) -> Option<[u32; 2]> {
    match shape {
        Shape::Raster { picture: Some(p), .. } => Some(p.size),
        Shape::Raster { px_size, .. } => Some(*px_size),
        _ => None,
    }
}

/// The least share of each side a crop may keep: two pixels.
pub fn min_share(px: [u32; 2]) -> [f32; 2] {
    [(2.0 / px[0].max(1) as f32).min(1.0), (2.0 / px[1].max(1) as f32).min(1.0)]
}

/// `[from, to)` pixels of a side `n` long for the shares `a..b`, at least
/// one pixel.
fn span(a: f32, b: f32, n: u32) -> (u32, u32) {
    let from = ((a * n as f32).round() as u32).min(n.saturating_sub(1));
    let to = ((b * n as f32).round() as u32).clamp(from + 1, n.max(from + 1));
    (from, to)
}

/// PNG bytes cut to the shares `[u0, v0, u1, v1]` of their own pixels.
fn cut_png(png: &[u8], s: [f32; 4]) -> Option<(Vec<u8>, [u32; 2])> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (x0, x1) = span(s[0], s[2], img.width());
    let (y0, y1) = span(s[1], s[3], img.height());
    let part = image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image();
    Some((crate::canvas::pad::encode_png(&part)?, [part.width(), part.height()]))
}

/// Give picture `out` the box `rect` of `obb` (its box before), so the
/// part kept stays where it showed.
fn place(out: &mut Shape, obb: Obb, local: [f32; 4]) {
    let c = obb.to_canvas([(local[0] + local[2]) * 0.5, (local[1] + local[3]) * 0.5]);
    let size = [local[2] - local[0], local[3] - local[1]];
    if let Shape::Raster { pos, size: s, .. } = out {
        *pos = [c[0] - size[0] * 0.5, c[1] - size[1] * 0.5];
        *s = size;
    }
}

/// Picture `shape` cropped to `rect`. `None` for anything but a still
/// picture, or one whose pixels cannot be read.
pub fn apply(shape: &Shape, rect: CropRect) -> Option<Shape> {
    let Shape::Raster { media: None, flip, .. } = shape else { return None };
    let obb = shape.obb();
    let mut out = shape.clone();
    // The shares the way the pixels are stored: the shape's mirror undone.
    let (su0, su1) = if *flip { (1.0 - rect.u1, 1.0 - rect.u0) } else { (rect.u0, rect.u1) };
    let stored = [su0, rect.v0, su1, rect.v1];
    let Shape::Raster { png, px_size, picture, .. } = &mut out else { return None };
    match picture {
        Some(p) => {
            // Narrow the file's crop; the file stays as it is.
            let [cx, cy, cw, ch] = p.crop.unwrap_or([0, 0, p.size[0], p.size[1]]);
            let (fu0, fu1) = if p.mirror { (1.0 - su1, 1.0 - su0) } else { (su0, su1) };
            let (x0, x1) = span(fu0, fu1, cw);
            let (y0, y1) = span(rect.v0, rect.v1, ch);
            p.crop = Some([cx + x0, cy + y0, x1 - x0, y1 - y0]);
            p.size = [x1 - x0, y1 - y0];
            // The preview shows the old view: cut it the same way.
            if let Some((bytes, size)) = cut_png(png, stored) {
                *png = Arc::new(bytes);
                *px_size = size;
            }
        }
        None => {
            let (bytes, size) = cut_png(png, stored)?;
            *png = Arc::new(bytes);
            *px_size = size;
        }
    }
    place(&mut out, obb, rect.local(obb.half));
    Some(out)
}

/// A stored picture that shows only part of its file, showing all of it
/// again (`file` pixels big): the box grows round the part it showed,
/// which stays where it was. The caller gives it a new preview. `None`
/// for a picture that is not stored, or shows all of its file already.
pub fn whole(shape: &Shape, file: [u32; 2]) -> Option<Shape> {
    let Shape::Raster { picture: Some(p), size, flip, .. } = shape else { return None };
    let [cx, cy, cw, ch] = p.crop?;
    let (sx, sy) = (size[0] / cw.max(1) as f32, size[1] / ch.max(1) as f32);
    let (fw, fh) = (file[0] as f32, file[1] as f32);
    let (before, after) = (cx as f32 * sx, (fw - (cx + cw) as f32).max(0.0) * sx);
    // Shown the other way round, what was left of the crop is right of it.
    let (left, right) = if p.mirror != *flip { (after, before) } else { (before, after) };
    let (top, bottom) = (cy as f32 * sy, (fh - (cy + ch) as f32).max(0.0) * sy);
    let obb = shape.obb();
    let [hw, hh] = obb.half;
    let mut out = shape.clone();
    if let Shape::Raster { picture: Some(p), .. } = &mut out {
        p.crop = None;
        p.size = file;
    }
    place(&mut out, obb, [-hw - left, -hh - top, hw + right, hh + bottom]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::{PictureRef, RasterKind};

    fn png_of(img: &image::RgbaImage) -> Vec<u8> {
        crate::canvas::pad::encode_png(img).unwrap()
    }

    /// Each pixel names where it is: red = x, green = y.
    fn grid(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 7, 255]))
    }

    fn picture(pos: [f32; 2], size: [f32; 2], img: &image::RgbaImage, flip: bool, rotation: f32) -> Shape {
        Shape::Raster {
            pos,
            size,
            png: Arc::new(png_of(img)),
            px_size: [img.width(), img.height()],
            color: [0; 4],
            rotation,
            flip,
            kind: RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: None,
            grain: None,
        }
    }

    fn pixels(s: &Shape) -> image::RgbaImage {
        let Shape::Raster { png, .. } = s else { panic!() };
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn presses_take_hold_of_corners_edges_or_the_whole() {
        let s = picture([0.0, 0.0], [100.0, 50.0], &grid(4, 2), false, 0.0);
        let o = s.obb();
        let r = CropRect { u0: 0.2, v0: 0.2, u1: 0.8, v1: 0.8 };
        let edges = |l, t, rt, b| Some(Grab::Edges { left: l, top: t, right: rt, bottom: b });
        // Left edge at x 20, top at y 10, right at 80, bottom at 40.
        assert_eq!(grab_at(o, r, [20.0, 10.0], 3.0), edges(true, true, false, false));
        assert_eq!(grab_at(o, r, [80.0, 25.0], 3.0), edges(false, false, true, false));
        assert_eq!(grab_at(o, r, [50.0, 41.0], 3.0), edges(false, false, false, true));
        assert_eq!(grab_at(o, r, [50.0, 25.0], 3.0), Some(Grab::Move));
        assert_eq!(grab_at(o, r, [5.0, 25.0], 3.0), None);
        assert_eq!(grab_at(o, r, [50.0, 49.0], 3.0), None);
    }

    #[test]
    fn dragging_stays_inside_and_never_turns_inside_out() {
        let half = [50.0, 25.0];
        let min = [0.1, 0.1];
        let r = CropRect { u0: 0.2, v0: 0.2, u1: 0.8, v1: 0.8 };
        let left = Grab::Edges { left: true, top: false, right: false, bottom: false };
        // 10 units of a 100-wide picture is a tenth.
        assert!(near(drag(left, r, [10.0, 0.0], half, min).u0, 0.3));
        assert!(near(drag(left, r, [-500.0, 0.0], half, min).u0, 0.0));
        // Past the other edge it stops `min` short of it.
        assert!(near(drag(left, r, [500.0, 0.0], half, min).u0, 0.7));
        let corner = Grab::Edges { left: false, top: false, right: true, bottom: true };
        let d = drag(corner, r, [500.0, -500.0], half, min);
        assert!(near(d.u1, 1.0) && near(d.v1, 0.3) && near(d.u0, 0.2));
        // A move keeps the size and stops at the picture's edge.
        let m = drag(Grab::Move, r, [-500.0, 5.0], half, min);
        assert!(near(m.u0, 0.0) && near(m.u1, 0.6) && near(m.v0, 0.3) && near(m.v1, 0.9));
        assert!(CropRect::WHOLE.is_whole() && !r.is_whole());
    }

    #[test]
    fn a_picture_in_the_slide_is_cut_and_the_part_kept_stays_put() {
        // 8×4 pixels shown 80×40: one pixel is ten units.
        let s = picture([100.0, 200.0], [80.0, 40.0], &grid(8, 4), false, 0.0);
        let r = CropRect { u0: 0.25, v0: 0.5, u1: 0.75, v1: 1.0 };
        let out = apply(&s, r).unwrap();
        let Shape::Raster { pos, size, px_size, .. } = &out else { panic!() };
        assert_eq!(*px_size, [4, 2]);
        assert!(near(pos[0], 120.0) && near(pos[1], 220.0) && near(size[0], 40.0) && near(size[1], 20.0));
        let img = pixels(&out);
        assert_eq!(img.get_pixel(0, 0).0, [2, 2, 7, 255]);
        assert_eq!(img.get_pixel(3, 1).0, [5, 3, 7, 255]);
    }

    #[test]
    fn a_mirrored_picture_is_cut_where_it_shows() {
        // Shown mirrored: its left on screen is the pixels' right.
        let s = picture([0.0, 0.0], [80.0, 40.0], &grid(8, 4), true, 0.0);
        let out = apply(&s, CropRect { u0: 0.0, v0: 0.0, u1: 0.25, v1: 1.0 }).unwrap();
        let img = pixels(&out);
        assert_eq!((img.width(), img.get_pixel(0, 0).0[0], img.get_pixel(1, 0).0[0]), (2, 6, 7));
        let Shape::Raster { pos, size, .. } = &out else { panic!() };
        assert!(near(pos[0], 0.0) && near(size[0], 20.0));
    }

    #[test]
    fn a_turned_picture_keeps_the_part_where_it_showed() {
        // Turned a quarter: its own right is the screen's down.
        let s = picture([0.0, 0.0], [80.0, 40.0], &grid(8, 4), false, std::f32::consts::FRAC_PI_2);
        let kept = CropRect { u0: 0.5, v0: 0.0, u1: 1.0, v1: 1.0 };
        let before = s.obb().to_canvas([20.0, 0.0]);
        let out = apply(&s, kept).unwrap();
        let after = out.obb().center;
        assert!(near(before[0], after[0]) && near(before[1], after[1]), "{before:?} {after:?}");
        assert!(near(out.obb().half[0], 20.0) && near(out.obb().half[1], 20.0));
    }

    fn stored(crop: Option<[u32; 4]>, size: [u32; 2], mirror: bool, flip: bool) -> Shape {
        let mut s = picture([0.0, 0.0], [size[0] as f32, size[1] as f32], &grid(size[0], size[1]), flip, 0.0);
        if let Shape::Raster { picture, .. } = &mut s {
            *picture = Some(Box::new(PictureRef { file: "f.png".into(), size, crop, mirror }));
        }
        s
    }

    fn picture_ref(s: &Shape) -> PictureRef {
        let Shape::Raster { picture: Some(p), .. } = s else { panic!() };
        (**p).clone()
    }

    #[test]
    fn a_stored_picture_only_narrows_its_crop() {
        // A 100×50 file shown whole: keep its right half's top half.
        let out = apply(&stored(None, [100, 50], false, false), CropRect { u0: 0.5, v0: 0.0, u1: 1.0, v1: 0.5 }).unwrap();
        let p = picture_ref(&out);
        assert_eq!((p.crop, p.size), (Some([50, 0, 50, 25]), [50, 25]));
        // Already cropped to [20, 10, 40, 20]: shares are of what shows.
        let out = apply(&stored(Some([20, 10, 40, 20]), [40, 20], false, false), CropRect { u0: 0.5, v0: 0.5, u1: 1.0, v1: 1.0 }).unwrap();
        assert_eq!(picture_ref(&out).crop, Some([40, 20, 20, 10]));
        // Mirrored by the file's crop, and again by the shape's flip: the
        // two undo each other, so the screen's left is the file's left.
        let both = apply(&stored(None, [100, 50], true, true), CropRect { u0: 0.0, v0: 0.0, u1: 0.3, v1: 1.0 }).unwrap();
        assert_eq!(picture_ref(&both).crop, Some([0, 0, 30, 50]));
        let one = apply(&stored(None, [100, 50], true, false), CropRect { u0: 0.0, v0: 0.0, u1: 0.3, v1: 1.0 }).unwrap();
        assert_eq!(picture_ref(&one).crop, Some([70, 0, 30, 50]));
    }

    #[test]
    fn a_stored_picture_comes_back_whole_round_what_it_showed() {
        // A 100×50 file, cropped to x 20..60, y 10..30, shown 40×20 at (0, 0).
        let s = stored(Some([20, 10, 40, 20]), [40, 20], false, false);
        let out = whole(&s, [100, 50]).unwrap();
        let p = picture_ref(&out);
        assert_eq!((p.crop, p.size), (None, [100, 50]));
        let Shape::Raster { pos, size, .. } = &out else { panic!() };
        assert!(near(pos[0], -20.0) && near(pos[1], -10.0) && near(size[0], 100.0) && near(size[1], 50.0), "{pos:?} {size:?}");
        // Shown mirrored: the file's left part comes back on the right.
        let m = whole(&stored(Some([20, 10, 40, 20]), [40, 20], true, false), [100, 50]).unwrap();
        let Shape::Raster { pos, .. } = &m else { panic!() };
        assert!(near(pos[0], -40.0), "{pos:?}");
        // Whole already, or not stored: nothing to do.
        assert!(whole(&stored(None, [100, 50], false, false), [100, 50]).is_none());
        assert!(whole(&picture([0.0, 0.0], [8.0, 4.0], &grid(8, 4), false, 0.0), [8, 4]).is_none());
    }
}
