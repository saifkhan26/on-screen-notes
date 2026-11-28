//! Paper grain: a light texture on a drawing pad's paper (`canvas::pad`),
//! under the ink. Each pad has its own (`Shape::Raster::grain`, switched
//! and sized from the paper bar; `None` is none). It is only drawn: none
//! of it goes into the pad's pixels.
//!
//! Its size is in slide units, not the pad's pixels, so it looks the same
//! on every pad, however many pixels the pad has (a stored picture keeps
//! only a small preview inline) and however it has been scaled.
//!
//! The grain is a square tile that repeats without a seam, made in code
//! (no file to ship), of fine bumps only: with no large features there is
//! nothing for the eye to catch repeating. It is drawn twice (`LAYERS`),
//! the second time a golden ratio bigger and shifted, so the two never
//! line up again on any pad. Further away smaller copies of the tile are
//! used, each the one above averaged down 2 × 2, as a GPU's mipmaps would
//! be (egui's wgpu backend makes none). Each texel is a shade from −1 (a
//! dip, darker) to 1 (a ridge, lighter), stored premultiplied as black or
//! white at that much alpha, so one texture drawn over the paper both
//! darkens and lightens it.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A pad's grain: how big and how strong.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grain {
    /// Slide units per texel of the tile's finest layer. In `SCALES`.
    pub scale: f32,
    /// How strongly it shows, 0..=1.
    pub strength: f32,
}

impl Grain {
    pub const DEFAULT: Self = Self { scale: 0.5, strength: 0.5 };

    /// Whether there is any grain to draw.
    pub fn shows(&self) -> bool {
        self.strength > 0.0 && self.scale > 0.0 && self.scale.is_finite()
    }
}

impl Default for Grain {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The sizes the paper bar offers, finest first.
pub const SCALES: std::ops::RangeInclusive<f32> = 0.25..=4.0;

/// Texels across the tile.
pub const TILE: usize = 512;

/// The tile and its smaller copies: 512, 256, 128, 64 and 32 across.
pub const LEVELS: usize = 5;

/// Alpha of a texel at shade ±1 and strength 1, all layers together.
pub const MAX_ALPHA: f32 = 0.45;

/// One drawing of the tile over the paper.
pub struct Layer {
    /// Its texels' size, times the grain's `scale`.
    pub size: f32,
    /// Where the tile starts, in tiles.
    pub offset: [f32; 2],
    /// Its share of the grain's strength.
    pub weight: f32,
}

/// The tile, then the tile a golden ratio bigger: the two together repeat
/// nowhere near a pad's size. Weighted so the sum is as strong as one.
pub const LAYERS: [Layer; 2] = [
    Layer { size: 1.0, offset: [0.0, 0.0], weight: std::f32::consts::FRAC_1_SQRT_2 },
    Layer { size: 1.618_034, offset: [0.382, 0.709], weight: std::f32::consts::FRAC_1_SQRT_2 },
];

/// A level is used while its texels are at least this many pixels
/// across; smaller than that the next one down is.
const MIN_TEXEL_PX: f32 = 0.75;

/// Past the last level the grain fades out, gone at texels this small.
const GONE_TEXEL_PX: f32 = 0.3;

/// How one layer of grain `g` is drawn where a slide unit is `px_per_unit`
/// pixels (on screen, or in an image).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// The level of the tile to draw with.
    pub level: usize,
    /// Slide units per texel of the full-size tile.
    pub texel: f32,
    /// Where the tile starts, in its texels.
    pub offset: [f32; 2],
    /// Alpha at shade ±1, before the paper's own opacity.
    pub alpha: f32,
}

impl Plan {
    /// Slide units across one tile.
    pub fn tile_units(&self) -> f32 {
        self.texel * TILE as f32
    }

    /// Slide units per texel of the level drawn with.
    pub fn level_texel(&self) -> f32 {
        self.texel * (1u32 << self.level) as f32
    }
}

/// How to draw each layer of `g` at `px_per_unit` pixels per slide unit;
/// layers too small to show are left out.
pub fn plan(g: &Grain, px_per_unit: f32) -> Vec<Plan> {
    if !g.shows() {
        return Vec::new();
    }
    LAYERS
        .iter()
        .filter_map(|l| {
            let texel = g.scale * l.size;
            let (level, fade) = pick(texel * px_per_unit)?;
            let alpha = (g.strength.min(1.0) * MAX_ALPHA * l.weight * fade).clamp(0.0, 1.0);
            let offset = [l.offset[0] * TILE as f32, l.offset[1] * TILE as f32];
            (alpha > 0.0).then_some(Plan { level, texel, offset, alpha })
        })
        .collect()
}

