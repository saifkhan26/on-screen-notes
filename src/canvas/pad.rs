//! Drawing pads: painting into a picture's own pixels with a pen, a
//! pencil, a marker, an airbrush and an eraser, the way a paint program
//! does.
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
//! moved. Pressure sets each stamp's size exactly as it sets ink's width
//! off a pad (the same curve, from the settings' slider, and the same
//! floor). The pencil lays its colour in light layers that build up, a
//! little darker the harder it is pressed; a light touch is thinner and
//! lighter, never faint. The marker is a see-through felt tip of one
//! width; the airbrush a very soft spray whose pressure sets how much it
//! lays, and which keeps spraying while the pen is held still.

use crate::canvas::shape::{Obb, Shape};

/// Longest side of a picture that can be drawn on, in pixels.
pub const MAX_SIDE: u32 = 4096;

/// A new pad's size in pixels.
pub const NEW_SIZE: [u32; 2] = [2048, 1536];

/// Share of its colour one pass of the pencil lays down pressed firmly.
/// Light enough that passes build up: two give 75 %, three 88 %.
pub const PENCIL_PASS: f32 = 0.5;

/// Share of a firm pass the lightest touch lays: pressing harder darkens
/// the pencil a little, and a light touch still shows.
pub const PENCIL_LIGHT: f32 = 0.6;

/// How much of its colour the marker lays: see-through, so a second
/// stroke over the first darkens it (one stroke never darkens itself).
pub const MARKER_ALPHA: f32 = 0.5;

/// Share of its colour one pass of the airbrush lays at its middle,
/// pressed firmly. Light, as a spray is: passes build up.
pub const AIRBRUSH_PASS: f32 = 0.3;

/// Share of a firm pass the lightest touch lays: pressure is how much
/// paint the airbrush lets out.
pub const AIRBRUSH_LIGHT: f32 = 0.1;

/// Passes' worth the airbrush lays a second while held still.
pub const AIRBRUSH_HOLD: f32 = 1.5;

/// What a stroke on a pad does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brush {
    /// Solid ink with a crisp edge; pressure sets its width.
    Pen,
    /// A soft edge, and colour laid in light layers: one pass lays up to
    /// `PENCIL_PASS` of it, a little less the lighter the touch, and going
    /// over it again, in the same stroke or another, darkens it, as
    /// graphite does.
    Pencil,
    /// Takes paint away: to clear on a pad (its paper shows) or a picture
    /// with see-through parts; on an opaque picture with no paper, back
    /// to white (`pad_gpu::Session::erase_white`).
    Eraser,
    /// A felt tip: one width whatever the pressure, `MARKER_ALPHA` of the
    /// colour. Within a stroke the strongest stamp wins, so crossing
    /// itself does not darken it; a new stroke over it does.
    Marker,
    /// A very soft round spray of one size: pressure sets how much it
    /// lays (`airbrush_pass`), passes build up, and holding the pen
    /// still keeps spraying (`Walk::hold`).
    Airbrush,
}

/// How a brush's stamps look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Share of the radius that is fully covered before the edge fades.
    pub hard: f32,
    /// Gap between stamps, as a share of their diameter.
    pub spacing: f32,
    /// The edge fades in an S-curve rather than straight: the
    /// airbrush's soft, round falloff.
    pub soft: bool,
}

impl Brush {
    pub fn look(self) -> Look {
        match self {
            Brush::Pen => Look { hard: 0.8, spacing: 0.12, soft: false },
            Brush::Pencil => Look { hard: 0.3, spacing: 0.1, soft: false },
            Brush::Eraser => Look { hard: 0.6, spacing: 0.12, soft: false },
            Brush::Marker => Look { hard: 0.85, spacing: 0.08, soft: false },
            Brush::Airbrush => Look { hard: 0.0, spacing: 0.08, soft: true },
        }
    }

