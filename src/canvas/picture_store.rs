//! Moving pictures kept in a slide out to the media folder.
//!
//! A pasted picture used to live in its slide as PNG bytes, which the
//! slide file spells out as decimal text (`png: [137, 80, …]`), four to
//! five characters a byte. A slide of a few hundred screenshots came to
//! over a gigabyte, and the autosave after any edit on it rewrote all of
//! it. Now such a picture is stored the way PureRef boards store theirs
//! (`PictureRef`): its bytes go unchanged to a file in the media folder,
//! and the slide keeps the file's name and a small preview.
//!
//! This runs in the background, for pictures already on slides as they
//! load and for new ones as they are pasted, captured or inserted. Worker
//! threads write each file and make its preview; once all of a slide's
//! pictures are done, the UI thread swaps them for their stored form in
//! one go and marks the slide for the autosave, so it is written small
//! once rather than at every step. A picture that cannot be stored stays
//! as it was.

use crate::canvas::canvas::{Canvas, UndoEntry};
use crate::canvas::shape::{PictureRef, RasterKind, Shape};
use crate::error::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// Pictures smaller than this stay in the slide: storing them would save
/// little.
const MIN_BYTES: usize = 64 << 10;
/// How often the slides are looked through.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// A picture's stored form.
#[derive(Clone)]
struct Stored {
    picture: PictureRef,
    preview: Arc<Vec<u8>>,
    preview_size: [u32; 2],
}

enum State {
    /// With a worker.
    Working,
    Stored(Stored),
    /// Could not be stored; it stays in the slide.
    Failed,
}

/// What is known about one picture's bytes. The weak reference keeps
/// their buffer's address from going to another buffer while it is a key.
struct Known {
    png: Weak<Vec<u8>>,
    state: State,
}

struct Job {
    key: usize,
    png: Arc<Vec<u8>>,
}

type Done = (usize, Option<Stored>);

pub struct PictureStore {
    /// The media folder; `None` stores nothing.
    dir: Option<PathBuf>,
    /// By the address of the picture's bytes (`Arc::as_ptr`), which
    /// every copy of the shape (undo, paste, pins) shares.
    known: HashMap<usize, Known>,
    /// Started with the first job.
    jobs: Option<Sender<Job>>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    working: usize,
    looked: Option<Instant>,
}

/// Threads storing pictures: half the cores, at most four.
fn workers() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4))
}

/// The bytes of a picture that should be stored, if `shape` is one: a
/// still picture kept in the slide, big enough to be worth it. Fill
/// stickers stay, as the eraser edits their pixels.
fn candidate(shape: &Shape) -> Option<&Arc<Vec<u8>>> {
    match shape {
        Shape::Raster { png, kind: RasterKind::Image, media: None, picture: None, .. } if png.len() >= MIN_BYTES => Some(png),
        _ => None,
    }
}

