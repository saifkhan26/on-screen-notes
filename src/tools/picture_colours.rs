//! The colours that sum up a picture, for the arrange bar's palette
//! button: draw with one, or keep them all as the toolbar's palette.
//!
//! Colours are compared in Oklab, where equal distances look about
//! equally different, so the picks match what the eye sees rather than
//! what the RGB numbers say. The picture is shrunk to a few thousand
//! pixels first, so even a 4K frame takes a few milliseconds:
//!   1. median cut splits the pixels into twice as many boxes as
//!      colours wanted, each box's mean a candidate;
//!   2. a few rounds of k-means move the candidates to where the
//!      pixels really cluster;
//!   3. the picks: first the colour most of the picture is, then each
//!      time the candidate that is both common and unlike everything
//!      picked so far, so a small vivid accent (a red scarf) makes it in
//!      instead of a third shade of the background.

use image::RgbaImage;

/// Longest side the picture is shrunk to before counting colours.
const SAMPLE_SIDE: u32 = 96;
/// Pixels more transparent than this are not part of the picture.
const MIN_ALPHA: u8 = 128;
/// Rounds of k-means after the median cut.
const ROUNDS: usize = 6;

/// Up to `k` colours (opaque) that sum up `img`, the most telling first.
/// Fewer when the picture has fewer distinct colours; none when it is
/// all transparent.
pub fn picture_colours(img: &RgbaImage, k: usize) -> Vec<[u8; 4]> {
    if k == 0 || img.width() == 0 || img.height() == 0 {
        return Vec::new();
    }
    let long = img.width().max(img.height());
    let small;
    let img = if long > SAMPLE_SIDE {
        let s = SAMPLE_SIDE as f32 / long as f32;
        let (w, h) = (((img.width() as f32 * s).round() as u32).max(1), ((img.height() as f32 * s).round() as u32).max(1));
        small = image::imageops::thumbnail(img, w, h);
        &small
    } else {
        img
    };
    let pixels: Vec<[f32; 3]> = img.pixels().filter(|p| p.0[3] >= MIN_ALPHA).map(|p| oklab([p.0[0], p.0[1], p.0[2]])).collect();
    if pixels.is_empty() {
        return Vec::new();
    }

    let mut centres = median_cut(&pixels, k * 2);
    let counts = refine(&pixels, &mut centres);
    pick(&centres, &counts, k).into_iter().map(|c| {
        let [r, g, b] = srgb(c);
        [r, g, b, 255]
    }).collect()
}

/// Means of up to `n` boxes, each split along its widest axis at the
/// median until there are `n` (or nothing left worth splitting).
fn median_cut(pixels: &[[f32; 3]], n: usize) -> Vec<[f32; 3]> {
    let mut boxes: Vec<Vec<[f32; 3]>> = vec![pixels.to_vec()];
    while boxes.len() < n {
        // The box whose spread, weighted by how much of the picture it
        // holds, is biggest.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let (axis, range) = widest_axis(b);
                (i, axis, range * (b.len() as f32).sqrt())
            })
            .filter(|(_, _, score)| *score > 1e-4)
            .max_by(|a, b| a.2.total_cmp(&b.2));
        let Some((i, axis, _)) = pick else { break };
        let mut b = boxes.swap_remove(i);
        b.sort_by(|p, q| p[axis].total_cmp(&q[axis]));
        let upper = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(upper);
    }
    boxes.iter().map(|b| mean(b)).collect()
}

fn widest_axis(b: &[[f32; 3]]) -> (usize, f32) {
    (0..3)
        .map(|a| {
            let (lo, hi) = b.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| (lo.min(p[a]), hi.max(p[a])));
            (a, hi - lo)
        })
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .unwrap_or((0, 0.0))
}

