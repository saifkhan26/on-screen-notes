//! Copying a selection to the OS clipboard as a picture, so it can be
//! pasted into other apps (Krita, Photoshop, a chat or a browser).
//!
//! * One picture on its own goes over pixel for pixel at its full size:
//!   a stored picture's file with its crop and mirror, a video's or
//!   GIF's current frame, or the pixels kept in the slide.
//! * Anything else (ink, shapes, several objects) goes over as it is
//!   drawn, at the resolution it is on screen (never less than its
//!   natural size), cropped to the selection, on a transparent
//!   background. Text labels are left out: the off-screen renderer has
//!   no font yet and would draw placeholders.
//!
//! The image is built off the UI thread; a 4K picture takes a moment to
//! decode and encode.

use crate::canvas::canvas::{Canvas, Layer};
use crate::canvas::render::MediaFrames;
use crate::canvas::shape::Shape;
use crate::canvas::stroke::Stroke;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Longest side of a rendered selection, in pixels.
const MAX_SIDE: f32 = 8192.0;

/// Identifies an image read back from the clipboard as the one the app
/// put there: its size and a hash of its pixels.
pub type Fingerprint = (usize, usize, u64);

pub fn fingerprint(width: usize, height: usize, rgba: &[u8]) -> Fingerprint {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rgba.hash(&mut h);
    (width, height, h.finish())
}

