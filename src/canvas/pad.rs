//! Drawing pads: painting into a picture's own pixels with a pen, a
//! pencil and an eraser, the way a paint program does.
//!
//! A pad is an ordinary still picture whose pixels hold only the ink,
//! on clear, with its paper beside them: `Shape::Raster::paper`, a colour
//! and an opacity drawn behind the pixels, so either can change at any
//! time. A new one is clear pixels on white paper (`blank_png`). Any
//! other still picture that is not linked to a PureRef board can be drawn
//! on too, and a pad is copied, moved, found and stored like every other
//! picture. While one is open its pixels live on the GPU (`pad_gpu`);
//! this module is the part that needs no GPU: where the stamps go, how
//! each brush looks, and the paper.
//!
//! A stroke is a row of round stamps laid along the pen's path at a
//! spacing set by their size, so it looks the same however fast the pen
//! moved. Pressure sets each stamp's size and, for the pencil, how much
//! colour it lays down: a light touch draws thin and faint, a firm one
//! wide and dark.

use crate::canvas::shape::{Obb, Shape};

/// Longest side of a picture that can be drawn on, in pixels.
pub const MAX_SIDE: u32 = 4096;

/// A new pad's size in pixels.
pub const NEW_SIZE: [u32; 2] = [2048, 1536];

/// What a stroke on a pad does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brush {
    /// Solid ink with a crisp edge; pressure sets its width.
    Pen,
    /// A softer edge, and pressure sets both its width (as the pen's)
    /// and how much colour it lays down.
    Pencil,
    /// Takes paint away: to clear on a pad (its paper shows) or a picture
    /// with see-through parts; on an opaque picture with no paper, back
    /// to white (`pad_gpu::Session::erase_white`).
    Eraser,
}

/// How a brush's stamps look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Share of the radius that is fully covered before the edge fades.
    pub hard: f32,
    /// Gap between stamps, as a share of their diameter.
    pub spacing: f32,
}

impl Brush {
    pub fn look(self) -> Look {
        match self {
            Brush::Pen => Look { hard: 0.8, spacing: 0.12 },
            Brush::Pencil => Look { hard: 0.55, spacing: 0.1 },
            Brush::Eraser => Look { hard: 0.6, spacing: 0.12 },
        }
    }

    /// A stamp's radius and flow at pressure `p` (0..1, after the
    /// pressure curve), for a brush whose full radius is `r_max` pixels.
    pub fn stamp_size(self, r_max: f32, p: f32) -> (f32, f32) {
        let p = p.clamp(0.0, 1.0);
        let pen = (r_max * p.max(0.08)).max(0.5);
        match self {
            Brush::Pen => (pen, 1.0),
            Brush::Pencil => (pen, p.max(0.04)),
            Brush::Eraser => ((r_max * (0.5 + 0.5 * p)).max(0.5), 1.0),
        }
    }
}

/// One round stamp, in the picture's pixels. Laid out as the GPU reads it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Stamp {
    pub c: [f32; 2],
    pub r: f32,
    /// Share of the colour it lays down (0..1): the pencil's pressure.
    /// Within a stroke the strongest stamp over a pixel wins, so a
    /// stroke never darkens where it crosses itself.
    pub flow: f32,
}

/// Where a picture is on the canvas, to turn canvas points into its
/// pixels: its box (turned with it) and its pixel size. A picture shown
/// mirrored (`flip`) is drawn into the way its pixels are stored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PadFrame {
    pub obb: Obb,
    pub px: [u32; 2],
    pub flip: bool,
}

impl PadFrame {
    /// The frame of picture `shape`, whose pixels are `px` big. `None`
    /// for anything but a picture, or one with no area.
    pub fn of(shape: &Shape, px: [u32; 2]) -> Option<Self> {
        let Shape::Raster { flip, .. } = shape else { return None };
        let obb = shape.obb();
        if !(obb.half[0] > 0.0 && obb.half[1] > 0.0) || px[0] == 0 || px[1] == 0 {
            return None;
        }
        Some(Self { obb, px, flip: *flip })
    }

    /// Canvas point → pixel position (fractional; outside the picture
    /// when the point is).
    pub fn to_px(self, p: [f32; 2]) -> [f32; 2] {
        let l = self.obb.to_local(p);
        let [hw, hh] = self.obb.half;
        let mut u = (l[0] + hw) / (2.0 * hw) * self.px[0] as f32;
        let v = (l[1] + hh) / (2.0 * hh) * self.px[1] as f32;
        if self.flip {
            u = self.px[0] as f32 - u;
        }
        [u, v]
    }