fn mean(b: &[[f32; 3]]) -> [f32; 3] {
    let n = b.len().max(1) as f32;
    let s = b.iter().fold([0.0; 3], |s, p| [s[0] + p[0], s[1] + p[1], s[2] + p[2]]);
    [s[0] / n, s[1] / n, s[2] / n]
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Lloyd's k-means from `centres`; returns how many pixels each ends up
/// with. A centre no pixel is nearest to keeps its place, with a count
/// of zero.
fn refine(pixels: &[[f32; 3]], centres: &mut [[f32; 3]]) -> Vec<usize> {
    let mut counts = vec![0usize; centres.len()];
    for _ in 0..ROUNDS {
        let mut sums = vec![[0.0f32; 3]; centres.len()];
        counts.iter_mut().for_each(|c| *c = 0);
        for p in pixels {
            let i = nearest(centres, *p);
            counts[i] += 1;
            for a in 0..3 {
                sums[i][a] += p[a];
            }
        }
        for (i, c) in centres.iter_mut().enumerate() {
            if counts[i] > 0 {
                let n = counts[i] as f32;
                *c = [sums[i][0] / n, sums[i][1] / n, sums[i][2] / n];
            }
        }
    }
    counts
}

fn nearest(centres: &[[f32; 3]], p: [f32; 3]) -> usize {
    centres
        .iter()
        .enumerate()
        .min_by(|a, b| dist2(*a.1, p).total_cmp(&dist2(*b.1, p)))
        .map_or(0, |(i, _)| i)
}

/// Two colours closer than this (Oklab distance) count as one shade.
const SAME_SHADE: f32 = 0.04;

/// The heaviest candidate, then again and again the one scoring best on
/// "common × unlike what is picked", leaving out near-copies of a pick.
fn pick(centres: &[[f32; 3]], counts: &[usize], k: usize) -> Vec<[f32; 3]> {
    let total = counts.iter().sum::<usize>().max(1) as f32;
    let mut left: Vec<usize> = (0..centres.len()).filter(|&i| counts[i] > 0).collect();
    let mut out: Vec<[f32; 3]> = Vec::with_capacity(k);
    while out.len() < k && !left.is_empty() {
        let score = |i: usize| {
            let share = (counts[i] as f32 / total).sqrt();
            match out.iter().map(|c| dist2(*c, centres[i]).sqrt()).min_by(f32::total_cmp) {
                None => share,
                Some(d) => share * d,
            }
        };
        let Some((at, &best)) = left.iter().enumerate().max_by(|a, b| score(*a.1).total_cmp(&score(*b.1))) else { break };
        left.swap_remove(at);
        if out.iter().any(|c| dist2(*c, centres[best]).sqrt() < SAME_SHADE) {
            continue;
        }
        out.push(centres[best]);
    }
    out
}

fn to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let c = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// sRGB → Oklab (Björn Ottosson's).
fn oklab([r, g, b]: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (to_linear(r), to_linear(g), to_linear(b));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Oklab → sRGB, clamped into gamut.
fn srgb([lab_l, a, b]: [f32; 3]) -> [u8; 3] {
    let l = (lab_l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lab_l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s = (lab_l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        from_linear(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
        from_linear(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
        from_linear(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [u8; 4], b: [u8; 3]) -> bool {
        (0..3).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= 6)
    }

    #[test]
    fn oklab_round_trips() {
        for c in [[0, 0, 0], [255, 255, 255], [200, 30, 40], [12, 140, 250], [128, 128, 128]] {
            let back = srgb(oklab(c));
            assert!((0..3).all(|i| (i32::from(back[i]) - i32::from(c[i])).abs() <= 1), "{c:?} → {back:?}");
        }
    }

    #[test]
    fn the_main_colour_comes_first_and_a_small_accent_still_makes_it() {
        // Mostly blue sky, a band of green, a small red dot.
        let img = RgbaImage::from_fn(200, 200, |x, y| {
            let red = (90..100).contains(&x) && (90..100).contains(&y);
            image::Rgba(if red {
                [220, 20, 30, 255]
            } else if y > 150 {
                [40, 160, 60, 255]
            } else {
                [90, 150, 230, 255]
            })
        });
        let got = picture_colours(&img, 6);
        assert!(close(got[0], [90, 150, 230]), "{got:?}");
        assert!(got.iter().any(|c| close(*c, [40, 160, 60])), "{got:?}");
        assert!(got.iter().any(|c| close(*c, [220, 20, 30])), "{got:?}");
        assert!(got.iter().all(|c| c[3] == 255));
    }

    #[test]
    fn a_flat_picture_gives_one_colour_and_a_clear_one_none() {
        let flat = RgbaImage::from_pixel(50, 30, image::Rgba([10, 200, 90, 255]));
        let got = picture_colours(&flat, 8);
        assert_eq!(got.len(), 1);
        assert!(close(got[0], [10, 200, 90]));
        let clear = RgbaImage::from_pixel(50, 30, image::Rgba([255, 0, 0, 0]));
        assert!(picture_colours(&clear, 8).is_empty());
    }
}