    /// A stamp's radius in pixels at pressure `p` (0..1, after the
    /// pressure curve), for a brush whose full radius is `r_max` pixels,
    /// `px` pixels making a screen point. The pen and pencil follow
    /// pressure as ink does off a pad (`stroke::width_at_pressure`), never
    /// thinner than a pixel; the eraser reaches between half and all of
    /// its size; the marker and the airbrush are always their size.
    pub fn radius(self, r_max: f32, p: f32, px: f32) -> f32 {
        match self {
            Brush::Pen | Brush::Pencil => 0.5 * crate::canvas::stroke::width_at_pressure(2.0 * r_max, p, px),
            Brush::Eraser => r_max * (0.5 + 0.5 * p.clamp(0.0, 1.0)),
            Brush::Marker | Brush::Airbrush => r_max,
        }
        .max(0.5)
    }

    /// Gap between stamps of radius `r`, in pixels.
    pub fn spacing(self, r: f32) -> f32 {
        (self.look().spacing * 2.0 * r).max(0.35)
    }

    /// Its stamps pile up, each adding a little colour (`flow`), rather
    /// than the strongest over a pixel winning.
    pub fn builds_up(self) -> bool {
        matches!(self, Brush::Pencil | Brush::Airbrush)
    }

    /// Share of its colour the brush lays at all: the marker is
    /// see-through.
    pub fn alpha(self) -> f32 {
        if self == Brush::Marker { MARKER_ALPHA } else { 1.0 }
    }

    /// Share of its colour one pass of the pencil lays at pressure `p`
    /// (after the curve): from `PENCIL_LIGHT` of `PENCIL_PASS` at the
    /// lightest touch to all of it pressed firmly.
    pub fn pencil_pass(p: f32) -> f32 {
        PENCIL_PASS * (PENCIL_LIGHT + (1.0 - PENCIL_LIGHT) * p.clamp(0.0, 1.0))
    }

    /// Share of its colour one pass of the airbrush lays at pressure `p`
    /// (after the curve): from `AIRBRUSH_LIGHT` of `AIRBRUSH_PASS` at the
    /// lightest touch to all of it pressed firmly.
    pub fn airbrush_pass(p: f32) -> f32 {
        AIRBRUSH_PASS * (AIRBRUSH_LIGHT + (1.0 - AIRBRUSH_LIGHT) * p.clamp(0.0, 1.0))
    }

    /// How many stamps of radius `r` one pass lays over a point on its
    /// line, each counted by how much of it covers the point: the stamp
    /// shader's edge, full out to `hard` of the radius, then fading (the
    /// airbrush's S-curve covers as much as a straight fade).
    fn stamps_over(self, r: f32) -> f32 {
        let ramp = (r + 0.5 - r * self.look().hard).max(1.0);
        (2.0 * r + 1.0 - ramp) / self.spacing(r)
    }

    /// Share of the colour a stamp of radius `r` lays at pressure `p`.
    /// The pencil's and the airbrush's are set by how many of their
    /// stamps cover a point, so one pass gives `pencil_pass(p)` (or
    /// `airbrush_pass(p)`) whatever its size; the others lay it all.
    pub fn flow(self, r: f32, p: f32) -> f32 {
        let pass = match self {
            Brush::Pencil => Self::pencil_pass(p),
            Brush::Airbrush => Self::airbrush_pass(p),
            _ => return 1.0,
        };
        1.0 - (1.0 - pass).powf(1.0 / self.stamps_over(r))
    }

    /// Stamps a second the airbrush lays in one place while the pen is
    /// held still there: `AIRBRUSH_HOLD` passes' worth.
    fn hold_rate(self, r: f32) -> f32 {
        AIRBRUSH_HOLD * self.stamps_over(r)
    }
}

/// One round stamp, in the picture's pixels. Laid out as the GPU reads it.
/// Within a stroke of the pen or the eraser the strongest stamp over a
/// pixel wins, so it never darkens where it crosses itself; the pencil's
/// pile up (`Brush::builds_up`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Stamp {
    pub c: [f32; 2],
    pub r: f32,
    /// Share of the colour it lays down (`Brush::flow`).
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
    /// Pixels a screen point takes (`Brush::radius`).
    px: f32,
    last: Option<([f32; 2], f32)>,
    /// Distance gone since the last stamp.
    carry: f32,
    /// Where the last stamp went.
    placed: Option<[f32; 2]>,
    /// Stamps laid since the last `hold`: the pen is not still.
    moved: bool,
    /// Airbrush stamps owed for time held still, not yet whole.
    held: f32,
}