    /// Pixels per canvas unit (the mean of the two sides', should the
    /// picture be stretched).
    pub fn px_per_unit(self) -> f32 {
        let sx = self.px[0] as f32 / (2.0 * self.obb.half[0]);
        let sy = self.px[1] as f32 / (2.0 * self.obb.half[1]);
        (sx * sy).sqrt()
    }

    /// The canvas point is on the picture.
    pub fn contains(self, p: [f32; 2]) -> bool {
        let l = self.obb.to_local(p);
        l[0].abs() <= self.obb.half[0] && l[1].abs() <= self.obb.half[1]
    }
}

/// Most stamps one pen move may lay, so a wild jump cannot flood the GPU.
const MAX_STAMPS_PER_MOVE: usize = 20_000;

/// Lays a stroke's stamps along the pen's path, evenly spaced however
/// the pen samples fell. Positions in pixels, pressures after the curve.
#[derive(Debug, Clone)]
pub struct Walk {
    brush: Brush,
    r_max: f32,
    last: Option<([f32; 2], f32)>,
    /// Distance gone since the last stamp.
    carry: f32,
}

impl Walk {
    pub fn new(brush: Brush, r_max: f32) -> Self {
        Self { brush, r_max: r_max.max(0.5), last: None, carry: 0.0 }
    }

    fn stamp(&self, c: [f32; 2], p: f32) -> Stamp {
        let (r, flow) = self.brush.stamp_size(self.r_max, p);
        Stamp { c, r, flow }
    }

    /// The pen reached `at` with pressure `p`: the stamps up to there go
    /// into `out`. The first point always gets one, so a tap leaves a dot.
    pub fn step(&mut self, at: [f32; 2], p: f32, out: &mut Vec<Stamp>) {
        let Some((from, p0)) = self.last else {
            out.push(self.stamp(at, p));
            self.last = Some((at, p));
            self.carry = 0.0;
            return;
        };
        let d = ((at[0] - from[0]).powi(2) + (at[1] - from[1]).powi(2)).sqrt();
        if d.is_finite() && d > 0.0 {
            let spacing_k = self.brush.look().spacing;
            let mut t = 0.0_f32;
            let mut laid = 0;
            while laid < MAX_STAMPS_PER_MOVE {
                let pp = p0 + (p - p0) * (t / d);
                let (r, _) = self.brush.stamp_size(self.r_max, pp);
                let spacing = (spacing_k * 2.0 * r).max(0.35);
                let need = (spacing - self.carry).max(0.0);
                if t + need > d {
                    self.carry += d - t;
                    break;
                }
                t += need;
                self.carry = 0.0;
                let f = t / d;
                out.push(self.stamp([from[0] + (at[0] - from[0]) * f, from[1] + (at[1] - from[1]) * f], p0 + (p - p0) * f));
                laid += 1;
            }
        }
        self.last = Some((at, p));
    }
}

/// The pixels stamps touch, as `[x0, y0, x1, y1]` (end exclusive) inside
/// a picture `size` big; `None` when they touch none.
pub fn bounds(stamps: &[Stamp], size: [u32; 2]) -> Option<[u32; 4]> {
    let (mut a, mut b, mut c, mut d) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in stamps {
        let e = s.r + 1.5;
        a = a.min(s.c[0] - e);
        b = b.min(s.c[1] - e);
        c = c.max(s.c[0] + e);
        d = d.max(s.c[1] + e);
    }
    if stamps.is_empty() || !(a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite()) {
        return None;
    }
    let x0 = a.floor().max(0.0) as u32;
    let y0 = b.floor().max(0.0) as u32;
    let x1 = (c.ceil().max(0.0) as u32).min(size[0]);
    let y1 = (d.ceil().max(0.0) as u32).min(size[1]);
    (x0 < x1 && y0 < y1).then_some([x0, y0, x1, y1])
}

/// Two rectangles' union (as `bounds` gives them).
pub fn union(a: Option<[u32; 4]>, b: [u32; 4]) -> [u32; 4] {
    match a {
        None => b,
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
    }
}

/// Straight RGBA → premultiplied, in place (what the GPU and egui blend).
pub fn premultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

