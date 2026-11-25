//! Pictures waiting for the browser extension to find who made them
//! ("Find artist"). The app only lists them, hands each one over as a
//! JPEG, and takes back the handle the user picked or a skip; finding the
//! artist (Google Lens, reading its results) is all the extension's.

use crate::canvas::shape::PictureRef;
use std::path::PathBuf;
use std::sync::Arc;

/// Pictures go to the browser no bigger than this on their long side:
/// plenty for a reverse image search.
pub const SIDE: u32 = 1600;

/// The pixels to hand over.
#[derive(Clone)]
pub enum Picture {
    /// A picture stored in the media folder: its file, and what of it
    /// the shape shows.
    Stored(PathBuf, PictureRef),
    /// One kept in its slide, or a video's or GIF's poster.
    Bytes(Arc<Vec<u8>>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Waiting,
    Tagged(String),
    Skipped,
}

pub struct Item {
    pub id: u64,
    /// The slide it is on, and what it is known by there
    /// (`browser_link::shape_key`).
    pub slide: usize,
    pub key: String,
    picture: Picture,
    pub state: State,
}

/// What `next` says.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Look this one up: its id, its place (from 1) and how many there are.
    Item { id: u64, index: usize, total: usize },
    Done { tagged: usize, skipped: usize, total: usize },
}

#[derive(Default)]
pub struct Queue {
    items: Vec<Item>,
    next_id: u64,
}

impl Queue {
    /// A new list, in place of any before it.
    pub fn start(&mut self, list: Vec<(usize, String, Picture)>) {
        self.items = list
            .into_iter()
            .map(|(slide, key, picture)| {
                self.next_id += 1;
                Item { id: self.next_id, slide, key, picture, state: State::Waiting }
            })
            .collect();
    }

    pub fn next(&self) -> Next {
        let total = self.items.len();
        match self.items.iter().position(|i| i.state == State::Waiting) {
            Some(at) => Next::Item { id: self.items[at].id, index: at + 1, total },
            None => {
                let tagged = self.items.iter().filter(|i| matches!(i.state, State::Tagged(_))).count();
                Next::Done { tagged, skipped: total - tagged, total }
            }
        }
    }

    pub fn picture(&self, id: u64) -> Option<Picture> {
        self.items.iter().find(|i| i.id == id).map(|i| i.picture.clone())
    }

    /// The answer for `id`: its slide and key, if it was still waiting.
    pub fn resolve(&mut self, id: u64, state: State) -> Option<(usize, String)> {
        let item = self.items.iter_mut().find(|i| i.id == id && i.state == State::Waiting)?;
        item.state = state;
        Some((item.slide, item.key.clone()))
    }

    /// The picture being looked up now (the one `next` hands out): its
    /// slide and key.
    pub fn current(&self) -> Option<(usize, String)> {
        self.items.iter().find(|i| i.state == State::Waiting).map(|i| (i.slide, i.key.clone()))
    }

    /// The picture known by `key` on `slide` was tagged without a lookup (a
    /// copy of it was): answered, if it was waiting.
    pub fn resolve_spot(&mut self, slide: usize, key: &str, state: State) -> bool {
        match self.items.iter_mut().find(|i| i.slide == slide && i.key == key && i.state == State::Waiting) {
            Some(i) => {
                i.state = state;
                true
            }
            None => false,
        }
    }

    /// How many are answered, and how many there are.
    pub fn progress(&self) -> (usize, usize) {
        (self.items.iter().filter(|i| i.state != State::Waiting).count(), self.items.len())
    }

    pub fn is_waiting(&self) -> bool {
        self.items.iter().any(|i| i.state == State::Waiting)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// The picture as a JPEG at most `SIDE` on its long side.
pub fn jpeg(picture: &Picture) -> Result<Vec<u8>, String> {
    let rgba = match picture {
        Picture::Stored(path, pref) => {
            let img = image::open(path).map_err(|e| format!("reading {}: {e}", path.display()))?.to_rgba8();
            pref.view(&img, SIDE)
        }
        Picture::Bytes(bytes) => {
            let img = image::load_from_memory(bytes).map_err(|e| format!("reading the picture: {e}"))?.to_rgba8();
            let (w, h) = img.dimensions();
            let k = (SIDE as f32 / w.max(h).max(1) as f32).min(1.0);
            if k < 1.0 {
                let size = |v: u32| ((v as f32 * k).round() as u32).max(1);
                image::imageops::resize(&img, size(w), size(h), image::imageops::FilterType::Triangle)
            } else {
                img
            }
        }
    };
    // JPEG has no see-through: what is see-through goes on white.
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (o, p) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = u16::from(p[3]);
        let over = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
        *o = image::Rgb([over(p[0]), over(p[1]), over(p[2])]);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&rgb).map_err(|e| format!("encoding: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut out = Vec::new();
        image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 0])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        Arc::new(out)
    }

    #[test]
    fn the_queue_hands_out_each_picture_once_then_says_done() {
        let mut q = Queue::default();
        q.start(vec![(0, "a.png".into(), Picture::Bytes(png(2, 2))), (3, "b.png".into(), Picture::Bytes(png(2, 2)))]);
        let Next::Item { id: first, index: 1, total: 2 } = q.next() else { panic!() };
        assert_eq!(q.next(), Next::Item { id: first, index: 1, total: 2 }, "asked again before an answer: the same one");
        assert_eq!(q.current(), Some((0, "a.png".to_string())));
        assert_eq!(q.resolve(first, State::Tagged("Aria".into())), Some((0, "a.png".into())));
        assert_eq!(q.resolve(first, State::Skipped), None, "answered once only");
        let Next::Item { id: second, index: 2, .. } = q.next() else { panic!() };
        assert!(q.is_waiting());
        assert_eq!(q.current(), Some((3, "b.png".to_string())));
        assert_eq!(q.resolve(second, State::Skipped), Some((3, "b.png".into())));
        assert_eq!(q.current(), None);
        assert_eq!(q.next(), Next::Done { tagged: 1, skipped: 1, total: 2 });
        assert_eq!(q.progress(), (2, 2));
        assert!(!q.is_waiting());
        q.start(vec![(1, "c.png".into(), Picture::Bytes(png(2, 2)))]);
        let Next::Item { id: third, index: 1, total: 1 } = q.next() else { panic!() };
        assert!(third > second, "ids are never reused");
    }

    #[test]
    fn pictures_go_out_as_small_jpegs_on_white() {
        let out = jpeg(&Picture::Bytes(png(3200, 1000))).unwrap();
        let img = image::load_from_memory_with_format(&out, image::ImageFormat::Jpeg).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (1600, 500));
        let p = img.get_pixel(10, 10);
        assert!(p[0] > 240 && p[1] > 240 && p[2] > 240, "see-through became white: {p:?}");
        assert!(jpeg(&Picture::Bytes(Arc::new(b"not a picture".to_vec()))).is_err());
    }
}