impl Walk {
    /// A stroke of `brush`, `r_max` pixels its full radius, drawn where
    /// `px` pixels make a screen point.
    pub fn new(brush: Brush, r_max: f32, px: f32) -> Self {
        let px = if px.is_finite() && px > 0.0 { px } else { 1.0 };
        Self { brush, r_max: r_max.max(0.5), px, last: None, carry: 0.0, placed: None, moved: false, held: 0.0 }
    }

    /// `dt` seconds went by with the pen down: an airbrush held still
    /// (no stamps laid since last time) keeps spraying where it is. Other
    /// brushes lay nothing.
    pub fn hold(&mut self, dt: f32, out: &mut Vec<Stamp>) {
        if self.brush != Brush::Airbrush {
            return;
        }
        let Some((at, p)) = self.last else { return };
        if std::mem::take(&mut self.moved) {
            self.held = 0.0;
            return;
        }
        let r = self.radius(p);
        // A long stall (a dropped frame, a debugger) sprays at most 1/4 s.
        self.held += self.brush.hold_rate(r) * dt.clamp(0.0, 0.25);
        let n = self.held.floor();
        self.held -= n;
        for _ in 0..n as usize {
            out.push(self.stamp(at, r, p));
        }
    }

    fn radius(&self, p: f32) -> f32 {
        self.brush.radius(self.r_max, p, self.px)
    }

    /// The brush it lays.
    pub fn brush(&self) -> Brush {
        self.brush
    }

    fn stamp(&self, c: [f32; 2], r: f32, p: f32) -> Stamp {
        Stamp { c, r, flow: self.brush.flow(r, p) }
    }

