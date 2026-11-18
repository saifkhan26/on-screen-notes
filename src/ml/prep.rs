//! A picture made ready for the vision model: flattened over white (a
//! transparent reference reads as drawn on paper), squashed to 256 × 256
//! — the model was trained on squashed pictures, not crops — and each
//! colour scaled to -1..1, colours first (3 planes of 256 × 256).

use image::{imageops, RgbImage, RgbaImage};

/// Side of the square the model reads.
pub const SIDE: u32 = 256;
/// Floats in one prepared picture.
pub const LEN: usize = 3 * (SIDE * SIDE) as usize;
/// Numbers in a picture's vector.
pub const DIM: usize = 768;

/// `img` as the model reads it: `LEN` floats.
pub fn prep(img: &RgbaImage) -> Vec<f32> {
    // A big picture is first shrunk with a fast box filter, to twice
    // what the model needs, so the finer resize below stays cheap.
    let (w, h) = img.dimensions();
    let shrunk;
    let img = if w.min(h) > 2 * SIDE {
        let s = (2 * SIDE) as f32 / w.min(h) as f32;
        let side = |v: u32| ((v as f32 * s).round() as u32).max(1);
        shrunk = imageops::thumbnail(img, side(w), side(h));
        &shrunk
    } else {
        img
    };
    let square = imageops::resize(&over_white(img), SIDE, SIDE, imageops::FilterType::Triangle);
    let n = (SIDE * SIDE) as usize;
    let mut out = vec![0.0; LEN];
    for (i, p) in square.pixels().enumerate() {
        for c in 0..3 {
            out[c * n + i] = p.0[c] as f32 / 127.5 - 1.0;
        }
    }
    out
}

/// The longest side to read a `w` × `h` picture at, at most: enough for
/// its short side to stay twice what the model reads, as the model
/// squashes both sides to `SIDE`. A very long strip is capped.
pub fn read_side(w: u32, h: u32) -> u32 {
    let (long, short) = (w.max(h).max(1) as f32, w.min(h).max(1) as f32);
    ((2 * SIDE) as f32 * long / short).ceil().clamp((2 * SIDE) as f32, 4096.0) as u32
}

/// `img` shrunk for reading (`read_side`), or as it is if small enough.
pub fn shrink(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let most = read_side(w, h);
    if w.max(h) <= most {
        return img.clone();
    }
    let s = most as f32 / w.max(h) as f32;
    imageops::thumbnail(img, ((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// `img` laid over white paper.
fn over_white(img: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let [r, g, b, a] = img.get_pixel(x, y).0;
        let a = a as u32;
        let mix = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([mix(r), mix(g), mix(b)])
    })
}

/// `v` scaled to length 1 (a zero vector stays zero).
pub fn unit(v: &[f32]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter().map(|x| x / n).collect()
    } else {
        v.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_becomes_three_planes_from_minus_one_to_one() {
        // Left half black, right half transparent (white paper shows).
        let img = RgbaImage::from_fn(1200, 700, |x, _| if x < 600 { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([0, 0, 0, 0]) });
        let p = prep(&img);
        assert_eq!(p.len(), LEN);
        let n = (SIDE * SIDE) as usize;
        let at = |c: usize, x: u32, y: u32| p[c * n + (y * SIDE + x) as usize];
        for c in 0..3 {
            assert_eq!(at(c, 10, 128), -1.0, "black");
            assert_eq!(at(c, 245, 128), 1.0, "transparent reads as white");
        }
        assert!(p.iter().all(|v| (-1.0..=1.0).contains(v)));
    }

    #[test]
    fn a_long_picture_is_read_big_enough_across() {
        assert_eq!(read_side(4000, 3000), 683, "short side 512");
        assert_eq!(read_side(300, 200), 768);
        assert_eq!(read_side(5000, 500), 4096, "a strip is capped");
        let tall = shrink(&RgbaImage::new(1000, 4000));
        assert_eq!(tall.dimensions(), (512, 2048));
        assert_eq!(shrink(&RgbaImage::new(600, 400)).dimensions(), (600, 400), "small enough as it is");
    }

    #[test]
    fn unit_vectors() {
        let u = unit(&[3.0, 4.0]);
        assert!((u[0] - 0.6).abs() < 1e-6 && (u[1] - 0.8).abs() < 1e-6);
        assert_eq!(unit(&[0.0, 0.0]), vec![0.0, 0.0]);
    }
}
