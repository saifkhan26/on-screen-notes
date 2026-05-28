//! Pure-pixel compositor: alpha-blend an annotation layer onto a
//! background image. Lives in its own file so it can be unit-tested without
//! pulling in the rest of the screenshot pipeline.

use image::RgbaImage;

/// Standard "source-over" alpha blend: `dst = src + dst * (1 - src.a)`.
///
/// `bg` is mutated in place — we paint `fg` on top.
///
/// Both images must be the same size. If they're not, we crop `fg` to fit.
pub fn alpha_blend(bg: &mut RgbaImage, fg: &RgbaImage) {
    let w = bg.width().min(fg.width());
    let h = bg.height().min(fg.height());
    for y in 0..h {
        for x in 0..w {
            let s = fg.get_pixel(x, y).0; // RGBA bytes of source.
            if s[3] == 0 {
                continue; // fully transparent → skip work.
            }
            let d = bg.get_pixel_mut(x, y);
            let sa = s[3] as f32 / 255.0;
            let inv = 1.0 - sa;
            d.0[0] = (s[0] as f32 * sa + d.0[0] as f32 * inv) as u8;
            d.0[1] = (s[1] as f32 * sa + d.0[1] as f32 * inv) as u8;
            d.0[2] = (s[2] as f32 * sa + d.0[2] as f32 * inv) as u8;
            // Output alpha — for a screenshot we always want 255 (opaque).
            d.0[3] = 255;
        }
    }
}