    /// The pen reached `at` with pressure `p`: the stamps up to there go
    /// into `out`. The first point always gets one, so a tap leaves a dot.
    pub fn step(&mut self, at: [f32; 2], p: f32, out: &mut Vec<Stamp>) {
        let Some((from, p0)) = self.last else {
            out.push(self.stamp(at, self.radius(p), p));
            self.last = Some((at, p));
            self.carry = 0.0;
            self.placed = Some(at);
            self.moved = true;
            return;
        };
        let d = ((at[0] - from[0]).powi(2) + (at[1] - from[1]).powi(2)).sqrt();
        if d.is_finite() && d > 0.0 {
            let mut t = 0.0_f32;
            let mut laid = 0;
            while laid < MAX_STAMPS_PER_MOVE {
                let pp = p0 + (p - p0) * (t / d);
                let spacing = self.brush.spacing(self.radius(pp));
                let need = (spacing - self.carry).max(0.0);
                if t + need > d {
                    self.carry += d - t;
                    break;
                }
                t += need;
                self.carry = 0.0;
                laid += 1;
                let f = t / d;
                let c = [from[0] + (at[0] - from[0]) * f, from[1] + (at[1] - from[1]) * f];
                let pf = p0 + (p - p0) * f;
                let r = self.radius(pf);
                // A slow pen wobbling within a pixel goes a long way along
                // its path and hardly anywhere: no stamp there, or the
                // pencil's would pile up into a blot. Going back over a
                // line still lays them; it is farther than this.
                if self.placed.is_some_and(|q| ((c[0] - q[0]).powi(2) + (c[1] - q[1]).powi(2)).sqrt() < 0.85 * spacing) {
                    continue;
                }
                out.push(self.stamp(c, r, pf));
                self.placed = Some(c);
                self.moved = true;
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

/// What a closed shape `poly` (in a picture's pixels) covers of a
/// picture `size` big: the pixels round it, as `bounds` gives them, and
/// one byte a pixel there, row by row (255 = all, smooth at its edge).
/// `None` when it covers none.
pub fn cover(poly: &[[f32; 2]], size: [u32; 2]) -> Option<([u32; 4], Vec<u8>)> {
    if poly.len() < 3 || poly.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()) {
        return None;
    }
    let (mut a, mut b, mut c, mut d) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in poly {
        a = a.min(p[0]);
        b = b.min(p[1]);
        c = c.max(p[0]);
        d = d.max(p[1]);
    }
    let x0 = (a.floor().max(0.0) as u32).min(size[0]);
    let y0 = (b.floor().max(0.0) as u32).min(size[1]);
    let x1 = (c.ceil().max(0.0) as u32).min(size[0]);
    let y1 = (d.ceil().max(0.0) as u32).min(size[1]);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(poly[0][0] - x0 as f32, poly[0][1] - y0 as f32);
    for p in &poly[1..] {
        pb.line_to(p[0] - x0 as f32, p[1] - y0 as f32);
    }
    pb.close();
    let path = pb.finish()?;
    let mut mask = tiny_skia::Mask::new(x1 - x0, y1 - y0)?;
    // Even-odd, as lasso erase on ink counts what is inside a loop.
    mask.fill_path(&path, tiny_skia::FillRule::EvenOdd, true, tiny_skia::Transform::identity());
    let data = mask.data();
    data.iter().any(|&v| v > 0).then(|| ([x0, y0, x1, y1], data.to_vec()))
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
            a: [0.0, 0.0], b: [1.0, 1.0], color: [0; 4], stroke_width: 1.0, border: Default::default(), filled: false, rotation: 0.0, pin: 0, group: 0, tags: Vec::new(), turn: 0.0,
        };
        assert!(PadFrame::of(&rect, [10, 10]).is_none());
    }

    #[test]
    fn the_marker_is_one_width_and_see_through() {
        let b = Brush::Marker;
        assert_eq!(b.radius(6.0, 0.1, 1.0), 6.0, "pressure does not thin it");
        assert_eq!(b.radius(6.0, 1.0, 1.0), 6.0);
        assert!(!b.builds_up(), "a stroke never darkens itself");
        assert_eq!(b.flow(6.0, 0.3), 1.0);
        assert_eq!(b.alpha(), MARKER_ALPHA);
    }

    #[test]
    fn the_airbrush_lays_more_pressed_harder_and_sprays_while_held() {
        let b = Brush::Airbrush;
        assert_eq!(b.radius(20.0, 0.1, 1.0), 20.0, "one size");
        assert!(b.builds_up() && b.look().soft);
        assert!(b.flow(20.0, 1.0) > 2.0 * b.flow(20.0, 0.1), "pressure is how much it lays");
        // A pass lays `airbrush_pass` at its middle whatever the size: the
        // stamps over a point, each laying `flow`, add up to it.
        for r in [4.0_f32, 20.0, 80.0] {
            let n = b.stamps_over(r);
            let laid = 1.0 - (1.0 - b.flow(r, 1.0)).powf(n);
            assert!((laid - AIRBRUSH_PASS).abs() < 1e-4, "{r}: {laid}");
        }
        // Held still for a second it keeps spraying: `AIRBRUSH_HOLD`
        // passes' worth of stamps in that spot.
        let mut w = Walk::new(b, 20.0, 1.0);
        let mut out = Vec::new();
        w.step([50.0, 50.0], 1.0, &mut out);
        w.hold(0.016, &mut out);
        assert_eq!(out.len(), 1, "it had just moved");
        for _ in 0..60 {
            w.hold(1.0 / 60.0, &mut out);
        }
        let held = out.len() - 1;
        let want = AIRBRUSH_HOLD * b.stamps_over(20.0);
        assert!((held as f32 - want).abs() <= 1.5, "{held} vs {want}");
        assert!(out.iter().all(|s| s.c == [50.0, 50.0]));
        // Moving on lays by distance again; other brushes never spray.
        w.step([90.0, 50.0], 1.0, &mut out);
        let mut none = Vec::new();
        w.hold(0.5, &mut none);
        assert!(none.is_empty(), "it moved since");
        let mut pen = Walk::new(Brush::Pen, 5.0, 1.0);
        pen.step([0.0, 0.0], 1.0, &mut none);
        none.clear();
        pen.hold(1.0, &mut none);
        pen.hold(1.0, &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn a_lasso_covers_the_pixels_inside_it() {
        // A square 10..30, clipped by a 25 px picture.
        let (rect, cov) = cover(&[[10.0, 10.0], [30.0, 10.0], [30.0, 30.0], [10.0, 30.0]], [25, 40]).unwrap();
        assert_eq!(rect, [10, 10, 25, 30]);
        let w = (rect[2] - rect[0]) as usize;
        assert_eq!(cov.len(), w * 20);
        assert_eq!(cov[5 * w + 5], 255, "inside");
        // Half a pixel in at its edge: partly covered.
        let (_, cov) = cover(&[[10.5, 10.0], [20.0, 10.0], [20.0, 20.0], [10.5, 20.0]], [40, 40]).unwrap();
        assert!(cov[0] > 64 && cov[0] < 192, "{}", cov[0]);
        // Off the picture, or no area: nothing.
        assert!(cover(&[[50.0, 50.0], [60.0, 50.0], [60.0, 60.0]], [40, 40]).is_none());
        assert!(cover(&[[1.0, 1.0], [5.0, 5.0]], [40, 40]).is_none());
    }

    #[test]
    fn stamps_are_evenly_spaced_whatever_the_samples() {
        let mut w = Walk::new(Brush::Pen, 5.0, 1.0);
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
    fn pressure_sets_size_as_it_sets_inks_width_off_a_pad() {
        use crate::canvas::stroke::width_at_pressure;
        for p in [0.0, 0.03, 0.1, 0.5, 1.0] {
            let ink = width_at_pressure(20.0, p, 1.0);
            assert_eq!(Brush::Pen.radius(10.0, p, 1.0), 0.5 * ink, "{p}");
            assert_eq!(Brush::Pencil.radius(10.0, p, 1.0), 0.5 * ink, "the pencil only softens the edge");
        }
        assert!(Brush::Pen.radius(10.0, 0.1, 1.0) < Brush::Pen.radius(10.0, 1.0, 1.0));
        // Zoomed out the floor is a screen point's, as ink's.
        assert_eq!(Brush::Pen.radius(10.0, 0.0, 4.0), 0.5 * (0.8 * 4.0));
        // Never thinner than a pixel, however far zoomed in.
        assert!(Brush::Pen.radius(0.1, 0.0, 0.01) >= 0.5);
    }

    #[test]
    fn one_pencil_pass_lays_the_same_share_of_colour_at_any_size() {
        for r in [0.5_f32, 1.0, 4.0, 40.0] {
            for p in [0.0, 0.4, 1.0] {
                let pass = 1.0 - (1.0 - Brush::Pencil.flow(r, p)).powf(Brush::Pencil.stamps_over(r));
                assert!((pass - Brush::pencil_pass(p)).abs() < 1e-4, "{r} {p}: {pass}");
            }
        }
        // Pressed harder, a little darker; the lightest touch still shows.
        assert_eq!(Brush::pencil_pass(1.0), PENCIL_PASS);
        assert_eq!(Brush::pencil_pass(0.0), PENCIL_PASS * PENCIL_LIGHT);
        assert!(Brush::pencil_pass(0.3) < Brush::pencil_pass(0.6));
        assert!(Brush::Pencil.flow(4.0, 1.0) < 0.2, "light layers");
        assert_eq!((Brush::Pen.flow(4.0, 0.1), Brush::Eraser.flow(4.0, 0.1)), (1.0, 1.0));
        assert!(Brush::Pencil.builds_up() && !Brush::Pen.builds_up() && !Brush::Eraser.builds_up());
    }

    #[test]
    fn a_pen_wobbling_in_place_lays_no_pile_of_stamps() {
        let mut w = Walk::new(Brush::Pencil, 4.0, 1.0);
        let mut out = Vec::new();
        w.step([0.0, 0.0], 1.0, &mut out);
        // Back and forth within a fraction of the spacing (0.8 px): a long
        // way along the path, nowhere on the pad.
        for k in 0..200 {
            w.step([if k % 2 == 0 { 0.3 } else { -0.3 }, 0.0], 1.0, &mut out);
        }
        assert!(out.len() <= 2, "{}", out.len());
        // Going back over a line still lays stamps there.
        let mut w = Walk::new(Brush::Pencil, 4.0, 1.0);
        let mut out = Vec::new();
        for x in [0.0, 20.0, 0.0] {
            w.step([x, 0.0], 1.0, &mut out);
        }
        assert!(out.len() >= 45, "{}", out.len());
    }

    #[test]
    fn a_wild_jump_lays_a_bounded_number_of_stamps() {
        let mut w = Walk::new(Brush::Pen, 0.5, 1.0);
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