impl PictureStore {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Self { dir, known: HashMap::new(), jobs: None, done_tx, done_rx, working: 0, looked: None }
    }

    /// Pictures are being stored: their files may exist with nothing
    /// naming them yet, so media cleanup must wait.
    pub fn busy(&self) -> bool {
        self.working > 0
    }

    /// Once a frame: take in what the workers finished and, every
    /// `LOOK_EVERY`, look through the loaded slides (`first` first) to
    /// send new pictures off and swap in the ones done. Returns each
    /// swapped picture's old bytes and its new reference.
    pub fn tick(&mut self, canvases: &mut [Option<Canvas>], first: usize) -> Vec<(Arc<Vec<u8>>, PictureRef)> {
        while let Ok((key, stored)) = self.done_rx.try_recv() {
            self.working = self.working.saturating_sub(1);
            if let Some(k) = self.known.get_mut(&key) {
                k.state = stored.map_or(State::Failed, State::Stored);
            }
        }
        let Some(dir) = self.dir.clone() else { return Vec::new() };
        if self.looked.is_some_and(|t| t.elapsed() < LOOK_EVERY) {
            return Vec::new();
        }
        self.looked = Some(Instant::now());
        // Bytes no shape holds any more (a job holds its own).
        self.known.retain(|_, k| k.png.strong_count() > 0);
        let mut swaps = Vec::new();
        let order = std::iter::once(first).chain((0..canvases.len()).filter(|&i| i != first));
        for i in order {
            if let Some(c) = canvases.get_mut(i).and_then(Option::as_mut) {
                let before = swaps.len();
                self.look_through(c, &dir, &mut swaps);
                if swaps.len() > before {
                    log::info!("slide {}: {} pictures moved to the media folder", i + 1, swaps.len() - before);
                    crate::perf::line(format!("pictures: slide {} — {} moved to the media folder", i + 1, swaps.len() - before));
                }
            }
        }
        swaps
    }

    fn look_through(&mut self, c: &mut Canvas, dir: &Path, swaps: &mut Vec<(Arc<Vec<u8>>, PictureRef)>) {
        // Undoing a background plate's promotion turns the layer's first
        // shape back into the plate, which needs the picture's bytes.
        let plated: Vec<usize> = c
            .undo_log
            .iter()
            .chain(&c.redo_log)
            .filter_map(|e| match e {
                UndoEntry::PlatePromoted(li, _) => Some(*li),
                _ => None,
            })
            .collect();
        let skip = |li: usize, si: usize| si == 0 && plated.contains(&li);
        let (mut settled, mut stored) = (true, false);
        for (li, layer) in c.layers.iter().enumerate() {
            for (si, s) in layer.shapes.iter().enumerate() {
                let Some(png) = candidate(s).filter(|_| !skip(li, si)) else { continue };
                let key = Arc::as_ptr(png) as usize;
                match self.known.get(&key).map(|k| &k.state) {
                    None => {
                        self.send(key, png);
                        settled = false;
                    }
                    Some(State::Working) => settled = false,
                    Some(State::Stored(_)) => stored = true,
                    Some(State::Failed) => {}
                }
            }
        }
        // Swapped only once every picture is done, so the slide is saved
        // small once instead of after every few.
        if !(settled && stored) {
            return;
        }
        let mut changed = false;
        for (li, layer) in c.layers.iter_mut().enumerate() {
            for (si, s) in layer.shapes.iter_mut().enumerate() {
                let Some(png) = candidate(s).filter(|_| !skip(li, si)) else { continue };
                let key = Arc::as_ptr(png) as usize;
                let Some(Known { state: State::Stored(st), .. }) = self.known.get(&key) else { continue };
                // The file can be gone: stored for a copy of the picture
                // on a slide since deleted, whose media cleanup took it.
                // Stored again on the next look.
                if !dir.join(&st.picture.file).is_file() {
                    self.known.remove(&key);
                    continue;
                }
                let st = st.clone();
                let old = Arc::clone(png);
                if let Shape::Raster { png, px_size, picture, .. } = s {
                    *png = st.preview;
                    *px_size = st.preview_size;
                    *picture = Some(Box::new(st.picture.clone()));
                }
                swaps.push((old, st.picture));
                changed = true;
            }
        }
        if changed {
            c.dirty = true;
        }
    }

    fn send(&mut self, key: usize, png: &Arc<Vec<u8>>) {
        let sent = self.start_workers() && self.jobs.as_ref().is_some_and(|tx| tx.send(Job { key, png: Arc::clone(png) }).is_ok());
        let state = if sent {
            self.working += 1;
            State::Working
        } else {
            State::Failed
        };
        self.known.insert(key, Known { png: Arc::downgrade(png), state });
    }

    /// Whether there are workers to take jobs.
    fn start_workers(&mut self) -> bool {
        if self.jobs.is_some() {
            return true;
        }
        let Some(dir) = self.dir.clone() else { return false };
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let mut started = 0;
        for i in 0..workers() {
            let (rx, done, dir) = (Arc::clone(&rx), self.done_tx.clone(), dir.clone());
            let spawned = std::thread::Builder::new().name(format!("osn-store-{i}")).spawn(move || {
                if i == 0 {
                    sweep_leftovers(&dir);
                }
                worker(&rx, &done, &dir)
            });
            match spawned {
                Ok(_) => started += 1,
                Err(e) => log::warn!("could not start a picture store thread: {e}"),
            }
        }
        if started == 0 {
            // Without workers nothing would ever finish; keep everything
            // in the slides.
            self.dir = None;
            return false;
        }
        self.jobs = Some(tx);
        true
    }
}