/// The grain's lightening (above 0) or darkening (below) at `at`, slide
/// units from the paper's top-left corner: every layer of `plans`
/// sampled as a GPU would and added up.
pub fn shade_at(plans: &[Plan], at: [f32; 2]) -> f32 {
    plans
        .iter()
        .map(|p| {
            let k = (1u32 << p.level) as f32;
            let u = (at[0] / p.texel + p.offset[0]) / k;
            let v = (at[1] / p.texel + p.offset[1]) / k;
            levels()[p.level].sample(u, v) * p.alpha
        })
        .sum()
}

/// One level of the tile.
pub struct Level {
    /// Texels across (and down).
    pub size: usize,
    /// Premultiplied RGBA: black or white at the shade's alpha.
    pub rgba: Vec<u8>,
}

impl Level {
    /// The shade (−1..=1) at texel `(x, y)`, wrapping round the tile.
    fn shade(&self, x: i64, y: i64) -> f32 {
        let n = self.size as i64;
        let i = (y.rem_euclid(n) * n + x.rem_euclid(n)) as usize * 4;
        let a = self.rgba[i + 3] as f32 / 255.0;
        if self.rgba[i] > 0 { a } else { -a }
    }

    /// The shade at `(u, v)` texels (texel centres at +0.5), blended
    /// between the four nearest as a GPU samples it.
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let top = self.shade(x0, y0) * (1.0 - fx) + self.shade(x0 + 1, y0) * fx;
        let bottom = self.shade(x0, y0 + 1) * (1.0 - fx) + self.shade(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// The tile's levels, largest first. Made the first time they are asked
/// for (a few tens of milliseconds).
pub fn levels() -> &'static [Level] {
    static LEVELS_MADE: OnceLock<Vec<Level>> = OnceLock::new();
    LEVELS_MADE.get_or_init(make_levels)
}

/// Which level to draw with, and how much of the grain shows (1, or less
/// once even the last level's texels are too small to see), when the
/// largest level's texels are `texel_px` pixels across. `None` when none
/// of it would show.
pub fn pick(texel_px: f32) -> Option<(usize, f32)> {
    if !(texel_px > 0.0) {
        return None;
    }
    let mut k = 0;
    let mut px = texel_px;
    while px < MIN_TEXEL_PX && k + 1 < LEVELS {
        px *= 2.0;
        k += 1;
    }
    let fade = ((px - GONE_TEXEL_PX) / (MIN_TEXEL_PX - GONE_TEXEL_PX)).clamp(0.0, 1.0);
    (fade > 0.0).then_some((k, fade))
}

/// A seeded hash of a lattice point, 0..=1.
fn hash(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

/// Smooth value noise at texel `(x, y)` with lattice points every
/// `period` texels, wrapping round the tile (`period` divides `TILE`).
fn value_noise(x: usize, y: usize, period: usize, seed: u32) -> f32 {
    let cells = (TILE / period) as u32;
    let (fx, fy) = (x as f32 / period as f32, y as f32 / period as f32);
    let (x0, y0) = (fx.floor(), fy.floor());
    let ease = |t: f32| t * t * (3.0 - 2.0 * t);
    let (u, v) = (ease(fx - x0), ease(fy - y0));
    let (x0, y0) = (x0 as u32 % cells, y0 as u32 % cells);
    let (x1, y1) = ((x0 + 1) % cells, (y0 + 1) % cells);
    let top = hash(x0, y0, seed) * (1.0 - u) + hash(x1, y0, seed) * u;
    let bottom = hash(x0, y1, seed) * (1.0 - u) + hash(x1, y1, seed) * u;
    top * (1.0 - v) + bottom * v
}

/// The tile's shades: a height field of small bumps, its tops lighter and
/// its hollows darker (curvature, the same every way round), a touch of
/// light from the top left so it reads as a surface, not as dirt (more
/// would streak it along the diagonal), and a little of the height itself
/// for the paper's uneven tone.
fn make_shades() -> Vec<f32> {
    // (period in texels, weight). Periods divide TILE, so every octave
    // wraps. Nothing bigger than 16: a large blot would show the eye
    // where the tile repeats.
    const OCTAVES: [(usize, f32); 5] = [(1, 0.18), (2, 0.30), (4, 0.26), (8, 0.16), (16, 0.10)];
    let n = TILE;
    let mut height = vec![0.0f32; n * n];
    for (o, &(period, w)) in OCTAVES.iter().enumerate() {
        for y in 0..n {
            for x in 0..n {
                let v = if period == 1 { hash(x as u32, y as u32, 99) } else { value_noise(x, y, period, o as u32 + 1) };
                height[y * n + x] += (v - 0.5) * w;
            }
        }
    }
    let h = |x: usize, y: usize| height[(y % n) * n + (x % n)];
    let mut shade = vec![0.0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            // Slope towards the light (top left), wrapping at the edges.
            let (xl, xr, yu, yd) = ((x + n - 1) % n, (x + 1) % n, (y + n - 1) % n, (y + 1) % n);
            let lit = (h(xl, y) - h(xr, y)) + (h(x, yu) - h(x, yd));
            // Above the mean of its neighbours two texels out: the top of
            // a bump, not of a single texel.
            let (xl, xr, yu, yd) = ((x + n - 2) % n, (x + 2) % n, (y + n - 2) % n, (y + 2) % n);
            let top = 4.0 * h(x, y) - h(xl, y) - h(xr, y) - h(x, yu) - h(x, yd);
            shade[y * n + x] = lit * 1.6 + top * 0.3 + h(x, y) * 0.6;
        }
    }
    // Spread to −1..=1: a shade 2.5 standard deviations out is full.
    let mut shade = standard(&shade);
    for s in &mut shade {
        *s = (*s / 2.5).clamp(-1.0, 1.0);
    }
    shade
}

/// `v` moved and scaled to a mean of 0 and a standard deviation of 1.
fn standard(v: &[f32]) -> Vec<f32> {
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let sd = (v.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt().max(1e-6);
    v.iter().map(|s| (s - mean) / sd).collect()
}

fn make_levels() -> Vec<Level> {
    let mut shades = make_shades();
    let mut size = TILE;
    let mut out = Vec::with_capacity(LEVELS);
    for k in 0..LEVELS {
        out.push(Level { size, rgba: to_rgba(&shades) });
        if k + 1 < LEVELS {
            let half = size / 2;
            let mut next = vec![0.0f32; half * half];
            for y in 0..half {
                for x in 0..half {
                    let at = |dx: usize, dy: usize| shades[(2 * y + dy) * size + 2 * x + dx];
                    next[y * half + x] = (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) * 0.25;
                }
            }
            shades = next;
            size = half;
        }
    }
    out
}

/// Shades as premultiplied RGBA: white over 0, black under it.
fn to_rgba(shades: &[f32]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(shades.len() * 4);
    for &s in shades {
        let a = (s.abs() * 255.0).round().clamp(0.0, 255.0) as u8;
        let c = if s > 0.0 { a } else { 0 };
        rgba.extend_from_slice(&[c, c, c, a]);
    }
    rgba
}

/// Paper colour `paper` (straight RGB) lightened (`amount` above 0) or
/// darkened (below) by that much, −1..=1.
pub fn shaded(paper: [u8; 3], amount: f32) -> [u8; 3] {
    let a = amount.abs().min(1.0);
    let to = if amount > 0.0 { 255.0 } else { 0.0 };
    paper.map(|c| (c as f32 * (1.0 - a) + to * a).round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tile_repeats_without_a_seam() {
        let l = &levels()[0];
        // Across the wrap, neighbours differ no more than they do inside.
        let step = |x0: i64, y0: i64, x1: i64, y1: i64| (l.shade(x0, y0) - l.shade(x1, y1)).abs();
        let n = TILE as i64;
        let (mut seam, mut inside) = (0.0, 0.0);
        for i in 0..n {
            seam += step(n - 1, i, n, i) + step(i, n - 1, i, n);
            inside += step(n / 2 - 1, i, n / 2, i) + step(i, n / 2 - 1, i, n / 2);
        }
        assert!(seam < inside * 1.25, "seam {seam} vs inside {inside}");
        // Wrapping is real wrapping: texel n is texel 0.
        assert_eq!(l.shade(n, 3), l.shade(0, 3));
    }

    #[test]
    fn shades_are_centred_and_use_the_range() {
        let l = &levels()[0];
        let s: Vec<f32> = (0..TILE * TILE).map(|i| l.shade((i % TILE) as i64, (i / TILE) as i64)).collect();
        let mean = s.iter().sum::<f32>() / s.len() as f32;
        assert!(mean.abs() < 0.02, "mean {mean}");
        assert!(s.iter().any(|&v| v > 0.9) && s.iter().any(|&v| v < -0.9));
    }

    #[test]
    fn the_tile_has_no_large_blots() {
        // Averaged over 32 × 32 texels, the shade is near flat: nothing
        // big enough to show where the tile repeats.
        let l = &levels()[0];
        let block = 32usize;
        let mut worst = 0.0f32;
        for by in 0..TILE / block {
            for bx in 0..TILE / block {
                let mut sum = 0.0;
                for y in 0..block {
                    for x in 0..block {
                        sum += l.shade((bx * block + x) as i64, (by * block + y) as i64);
                    }
                }
                worst = worst.max((sum / (block * block) as f32).abs());
            }
        }
        assert!(worst < 0.08, "a 32-texel block averages {worst}");
    }

    #[test]
    fn each_level_is_half_the_one_above() {
        let ls = levels();
        assert_eq!(ls.len(), LEVELS);
        for (k, l) in ls.iter().enumerate() {
            assert_eq!(l.size, TILE >> k);
            assert_eq!(l.rgba.len(), l.size * l.size * 4);
            // Premultiplied: colour never above alpha.
            assert!(l.rgba.chunks_exact(4).all(|p| p[0] <= p[3]));
        }
        // Averaged down: less contrast further away.
        let spread = |l: &Level| l.rgba.chunks_exact(4).map(|p| p[3] as f32).sum::<f32>() / (l.size * l.size) as f32;
        assert!(spread(&ls[2]) < spread(&ls[0]));
    }

    #[test]
    fn far_away_picks_smaller_levels_then_fades() {
        assert_eq!(pick(1.0), Some((0, 1.0)));
        assert_eq!(pick(0.5), Some((1, 1.0)));
        assert_eq!(pick(0.2), Some((2, 1.0)));
        // Past the last level: fading, then gone.
        let last = MIN_TEXEL_PX / (1 << (LEVELS - 1)) as f32;
        let (k, fade) = pick(last * 0.8).unwrap();
        assert_eq!(k, LEVELS - 1);
        assert!(fade > 0.0 && fade < 1.0);
        assert_eq!(pick(last * 0.3), None);
        assert_eq!(pick(0.0), None);
        assert_eq!(pick(f32::NAN), None);
    }

    #[test]
    fn the_size_is_in_slide_units() {
        // Same grain, same view: the same texels, whatever the pad's pixels.
        let g = Grain { scale: 2.0, strength: 1.0 };
        let p = plan(&g, 1.0);
        assert_eq!(p.len(), LAYERS.len());
        assert_eq!(p[0].texel, 2.0);
        assert_eq!(p[0].level, 0);
        assert!((p[1].texel - 2.0 * LAYERS[1].size).abs() < 1e-6);
        // Zoomed out 8×: a smaller level, same texel size in slide units.
        let far = plan(&g, 0.125);
        assert_eq!(far[0].level, 2);
        assert_eq!(far[0].level_texel(), 8.0);
        // Strength 0, or too far to see: nothing drawn.
        assert!(plan(&Grain { strength: 0.0, ..g }, 1.0).is_empty());
        assert!(plan(&g, 1e-4).is_empty());
    }

    #[test]
    fn layers_add_up_to_the_strength() {
        let g = Grain { scale: 1.0, strength: 1.0 };
        let total: f32 = plan(&g, 1.0).iter().map(|p| p.alpha * p.alpha).sum::<f32>().sqrt();
        assert!((total - MAX_ALPHA).abs() < 1e-3, "{total}");
    }

    #[test]
    fn shading_lightens_and_darkens_the_paper() {
        assert_eq!(shaded([200, 100, 0], 0.0), [200, 100, 0]);
        assert_eq!(shaded([200, 100, 0], 0.5), [228, 178, 128]);
        assert_eq!(shaded([200, 100, 0], -0.5), [100, 50, 0]);
    }

    #[test]
    fn grain_reads_with_fields_left_out_or_unknown() {
        let g: Grain = ron::from_str("()").unwrap();
        assert_eq!(g, Grain::DEFAULT);
        // The switch the app kept here for a short while is ignored.
        let g: Grain = ron::from_str("(on:true,scale:2.0)").unwrap();
        assert_eq!(g, Grain { scale: 2.0, strength: Grain::DEFAULT.strength });
    }

    /// The grain at a few sizes over a big sheet, as a PNG to look at for
    /// repeats: `OSN_SNAP_DIR=… cargo test snapshot_grain -- --ignored`.
    #[test]
    #[ignore]
    fn snapshot_grain() {
        let Ok(dir) = std::env::var("OSN_SNAP_DIR") else { return };
        let paper = [246u8, 238, 220];
        let (w, h) = (1200u32, 900u32);
        // (scale, slide units per image pixel): fine grain seen zoomed out
        // is where a repeat would show most.
        let views: [(f32, f32); 4] = [(0.5, 1.0), (0.5, 2.0), (1.0, 1.0), (2.0, 1.0)];
        for &(scale, units) in &views {
            let mut img = image::RgbaImage::new(w, h);
            let plans = plan(&Grain { scale, strength: 1.0 }, 1.0 / units);
            for y in 0..h {
                for x in 0..w {
                    let at = [(x as f32 + 0.5) * units, (y as f32 + 0.5) * units];
                    let c = shaded(paper, shade_at(&plans, at));
                    img.put_pixel(x, y, image::Rgba([c[0], c[1], c[2], 255]));
                }
            }
            img.save(std::path::Path::new(&dir).join(format!("grain-size{scale}-zoom{}.png", 1.0 / units))).unwrap();
        }
    }
}