/// Premultiplied RGBA → straight, in place (what a PNG holds).
pub fn unpremultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 {
            p[..3].fill(0);
        } else if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

/// The pixels hold no see-through ones: erasing paints white paper back.
pub fn is_opaque(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|p| p[3] == 255)
}

/// A new pad's pixels: clear (no ink yet), `size` big, as a PNG. Its
/// paper is the shape's.
pub fn blank_png(size: [u32; 2]) -> Option<Vec<u8>> {
    encode_png(&image::RgbaImage::new(size[0], size[1]))
}

/// The pixels as they show on `paper` (both straight RGBA): what copying
/// a pad puts on the clipboard.
pub fn on_paper(img: &mut image::RgbaImage, paper: [u8; 4]) {
    if paper[3] == 0 {
        return;
    }
    let pa = paper[3] as f32 / 255.0;
    for p in img.pixels_mut() {
        let a = p[3] as f32 / 255.0;
        let out_a = a + pa * (1.0 - a);
        if out_a <= 0.0 {
            continue;
        }
        for c in 0..3 {
            let v = (p[c] as f32 * a + paper[c] as f32 * pa * (1.0 - a)) / out_a;
            p[c] = v.round().clamp(0.0, 255.0) as u8;
        }
        p[3] = (out_a * 255.0).round() as u8;
    }
}

/// Opaque pixels drawn on white paper → the ink alone, on clear (straight
/// RGBA, in place): each pixel becomes the least opaque colour that gives
/// it back over white. Exact for ink of any colour laid on white; how
/// pads made before they had paper of their own are taken apart once.
pub fn lift_off_white(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[..3].iter().map(|&c| 255 - c as u32).max().unwrap_or(0);
        if a == 0 {
            p.fill(0);
            continue;
        }
        for c in &mut p[..3] {
            // c = ink·a + 255·(1 − a)  →  ink = (c − 255·(1 − a)) / a
            let v = (*c as i32 - (255 - a as i32)) * 255 / a as i32;
            *c = v.clamp(0, 255) as u8;
        }
        p[3] = a as u8;
    }
}