/// Remove half-written files that an earlier run left when it closed
/// while storing (`store_bytes_into` writes to `.store-<process>-<n>.tmp`
/// and renames it into place when done).
fn sweep_leftovers(dir: &Path) {
    let mine = format!(".store-{}-", std::process::id());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".store-") && name.ends_with(".tmp") && !name.starts_with(&mine) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn worker(jobs: &Mutex<Receiver<Job>>, done: &Sender<Done>, dir: &Path) {
    loop {
        let Ok(job) = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv() else { return };
        let stored = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store(&job.png, dir)))
            .unwrap_or_else(|_| Err(anyhow!("storing it panicked")))
            .map_err(|e| log::warn!("a picture stays in its slide: {e:#}"))
            .ok();
        if done.send((job.key, stored)).is_err() {
            return;
        }
    }
}

/// Write a picture's bytes, unchanged, to a file in `dir` and make its
/// preview. Decoded first, so bytes that are not a picture the app can
/// read again are left where they are.
fn store(png: &[u8], dir: &Path) -> Result<Stored> {
    use image::ImageFormat as F;
    let format = image::guess_format(png).context("reading the picture")?;
    let ext = match format {
        F::Png => "png",
        F::Jpeg => "jpg",
        F::WebP => "webp",
        F::Bmp => "bmp",
        other => bail!("{other:?} is not stored"),
    };
    let img = image::load_from_memory_with_format(png, format).context("decoding the picture")?.to_rgba8();
    let (file, _) = crate::media::import::store_bytes_into(dir, png, ext)?;
    let picture = PictureRef { file, size: [img.width(), img.height()], crop: None, mirror: false };
    let preview = picture.view(&img, crate::media::pureref::PREVIEW_SIDE);
    let (bytes, _) = crate::media::pureref::encode_picture(&preview, 80)?;
    Ok(Stored { picture, preview: Arc::new(bytes), preview_size: [preview.width(), preview.height()] })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("osn-picture-store-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A PNG of noise, which does not compress, so it is `side²·3` bytes
    /// or more.
    fn noise_png(side: u32, seed: u32) -> Arc<Vec<u8>> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        let img = image::RgbImage::from_fn(side, side, |_, _| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let [a, b, c, _] = x.to_le_bytes();
            image::Rgb([a, b, c])
        });
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        Arc::new(out)
    }

    fn picture(png: &Arc<Vec<u8>>, kind: RasterKind) -> Shape {
        Shape::Raster {
            pos: [5.0, 6.0],
            size: [300.0, 300.0],
            png: Arc::clone(png),
            px_size: [200, 200],
            color: [0; 4],
            rotation: 0.25,
            kind,
            media: None,
            picture: None,
            pin: 7,
            flip: false,
            group: 0,
        }
    }

    /// Tick until `done` holds or a few seconds pass.
    fn run(store: &mut PictureStore, slides: &mut [Option<Canvas>], mut done: impl FnMut(&[Option<Canvas>]) -> bool) -> Vec<(Arc<Vec<u8>>, PictureRef)> {
        let mut swaps = Vec::new();
        let start = Instant::now();
        while !done(slides) {
            assert!(start.elapsed() < Duration::from_secs(20), "timed out");
            store.looked = None;
            swaps.extend(store.tick(slides, 0));
            std::thread::sleep(Duration::from_millis(5));
        }
        swaps
    }

    fn stored(s: &Shape) -> Option<&PictureRef> {
        match s {
            Shape::Raster { picture: Some(p), .. } => Some(p),
            _ => None,
        }
    }

    #[test]
    fn big_pictures_go_to_files_and_the_slide_keeps_a_preview() {
        let media = TempDir::new("big");
        let (big, other, small) = (noise_png(200, 1), noise_png(160, 2), noise_png(40, 3));
        assert!(big.len() >= MIN_BYTES && other.len() >= MIN_BYTES && small.len() < MIN_BYTES);
        let mut c = Canvas::default();
        c.dirty = false;
        c.layers[0].shapes.push(picture(&big, RasterKind::Image));
        c.layers[0].shapes.push(picture(&small, RasterKind::Image));
        c.layers[0].shapes.push(picture(&big, RasterKind::Fill));
        c.layers[0].shapes.push(picture(&other, RasterKind::Image));
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(Some(media.0.clone()));
        let swaps = run(&mut store, &mut slides, |s| s[0].as_ref().unwrap().layers[0].shapes.iter().filter_map(stored).count() == 2);
        assert!(!store.busy());

        let c = slides[0].as_ref().unwrap();
        assert!(c.dirty, "the slide is saved again, small");
        let shapes = &c.layers[0].shapes;
        assert!(stored(&shapes[1]).is_none(), "a small picture stays");
        assert!(stored(&shapes[2]).is_none(), "a fill stays");
        for (i, original) in [(0, &big), (3, &other)] {
            let Shape::Raster { pos, size, png, px_size, rotation, kind, pin, picture: Some(p), .. } = &shapes[i] else { panic!("not stored") };
            assert_eq!((*pos, *size, *rotation, *kind, *pin), ([5.0, 6.0], [300.0, 300.0], 0.25, RasterKind::Image, 7), "placed as before");
            assert_eq!(std::fs::read(media.0.join(&p.file)).unwrap(), **original, "the file holds the very bytes");
            let side = if i == 0 { 200 } else { 160 };
            assert_eq!((p.size, p.crop, p.mirror), ([side, side], None, false));
            let preview = image::load_from_memory(png).unwrap();
            assert_eq!([preview.width(), preview.height()], *px_size);
            assert_eq!(px_size[0].max(px_size[1]), crate::media::pureref::PREVIEW_SIDE);
            assert!(png.len() < original.len());
        }
        let mut swapped: Vec<_> = swaps.iter().map(|(old, p)| (Arc::as_ptr(old), p.file.clone())).collect();
        swapped.sort();
        let mut want = vec![(Arc::as_ptr(&big), stored(&shapes[0]).unwrap().file.clone()), (Arc::as_ptr(&other), stored(&shapes[3]).unwrap().file.clone())];
        want.sort();
        assert_eq!(swapped, want, "the cache is told what each became");
    }

    #[test]
    fn a_copy_of_a_stored_picture_is_swapped_at_once_and_a_lost_file_is_written_again() {
        let media = TempDir::new("copy");
        let png = noise_png(200, 4);
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture(&png, RasterKind::Image));
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(Some(media.0.clone()));
        run(&mut store, &mut slides, |s| stored(&s[0].as_ref().unwrap().layers[0].shapes[0]).is_some());
        let file = media.0.join(&stored(&slides[0].as_ref().unwrap().layers[0].shapes[0]).unwrap().file);

        // An undo brings back the picture as it was, sharing its bytes.
        slides[0].as_mut().unwrap().layers[0].shapes.push(picture(&png, RasterKind::Image));
        store.looked = None;
        let swaps = store.tick(&mut slides, 0);
        assert_eq!(swaps.len(), 1, "known already: no new work");
        assert!(stored(&slides[0].as_ref().unwrap().layers[0].shapes[1]).is_some());

        // Again, after media cleanup took the file.
        std::fs::remove_file(&file).unwrap();
        slides[0].as_mut().unwrap().layers[0].shapes.push(picture(&png, RasterKind::Image));
        run(&mut store, &mut slides, |s| stored(&s[0].as_ref().unwrap().layers[0].shapes[2]).is_some());
        assert_eq!(std::fs::read(&file).unwrap(), *png, "written again");
    }

    #[test]
    fn what_cannot_be_stored_stays_in_the_slide() {
        let media = TempDir::new("bad");
        let junk = Arc::new(vec![7u8; MIN_BYTES]);
        let mut broken = (*noise_png(300, 5)).clone();
        broken.truncate(broken.len() / 2);
        let broken = Arc::new(broken);
        assert!(broken.len() >= MIN_BYTES);
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture(&junk, RasterKind::Image));
        c.layers[0].shapes.push(picture(&broken, RasterKind::Image));
        c.dirty = false;
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(Some(media.0.clone()));
        let start = Instant::now();
        loop {
            store.looked = None;
            store.tick(&mut slides, 0);
            if store.known.len() == 2 && !store.busy() {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let c = slides[0].as_ref().unwrap();
        assert!(c.layers[0].shapes.iter().all(|s| stored(s).is_none()));
        assert!(!c.dirty);
        assert_eq!(std::fs::read_dir(&media.0).unwrap().count(), 0, "no file written");
        // Not tried again.
        store.looked = None;
        store.tick(&mut slides, 0);
        assert!(!store.busy());
    }

    #[test]
    fn a_promoted_plate_stays_while_an_undo_can_turn_it_back() {
        let media = TempDir::new("plate");
        let png = noise_png(200, 6);
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture(&png, RasterKind::Image));
        c.undo_log.push(UndoEntry::PlatePromoted(0, 1.0));
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(Some(media.0.clone()));
        store.tick(&mut slides, 0);
        assert!(store.known.is_empty() && !store.busy(), "left alone");
        slides[0].as_mut().unwrap().undo_log.clear();
        run(&mut store, &mut slides, |s| stored(&s[0].as_ref().unwrap().layers[0].shapes[0]).is_some());
    }

    #[test]
    fn a_slide_is_swapped_when_all_its_pictures_are_done() {
        let media = TempDir::new("whole");
        let (a, b) = (noise_png(200, 8), noise_png(200, 9));
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture(&a, RasterKind::Image));
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(Some(media.0.clone()));
        run(&mut store, &mut slides, |s| stored(&s[0].as_ref().unwrap().layers[0].shapes[0]).is_some());
        // A new paste on a slide whose other picture is already known.
        let c = slides[0].as_mut().unwrap();
        c.layers[0].shapes.push(picture(&b, RasterKind::Image));
        c.layers[0].shapes.push(picture(&a, RasterKind::Image));
        store.looked = None;
        assert!(store.tick(&mut slides, 0).is_empty(), "waits for the new one");
        assert!(stored(&slides[0].as_ref().unwrap().layers[0].shapes[2]).is_none());
        let swaps = run(&mut store, &mut slides, |s| s[0].as_ref().unwrap().layers[0].shapes.iter().all(|s| stored(s).is_some()));
        assert_eq!(swaps.len(), 2, "both at once");
    }

    #[test]
    fn a_stored_slide_round_trips_and_is_small() {
        let media = TempDir::new("save");
        let png = noise_png(300, 10);
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture(&png, RasterKind::Image));
        let inline = crate::persistence::serialize_canvas(&c).unwrap().len();
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(Some(media.0.clone()));
        run(&mut store, &mut slides, |s| stored(&s[0].as_ref().unwrap().layers[0].shapes[0]).is_some());
        let text = crate::persistence::serialize_canvas(slides[0].as_ref().unwrap()).unwrap();
        // Noise is the worst case for a preview; real pictures shrink far more.
        assert!(text.len() * 4 < inline, "{} vs {inline}", text.len());
        let back: Canvas = ron::from_str(&text).unwrap();
        assert_eq!(stored(&back.layers[0].shapes[0]), stored(&slides[0].as_ref().unwrap().layers[0].shapes[0]));
    }

    /// Checks slides the app has stored pictures for against the same
    /// slides from before: every stored picture's file holds exactly the
    /// bytes the slide had, and everything else is unchanged. Run on
    /// copies, never on the real data folder:
    ///
    ///   OSN_TEST_BEFORE=<backup>/canvases OSN_TEST_AFTER=<copy> cargo test --release real_slides_stored_as_they_were -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_slides_stored_as_they_were() {
        let before = PathBuf::from(std::env::var_os("OSN_TEST_BEFORE").expect("set OSN_TEST_BEFORE"));
        let after = PathBuf::from(std::env::var_os("OSN_TEST_AFTER").expect("set OSN_TEST_AFTER"));
        let read = |p: &Path| -> Canvas {
            let text = std::fs::read_to_string(p).unwrap();
            crate::persistence::parse_canvas(&text, 0).unwrap()
        };
        let mut files: Vec<_> = std::fs::read_dir(&before).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).filter(|n| n.to_string_lossy().starts_with("canvas_") && n.to_string_lossy().ends_with(".ron")).collect();
        files.sort();
        let (mut total_before, mut total_after, mut total_stored) = (0u64, 0u64, 0usize);
        for name in &files {
            let (b_path, a_path) = (before.join(name), after.join("canvases").join(name));
            let (b_len, a_len) = (std::fs::metadata(&b_path).unwrap().len(), std::fs::metadata(&a_path).unwrap().len());
            let (b, a) = (read(&b_path), read(&a_path));
            assert_eq!(b.layers.len(), a.layers.len(), "{name:?}");
            let mut stored_here = 0;
            for (lb, la) in b.layers.iter().zip(&a.layers) {
                assert_eq!(lb.shapes.len(), la.shapes.len(), "{name:?}");
                assert_eq!(ron::to_string(&lb.strokes).unwrap(), ron::to_string(&la.strokes).unwrap(), "{name:?}: ink changed");
                assert_eq!(ron::to_string(&lb.image).unwrap(), ron::to_string(&la.image).unwrap(), "{name:?}: plate changed");
                for (sb, sa) in lb.shapes.iter().zip(&la.shapes) {
                    match (sb, sa) {
                        (
                            Shape::Raster { pos, size, png, color, rotation, kind, media: None, picture: None, pin, .. },
                            Shape::Raster { pos: p2, size: s2, color: c2, rotation: r2, kind: k2, media: None, picture: Some(p), pin: pin2, .. },
                        ) => {
                            assert_eq!((pos, size, color, rotation, kind, pin), (p2, s2, c2, r2, k2, pin2), "{name:?}: moved");
                            let file = std::fs::read(after.join("media").join(&p.file)).unwrap();
                            assert!(file == **png, "{name:?}: {} differs from the picture it holds", p.file);
                            let img = image::load_from_memory(png).unwrap();
                            assert_eq!(p.size, [img.width(), img.height()]);
                            stored_here += 1;
                        }
                        _ => assert_eq!(ron::to_string(sb).unwrap(), ron::to_string(sa).unwrap(), "{name:?}: a shape changed"),
                    }
                }
            }
            println!("{}: {:.1} MB -> {:.1} MB, {stored_here} pictures stored", name.to_string_lossy(), b_len as f64 / 1e6, a_len as f64 / 1e6);
            total_before += b_len;
            total_after += a_len;
            total_stored += stored_here;
        }
        println!("all: {:.1} MB -> {:.1} MB, {total_stored} pictures stored", total_before as f64 / 1e6, total_after as f64 / 1e6);
    }

    #[test]
    fn only_other_runs_leftovers_are_swept() {
        let media = TempDir::new("sweep");
        let names = [".store-1-0.tmp".to_string(), format!(".store-{}-0.tmp", std::process::id()), ".import-1-0.tmp".into(), "0123456789abcdef.png".into()];
        for n in &names {
            std::fs::write(media.0.join(n), b"x").unwrap();
        }
        sweep_leftovers(&media.0);
        let left: Vec<bool> = names.iter().map(|n| media.0.join(n).exists()).collect();
        assert_eq!(left, [false, true, true, true]);
    }

    #[test]
    fn nothing_is_stored_without_a_media_folder() {
        let png = noise_png(200, 11);
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture(&png, RasterKind::Image));
        let mut slides = vec![Some(c)];
        let mut store = PictureStore::new(None);
        assert!(store.tick(&mut slides, 0).is_empty());
        assert!(store.known.is_empty() && !store.busy());
    }
}