/// The picture to put on the clipboard for these objects. `zoom` and
/// `pixels_per_point` are the view's, so drawn objects come out at their
/// on-screen sharpness; `frames` holds the videos' and GIFs' current
/// frames; `media_dir` is where stored pictures live.
pub fn image_of(
    strokes: Vec<Stroke>,
    shapes: Vec<Shape>,
    zoom: f32,
    pixels_per_point: f32,
    frames: &MediaFrames,
    media_dir: Option<&Path>,
) -> Option<image::RgbaImage> {
    if strokes.is_empty() && shapes.len() == 1 {
        if let Some(img) = picture_pixels(&shapes[0], frames, media_dir) {
            return Some(img);
        }
    }
    let shapes = shapes.into_iter().filter(|s| !matches!(s, Shape::Text { .. })).collect();
    let layer = Layer { strokes, shapes, ..Layer::default() };
    let [x0, y0, x1, y1] = layer.content_bounds()?;
    let (w, h) = (x1 - x0, y1 - y0);
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let ppp = pixels_per_point.max(0.01);
    let mut scale = (zoom * ppp).max(ppp);
    scale = scale.min(MAX_SIDE / w.max(h));
    let (pw, ph) = (((w * scale).ceil() as u32).max(1), ((h * scale).ceil() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    // `render_canvas` maps p → (p + pan) · zoom.
    let canvas = Canvas { layers: vec![layer], pan: [-x0, -y0], zoom: scale, ..Canvas::default() };
    crate::canvas::render::render_canvas(&mut pixmap.as_mut(), &canvas, None, None, 1.0, Some(frames));
    let mut out = image::RgbaImage::new(pw, ph);
    for (dst, src) in out.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(out)
}

/// A lone picture's own pixels, at full size.
fn picture_pixels(shape: &Shape, frames: &MediaFrames, media_dir: Option<&Path>) -> Option<image::RgbaImage> {
    let Shape::Raster { png, media, picture, .. } = shape else { return None };
    if let Some(frame) = media.as_ref().and_then(|m| frames.get(&m.id)) {
        let [w, h] = frame.size;
        let bytes: Vec<u8> = frame.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
        return image::RgbaImage::from_raw(w as u32, h as u32, bytes);
    }
    if let (Some(p), Some(dir)) = (picture, media_dir) {
        match image::open(dir.join(&p.file)) {
            Ok(img) => return Some(p.view(&img.to_rgba8(), u32::MAX)),
            Err(e) => log::warn!("could not read picture {} for the clipboard: {e}", p.file),
        }
    }
    image::load_from_memory(png).ok().map(|i| i.to_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::{BorderStyle, PictureRef, RasterKind};
    use std::sync::Arc;

    fn raster(png: Vec<u8>, picture: Option<PictureRef>) -> Shape {
        Shape::Raster {
            pos: [10.0, 10.0],
            size: [50.0, 50.0],
            png: Arc::new(png),
            px_size: [1, 1],
            color: [0; 4],
            rotation: 0.3,
            kind: RasterKind::Image,
            media: None,
            picture: picture.map(Box::new),
        }
    }

    fn png_of(img: &image::RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn a_lone_stored_picture_goes_over_at_full_size_with_its_crop() {
        let dir = std::env::temp_dir().join(format!("osn-clip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let full = image::RgbaImage::from_fn(300, 200, |x, _| image::Rgba([if x < 150 { 255 } else { 0 }, 0, 0, 255]));
        std::fs::write(dir.join("full.png"), png_of(&full)).unwrap();
        let tiny_preview = png_of(&image::RgbaImage::new(3, 2));
        let p = PictureRef { file: "full.png".into(), size: [150, 200], crop: Some([150, 0, 150, 200]), mirror: false };
        let img = image_of(Vec::new(), vec![raster(tiny_preview, Some(p))], 0.25, 1.0, &MediaFrames::new(), Some(&dir)).unwrap();
        assert_eq!(img.dimensions(), (150, 200), "the file's pixels, not the preview or the on-screen size");
        assert!(img.pixels().all(|px| px.0 == [0, 0, 0, 255]), "the right half, as cropped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lone_inline_picture_goes_over_as_its_pixels() {
        let src = image::RgbaImage::from_pixel(7, 5, image::Rgba([1, 2, 3, 128]));
        let img = image_of(Vec::new(), vec![raster(png_of(&src), None)], 3.0, 2.0, &MediaFrames::new(), None).unwrap();
        assert_eq!(img, src);
    }

    #[test]
    fn drawn_objects_render_cropped_on_transparent_at_screen_resolution() {
        let rect = Shape::Rect { a: [100.0, 100.0], b: [140.0, 120.0], color: [0, 255, 0, 255], stroke_width: 2.0, border: BorderStyle::Solid, rotation: 0.0 };
        let label = Shape::Text { pos: [0.0, 0.0], content: "left out".into(), font_size: 20.0, color: [255; 4], rotation: 0.0 };
        let img = image_of(Vec::new(), vec![rect, label], 2.0, 1.5, &MediaFrames::new(), None).unwrap();
        // 44 × 24 canvas units (outline padding included) at 2 × 1.5.
        assert_eq!(img.dimensions(), (132, 72), "the text label does not widen it");
        assert!(img.pixels().any(|p| p.0[1] > 200 && p.0[3] > 200), "the outline is drawn");
        assert_eq!(img.get_pixel(66, 36).0[3], 0, "inside the outline stays see-through");
    }

    /// Uses the real OS clipboard (what it held is put back after): an
    /// image put there reads back with the same fingerprint, which is
    /// how a paste recognises the app's own copy.
    #[test]
    #[ignore]
    fn the_os_clipboard_hands_back_the_same_image() {
        let mut cb = arboard::Clipboard::new().unwrap();
        let saved_text = cb.get_text().ok();
        let saved_image = cb.get_image().ok().map(|i| i.to_owned_img());
        let img = image::RgbaImage::from_fn(33, 17, |x, y| image::Rgba([x as u8 * 7, y as u8 * 13, 99, (x * y % 256) as u8]));
        let fp = fingerprint(33, 17, img.as_raw());
        cb.set_image(arboard::ImageData { width: 33, height: 17, bytes: std::borrow::Cow::Borrowed(img.as_raw()) }).unwrap();
        let back = cb.get_image().unwrap();
        let got = fingerprint(back.width, back.height, &back.bytes);
        match (saved_image, saved_text) {
            (Some(i), _) => cb.set_image(i).unwrap(),
            (None, Some(t)) => cb.set_text(t).unwrap(),
            (None, None) => cb.clear().unwrap(),
        }
        assert_eq!(got, fp, "transparency and every pixel survive the trip");
    }

    #[test]
    fn fingerprints_tell_images_apart() {
        let a = fingerprint(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(a, fingerprint(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]));
        assert_ne!(a, fingerprint(2, 1, &[1, 2, 3, 4, 5, 6, 7, 9]));
        assert_ne!(a, fingerprint(1, 2, &[1, 2, 3, 4, 5, 6, 7, 8]));
    }
}