/// White, fully opaque: a new pad's paper until another is picked.
pub const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A pad's pixels as a PNG, quickly: a pad is saved each time it is put
/// away, and the media folder keeps it as it is.
pub fn encode_png(img: &image::RgbaImage) -> Option<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)
        .map_err(|e| log::warn!("could not encode a pad: {e}"))
        .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::RasterKind;
    use std::sync::Arc;

    fn picture(pos: [f32; 2], size: [f32; 2], rotation: f32, flip: bool) -> Shape {
        Shape::Raster {
            pos,
            size,
            png: Arc::new(Vec::new()),
            px_size: [200, 100],
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
        }
    }

    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn canvas_points_land_on_the_right_pixels() {
        // 100×50 canvas units shown, 200×100 pixels: 2 px a unit.
        let f = PadFrame::of(&picture([10.0, 20.0], [100.0, 50.0], 0.0, false), [200, 100]).unwrap();
        assert!(near(f.to_px([10.0, 20.0]), [0.0, 0.0]));
        assert!(near(f.to_px([110.0, 70.0]), [200.0, 100.0]));
        assert!(near(f.to_px([35.0, 30.0]), [50.0, 20.0]));
        assert!((f.px_per_unit() - 2.0).abs() < 1e-4);
        assert!(f.contains([60.0, 45.0]) && !f.contains([9.0, 45.0]) && !f.contains([60.0, 71.0]));
        // Shown mirrored: the left edge is the pixels' right edge.
        let f = PadFrame::of(&picture([10.0, 20.0], [100.0, 50.0], 0.0, true), [200, 100]).unwrap();
        assert!(near(f.to_px([10.0, 20.0]), [200.0, 0.0]));
        assert!(near(f.to_px([35.0, 30.0]), [150.0, 20.0]));
        // Turned a quarter clockwise about its centre (60, 45): the
        // picture's top-left corner now sits at the top right.
        let f = PadFrame::of(&picture([10.0, 20.0], [100.0, 50.0], std::f32::consts::FRAC_PI_2, false), [200, 100]).unwrap();
        assert!(near(f.to_px([60.0 + 25.0, 45.0 - 50.0]), [0.0, 0.0]));
        assert!(near(f.to_px([60.0, 45.0]), [100.0, 50.0]));
        assert!(f.contains([60.0 + 20.0, 45.0 + 40.0]) && !f.contains([60.0 + 40.0, 45.0]));
        // Not a picture.
        let rect = Shape::Rect {
            a: [0.0, 0.0], b: [1.0, 1.0], color: [0; 4], stroke_width: 1.0, border: Default::default(), rotation: 0.0, pin: 0, group: 0, tags: Vec::new(), turn: 0.0,
        };
        assert!(PadFrame::of(&rect, [10, 10]).is_none());
    }

    #[test]
    fn stamps_are_evenly_spaced_whatever_the_samples() {
        let mut w = Walk::new(Brush::Pen, 5.0);
        let mut out = Vec::new();
        // Full pressure: radius 5, spacing 0.12 · 10 = 1.2 px.
        w.step([0.0, 0.0], 1.0, &mut out);
        assert_eq!(out.len(), 1, "a tap leaves a dot");
        for x in [0.3_f32, 0.9, 5.0, 5.1, 12.0] {
            w.step([x, 0.0], 1.0, &mut out);
        }
        let xs: Vec<f32> = out.iter().map(|s| s.c[0]).collect();
        assert_eq!(xs.len(), 11, "{xs:?}");
        for pair in xs.windows(2) {
            assert!((pair[1] - pair[0] - 1.2).abs() < 1e-3, "{xs:?}");
        }
        // A pen held still lays nothing more.
        let n = out.len();
        w.step([12.0, 0.0], 1.0, &mut out);
        assert_eq!(out.len(), n);
    }

    #[test]
    fn pressure_sets_size_and_the_pencils_colour() {
        let (thin, _) = Brush::Pen.stamp_size(10.0, 0.1);
        let (wide, _) = Brush::Pen.stamp_size(10.0, 1.0);
        assert!(thin < wide && (wide - 10.0).abs() < 1e-4);
        // The pencil: width as the pen's, and its colour follows pressure.
        let (thin_p, light) = Brush::Pencil.stamp_size(10.0, 0.1);
        let (wide_p, firm) = Brush::Pencil.stamp_size(10.0, 1.0);
        assert_eq!((thin_p, wide_p), (thin, wide));
        assert!(light < 0.2 && firm == 1.0);
        // Never vanishing.
        assert!(Brush::Pen.stamp_size(0.1, 0.0).0 >= 0.5);
    }

    #[test]
    fn a_wild_jump_lays_a_bounded_number_of_stamps() {
        let mut w = Walk::new(Brush::Pen, 0.5);
        let mut out = Vec::new();
        w.step([0.0, 0.0], 0.0, &mut out);
        w.step([1.0e9, 0.0], 0.0, &mut out);
        assert!(out.len() <= MAX_STAMPS_PER_MOVE + 1);
    }

    #[test]
    fn bounds_hold_every_stamp_inside_the_picture() {
        let s = [Stamp { c: [10.0, 10.0], r: 2.0, flow: 1.0 }, Stamp { c: [30.0, 5.0], r: 4.0, flow: 1.0 }];
        let [x0, y0, x1, y1] = bounds(&s, [100, 100]).unwrap();
        assert!(x0 <= 8 && y0 == 0 && x1 >= 34 && y1 >= 12);
        assert_eq!(bounds(&s, [20, 20]).unwrap()[2], 20);
        assert!(bounds(&[Stamp { c: [-50.0, -50.0], r: 2.0, flow: 1.0 }], [100, 100]).is_none());
        assert!(bounds(&[], [100, 100]).is_none());
        assert_eq!(union(Some([0, 0, 5, 5]), [3, 3, 9, 4]), [0, 0, 9, 5]);
    }

    #[test]
    fn premultiplying_round_trips_and_opaque_stays_untouched() {
        let mut px = vec![200, 100, 50, 255, 200, 100, 50, 128, 9, 9, 9, 0];
        premultiply(&mut px);
        assert_eq!(&px[..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[100, 50, 25, 128]);
        assert_eq!(&px[8..], &[0, 0, 0, 0]);
        unpremultiply(&mut px);
        assert_eq!(&px[..4], &[200, 100, 50, 255]);
        assert!(px[4].abs_diff(200) <= 1 && px[5].abs_diff(100) <= 1 && px[6].abs_diff(50) <= 1);
        assert!(is_opaque(&[1, 2, 3, 255]) && !is_opaque(&[1, 2, 3, 255, 1, 2, 3, 254]));
    }

    #[test]
    fn a_new_pad_is_clear_and_small_on_disk() {
        let png = blank_png(NEW_SIZE).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (NEW_SIZE[0], NEW_SIZE[1]));
        assert!(img.pixels().all(|p| p.0 == [0, 0, 0, 0]));
        // Small enough to stay in the slide (`picture_store` moves 64 KB+).
        assert!(png.len() < 64 << 10, "{}", png.len());
    }

    #[test]
    fn paper_is_saved_on_pads_only_and_drawn_behind_the_pixels() {
        let plain = ron::to_string(&picture([0.0, 0.0], [10.0, 10.0], 0.0, false)).unwrap();
        assert!(!plain.contains("paper"), "other pictures save as before: {plain}");
        let png = encode_png(&image::RgbaImage::new(4, 4)).unwrap();
        let mut pad = picture([0.0, 0.0], [10.0, 10.0], 0.0, false);
        if let Shape::Raster { paper, png: p, px_size, .. } = &mut pad {
            *paper = Some([200, 30, 40, 255]);
            *p = Arc::new(png);
            *px_size = [4, 4];
        }
        let back: Shape = ron::from_str(&ron::to_string(&pad).unwrap()).unwrap();
        assert!(matches!(back, Shape::Raster { paper: Some([200, 30, 40, 255]), .. }));
        // Off screen (thumbnails, fill, copies of several objects) the
        // clear pixels show the paper.
        let layer = crate::canvas::canvas::Layer { shapes: vec![pad], ..Default::default() };
        let canvas = crate::canvas::canvas::Canvas { layers: vec![layer], ..Default::default() };
        let mut pixmap = tiny_skia::Pixmap::new(10, 10).unwrap();
        crate::canvas::render::render_canvas(&mut pixmap.as_mut(), &canvas, None, None, 1.0, None);
        let c = pixmap.pixel(5, 5).unwrap().demultiply();
        assert_eq!((c.red(), c.green(), c.blue(), c.alpha()), (200, 30, 40, 255));
    }

    /// Straight `ink` over straight `under`, as the screen blends them.
    fn over(ink: [u8; 4], under: [u8; 4]) -> [u8; 4] {
        let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba(ink));
        on_paper(&mut img, under);
        img.get_pixel(0, 0).0
    }

    #[test]
    fn the_paper_shows_through_the_ink() {
        // Clear pixels show the paper; solid ink hides it.
        assert_eq!(over([0, 0, 0, 0], [250, 240, 220, 255]), [250, 240, 220, 255]);
        assert_eq!(over([10, 20, 30, 255], [250, 240, 220, 255]), [10, 20, 30, 255]);
        // Half black over white is mid grey.
        let mid = over([0, 0, 0, 128], WHITE);
        assert!(mid[0].abs_diff(127) <= 1 && mid[3] == 255, "{mid:?}");
        // See-through paper stays see-through where there is no ink.
        assert_eq!(over([0, 0, 0, 0], [255, 255, 255, 100]), [255, 255, 255, 100]);
        assert_eq!(over([0, 0, 0, 0], [255, 255, 255, 0]), [0, 0, 0, 0]);
    }

    #[test]
    fn ink_lifted_off_white_looks_the_same_back_on_white() {
        let samples: [[u8; 4]; 6] =
            [[255, 255, 255, 255], [0, 0, 0, 255], [128, 128, 128, 255], [255, 128, 128, 255], [30, 90, 200, 255], [240, 250, 245, 255]];
        let mut px: Vec<u8> = samples.iter().flatten().copied().collect();
        lift_off_white(&mut px);
        // White paper goes, black ink stays solid.
        assert_eq!(&px[..4], &[0, 0, 0, 0]);
        assert_eq!(&px[4..8], &[0, 0, 0, 255]);
        // Grey becomes half-clear black; light red, half-clear red.
        assert_eq!(&px[8..12], &[0, 0, 0, 127]);
        assert_eq!(&px[12..16], &[255, 0, 0, 127]);
        for (i, want) in samples.iter().enumerate() {
            let got = over([px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]], WHITE);
            for c in 0..4 {
                assert!(got[c].abs_diff(want[c]) <= 2, "{i}: {got:?} vs {want:?}");
            }
        }
    }
}
