//! The signatures (`tools::similar`) of the pictures on the slides, for
//! finding pictures like a given one.
//!
//! Read only once find is asked for pictures, then kept up to date
//! while it is: worker threads decode each picture's bytes as the slide
//! keeps them (a stored picture's 96 px preview, a video's poster, a
//! small picture whole) and read its signature, so no media file is
//! opened. Signatures live only in memory; a few hundred bytes each.

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{RasterKind, Shape};
use crate::tools::similar::{Print, Signature};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// How often the slides are looked through for new pictures.
const LOOK_EVERY: Duration = Duration::from_millis(300);

struct Known {
    /// Keeps the bytes' address from going to other bytes while it is
    /// a key; see `picture_store::Known`.
    png: Weak<Vec<u8>>,
    /// `None` while a worker has it; `Some(None)` when it could not be read.
    sig: Option<Option<Print>>,
}

struct Job {
    key: usize,
    png: Arc<Vec<u8>>,
}

type Done = (usize, Option<Print>);

pub struct PictureIndex {
    /// By the address of the picture's bytes (`Arc::as_ptr`), which
    /// every copy of the shape shares.
    known: HashMap<usize, Known>,
    jobs: Option<Sender<Job>>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    looked: Option<Instant>,
    /// Pictures on the slides last looked through, and how many of
    /// them are read.
    total: usize,
    read: usize,
    /// Goes up whenever a signature comes in, so results know to update.
    generation: u64,
}

impl Default for PictureIndex {
    fn default() -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Self { known: HashMap::new(), jobs: None, done_tx, done_rx, looked: None, total: 0, read: 0, generation: 0 }
    }
}

/// Threads reading pictures: half the cores, at most four.
fn workers() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4))
}

/// The bytes a picture is read from, if `shape` is a picture, GIF or video.
fn bytes_of(shape: &Shape) -> Option<&Arc<Vec<u8>>> {
    match shape {
        Shape::Raster { png, kind: RasterKind::Image, .. } => Some(png),
        _ => None,
    }
}

impl PictureIndex {
    /// Take in what the workers read and, every `LOOK_EVERY` (or now,
    /// with `now`), send new pictures on `slides` off to be read, the
    /// first slide given first.
    pub fn tick<'a>(&mut self, slides: impl IntoIterator<Item = &'a Canvas>, now: bool) {
        while let Ok((key, sig)) = self.done_rx.try_recv() {
            if let Some(k) = self.known.get_mut(&key) {
                k.sig = Some(sig);
                self.generation += 1;
            }
        }
        if !now && self.looked.is_some_and(|t| t.elapsed() < LOOK_EVERY) {
            return;
        }
        self.looked = Some(Instant::now());
        // Bytes no shape holds any more (a job holds its own).
        self.known.retain(|_, k| k.png.strong_count() > 0);
        let mut seen = std::collections::HashSet::new();
        let (mut total, mut read) = (0, 0);
        for c in slides {
            for s in c.layers.iter().flat_map(|l| &l.shapes) {
                let Some(png) = bytes_of(s) else { continue };
                let key = Arc::as_ptr(png) as usize;
                if !seen.insert(key) {
                    continue;
                }
                total += 1;
                match self.known.get(&key) {
                    Some(Known { sig: Some(_), .. }) => read += 1,
                    Some(_) => {}
                    None => self.send(key, png),
                }
            }
        }
        (self.total, self.read) = (total, read);
    }

    /// The signature of the picture these bytes are, once read.
    pub fn get(&self, png: &Arc<Vec<u8>>) -> Option<Signature> {
        self.print(png).map(|p| p.signature())
    }

    /// What is kept of the picture these bytes are, once read.
    pub fn print(&self, png: &Arc<Vec<u8>>) -> Option<Print> {
        self.known.get(&(Arc::as_ptr(png) as usize)).and_then(|k| k.sig.flatten())
    }

    /// `(read, total)` pictures on the slides last looked through.
    pub fn progress(&self) -> (usize, usize) {
        (self.read, self.total)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn send(&mut self, key: usize, png: &Arc<Vec<u8>>) {
        let sent = self.start_workers() && self.jobs.as_ref().is_some_and(|tx| tx.send(Job { key, png: Arc::clone(png) }).is_ok());
        // Not sent: counted as read, with nothing to show for it.
        let sig = if sent { None } else { Some(None) };
        self.known.insert(key, Known { png: Arc::downgrade(png), sig });
    }

    fn start_workers(&mut self) -> bool {
        if self.jobs.is_some() {
            return true;
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let mut started = 0;
        for i in 0..workers() {
            let (rx, done) = (Arc::clone(&rx), self.done_tx.clone());
            match std::thread::Builder::new().name(format!("osn-similar-{i}")).spawn(move || worker(&rx, &done)) {
                Ok(_) => started += 1,
                Err(e) => log::warn!("could not start a picture reading thread: {e}"),
            }
        }
        if started == 0 {
            return false;
        }
        self.jobs = Some(tx);
        true
    }
}

fn worker(jobs: &Mutex<Receiver<Job>>, done: &Sender<Done>) {
    loop {
        let Ok(job) = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv() else { return };
        let sig = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Print::of_bytes(&job.png))).ok().flatten();
        if done.send((job.key, sig)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(png: Arc<Vec<u8>>) -> Shape {
        Shape::Raster {
            pos: [0.0, 0.0],
            size: [10.0, 10.0],
            png,
            px_size: [8, 8],
            color: [0; 4],
            rotation: 0.0,
            flip: false,
            kind: RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
        }
    }

    #[test]
    fn reads_every_picture_once_and_counts_progress() {
        let img = image::RgbaImage::from_fn(8, 8, |x, y| image::Rgba([(x * 30) as u8, (y * 30) as u8, 90, 255]));
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let png = Arc::new(bytes);
        let broken = Arc::new(vec![1, 2, 3]);
        let mut c = Canvas::default();
        // The same bytes twice (a copy) count once.
        c.layers[0].shapes.extend([picture(Arc::clone(&png)), picture(Arc::clone(&png)), picture(Arc::clone(&broken))]);
        let mut index = PictureIndex::default();
        index.tick([&c], true);
        assert_eq!(index.progress().1, 2);
        let start = Instant::now();
        while index.progress().0 < 2 {
            assert!(start.elapsed() < Duration::from_secs(10), "never read");
            std::thread::sleep(Duration::from_millis(5));
            index.tick([&c], true);
        }
        assert!(index.get(&png).is_some());
        assert!(index.get(&broken).is_none(), "not a picture: read, nothing kept");
        assert!(index.generation() >= 2);
    }
}
