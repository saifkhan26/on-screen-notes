//! Pixels for the pictures on a slide: stored pictures (`PictureRef`,
//! files in the media folder, as PureRef boards bring in) and pictures
//! kept in the slide itself (pasted pictures, fills, video posters).
//!
//! A slide can hold hundreds of large pictures. Decoding them all on the
//! UI thread and uploading each at full size stalled the app for seconds
//! on going to such a slide, and filled the GPU, so instead:
//!   * worker threads decode them and scale them to about the size they
//!     are shown at (the next power of two up), newest request first,
//!     so what is on screen comes first; a stored picture's small
//!     preview comes before its file;
//!   * the first new picture or two of a frame are still decoded at
//!     once, so one that an edit just made (a fill, a paste, an erase)
//!     shows without a blank frame;
//!   * zooming in asks for a bigger copy; pictures off screen or on a
//!     hidden layer ask for nothing;
//!   * a texture not drawn for a while is dropped, and when the total
//!     grows past a budget, oversized ones are swapped for smaller.

use crate::canvas::shape::PictureRef;
use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Smallest copy asked for, so a tiny on-screen picture does not reload
/// every time it grows by a few pixels.
const MIN_SIDE: u32 = 128;
/// How long a stored picture may go undrawn before its full-size
/// texture goes; its preview stays. Time, not frames: the frame rate
/// swings from ~90 to several hundred, and flipping between two slides
/// should not reload their pictures.
const DROP_AFTER: Duration = Duration::from_secs(3);
/// How long before a stored picture's preview and bookkeeping go too,
/// and before a picture kept in a slide (which has no preview to fall
/// back on) gives up its texture.
const FORGET_AFTER: Duration = Duration::from_secs(60);
/// Frames count too for a picture kept in a slide, so hiding the overlay
/// for a while (no frames) does not throw away the active slide's.
const FORGET_FRAMES: u64 = 600;
/// Texture memory above which oversized copies are replaced by smaller.
const BUDGET_BYTES: usize = 768 << 20;
/// Most pixel bytes uploaded per frame, to keep frames even.
const UPLOAD_PER_FRAME: usize = 48 << 20;
/// New pictures per frame decoded on the UI thread rather than by the
/// workers: an edit makes one; going to a slide of pictures makes them
/// all at once, and those go to the workers.
const DECODE_NOW_PICTURES: usize = 2;
/// Largest PNG decoded on the UI thread (a big screenshot).
const DECODE_NOW_BYTES: usize = 16 << 20;

/// Where a picture's pixels come from.
#[derive(Clone, Debug)]
pub enum Source {
    /// A file in the media folder.
    File(PictureRef),
    /// PNG bytes kept in the slide. Known by the buffer they live in,
    /// which copies of a shape (undo, paste, pins) share.
    Inline(Arc<Vec<u8>>),
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Source::File(a), Source::File(b)) => a == b,
            (Source::Inline(a), Source::Inline(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Source {}

impl Hash for Source {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Source::File(p) => p.hash(h),
            Source::Inline(png) => (Arc::as_ptr(png) as usize).hash(h),
        }
    }
}

impl Source {
    fn label(&self) -> String {
        match self {
            Source::File(p) => p.file.clone(),
            Source::Inline(png) => format!("inline_{:x}", Arc::as_ptr(png) as usize),
        }
    }
}

struct Entry {
    /// Texture and the long side it was asked for.
    tex: Option<(u32, TextureHandle)>,
    preview: Option<TextureHandle>,
    /// Long side last asked for (0 = nothing in flight).
    pending: u32,
    /// The preview is being decoded.
    preview_pending: bool,
    /// Long side the picture is shown at now.
    want: u32,
    /// Long side of the whole picture, as far as known.
    full: u32,
    /// Frame it was last drawn in, and when; whether it was on screen.
    seen: u64,
    seen_at: Instant,
    on_screen: bool,
    /// Frame the counted window (the overlay) last drew it; `None` when
    /// only another window has. See `PictureCache::count_shown`.
    shown_at: Option<u64>,
    /// The picture could not be read; the preview, if any, stays.
    broken: bool,
}

impl Entry {
    fn showable(&self) -> bool {
        self.tex.is_some() || self.preview.is_some()
    }
}

struct Job {
    source: Source,
    /// Long side to load at; 0 for a stored picture's preview.
    side: u32,
    /// For a preview: its bytes.
    preview: Option<Arc<Vec<u8>>>,
    /// Frame it was asked for; newest first, previews before files.
    asked: u64,
}

struct Loaded {
    source: Source,
    side: u32,
    /// Long side of the whole picture, before scaling.
    full: u32,
    image: Option<ColorImage>,
}

#[derive(Default)]
struct Queue {
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
}

pub struct PictureCache {
    /// Two crops of one file are two textures.
    entries: HashMap<Source, Entry>,
    queue: Arc<Queue>,
    done_tx: Sender<Loaded>,
    done_rx: Receiver<Loaded>,
    /// Finished copies not uploaded yet (the per-frame upload budget ran
    /// out): one per picture, the biggest. Side 0 is a preview.
    ready: HashMap<Source, (u32, ColorImage)>,
    workers_started: bool,
    tick: u64,
    /// Pictures first seen this frame.
    fresh: usize,
    /// Whether `texture` calls count towards `shown` (the overlay's
    /// "loading pictures" progress). Off while the slide window asks for
    /// its pictures, so that progress is the canvas's own.
    counting: bool,
}

impl Default for PictureCache {
    fn default() -> Self {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        Self {
            entries: HashMap::new(),
            queue: Arc::default(),
            done_tx,
            done_rx,
            ready: HashMap::new(),
            workers_started: false,
            tick: 0,
            fresh: 0,
            counting: true,
        }
    }
}

/// Long side to load a picture at to show it `shown` physical pixels
/// long: the next power of two up, within [`MIN_SIDE`, its real size].
fn side_for(shown: f32, full: u32) -> u32 {
    let need = shown.max(1.0).min(u32::MAX as f32).ceil() as u32;
    need.max(MIN_SIDE).checked_next_power_of_two().unwrap_or(u32::MAX).min(full.max(1))
}

/// Threads decoding pictures: half the cores, two to six.
fn workers() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 6))
}

impl PictureCache {
    /// Once per frame, before any `texture` call: let go of what has not
    /// been drawn lately, then upload what the workers finished.
    ///
    /// Nothing made here may be freed in the same frame. egui-wgpu writes
    /// a new texture's pixels through the GPU queue and destroys freed
    /// textures before it submits the frame, so a texture made and freed
    /// in one frame fails the submit, which aborts the app. Flipping
    /// between two slides of pictures did exactly that: a copy finished
    /// for a picture that had just gone idle was made, then freed by the
    /// eviction below. So eviction runs first, each picture gets at most
    /// one new copy a frame, and only while it is still being drawn.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.tick += 1;
        let tick = self.tick;
        let now = Instant::now();
        self.fresh = 0;

        // Take in finished work, keeping the biggest copy per picture.
        while let Ok(done) = self.done_rx.try_recv() {
            let Some(e) = self.entries.get_mut(&done.source) else { continue };
            if done.side == 0 {
                e.preview_pending = false;
            } else {
                if e.pending <= done.side {
                    e.pending = 0;
                }
                if done.full > 0 {
                    e.full = done.full;
                }
            }
            match done.image {
                // A preview (side 0) never pushes out a finished copy.
                Some(img) => {
                    if self.ready.get(&done.source).map_or(true, |(side, _)| *side < done.side) {
                        self.ready.insert(done.source, (done.side, img));
                    }
                }
                None if done.side > 0 => e.broken = true,
                None => {}
            }
        }

        // Undrawn for a while: free the texture; for longer: forget it.
        // A picture kept in a slide has no preview to show meanwhile, so
        // it keeps its texture as long as it is remembered at all.
        self.entries.retain(|source, e| {
            let idle = now.saturating_duration_since(e.seen_at);
            match source {
                Source::File(_) => {
                    if idle > DROP_AFTER {
                        e.tex = None;
                        e.pending = 0;
                    }
                    idle <= FORGET_AFTER
                }
                Source::Inline(_) => idle <= FORGET_AFTER || tick.saturating_sub(e.seen) <= FORGET_FRAMES,
            }
        });
        // Over budget: copies much bigger than they are shown at go back
        // to the size they are shown at.
        let bytes: usize = self.entries.values().filter_map(|e| e.tex.as_ref()).map(|(_, t)| t.byte_size()).sum();
        if bytes > BUDGET_BYTES {
            for e in self.entries.values_mut() {
                if e.tex.as_ref().is_some_and(|(s, _)| *s > e.want.saturating_mul(2)) {
                    e.tex = None;
                    e.pending = 0;
                }
            }
        }

        // Upload, within the per-frame budget. A copy for a picture not
        // drawn last frame is let go; it is asked for again if the
        // picture comes back.
        let mut budget = UPLOAD_PER_FRAME;
        for (source, (side, img)) in std::mem::take(&mut self.ready) {
            let Some(e) = self.entries.get_mut(&source) else { continue };
            if tick.saturating_sub(e.seen) > 2 {
                continue;
            }
            if budget == 0 {
                self.ready.insert(source, (side, img));
                continue;
            }
            if side == 0 {
                if e.preview.is_none() {
                    budget = budget.saturating_sub(img.pixels.len() * 4);
                    e.preview = Some(ctx.load_texture(format!("osn_picture_preview_{}", source.label()), img, TextureOptions::LINEAR));
                }
            } else if e.tex.as_ref().map_or(true, |(s, _)| *s < side) {
                // A late, smaller copy never replaces a bigger one.
                budget = budget.saturating_sub(img.pixels.len() * 4);
                e.tex = Some((side, ctx.load_texture(format!("osn_picture_{}", source.label()), img, TextureOptions::LINEAR)));
            }
        }

        // Requests for pictures no longer on screen are dropped unstarted;
        // they are asked for again if the picture comes back.
        let mut dropped = Vec::new();
        {
            let entries = &self.entries;
            let mut jobs = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            jobs.retain(|j| {
                let keep = entries.get(&j.source).is_some_and(|e| {
                    tick.saturating_sub(e.seen) <= 2 && if j.side == 0 { e.preview_pending } else { e.pending == j.side }
                });
                if !keep {
                    dropped.push((j.source.clone(), j.side));
                }
                keep
            });
        }
        for (source, side) in dropped {
            if let Some(e) = self.entries.get_mut(&source) {
                if side == 0 {
                    e.preview_pending = false;
                } else {
                    e.pending = 0;
                }
            }
        }
    }

    /// The texture to draw a picture with: the best copy loaded so far,
    /// else a stored picture's `preview` (its small PNG). `shown` is its
    /// long side on screen in physical pixels and `full` its long side
    /// in pixels (0 if unknown); `visible` is false when it is off
    /// screen, which keeps what is loaded but asks for nothing new.
    pub fn texture(
        &mut self,
        ctx: &egui::Context,
        source: &Source,
        preview: Option<&Arc<Vec<u8>>>,
        shown: f32,
        full: u32,
        visible: bool,
    ) -> Option<TextureId> {
        let tick = self.tick;
        let fresh = !self.entries.contains_key(source);
        if fresh {
            self.fresh += 1;
            let full = match source {
                Source::File(p) => p.size[0].max(p.size[1]),
                Source::Inline(_) if full > 0 => full,
                Source::Inline(_) => u32::MAX,
            };
            self.entries.insert(
                source.clone(),
                Entry {
                    tex: None,
                    preview: None,
                    pending: 0,
                    preview_pending: false,
                    want: 0,
                    full,
                    seen: tick,
                    seen_at: Instant::now(),
                    on_screen: false,
                    shown_at: None,
                    broken: false,
                },
            );
        }
        let counting = self.counting;
        let e = self.entries.get_mut(source).expect("just made");
        e.seen = tick;
        e.seen_at = Instant::now();
        if counting {
            e.on_screen = visible;
            e.shown_at = Some(tick);
        }
        let want = side_for(shown, e.full);
        e.want = want;

        // One of the first new pictures this frame: decode it here, so
        // a picture an edit just made shows at once.
        let png = match (source, preview) {
            (Source::Inline(png), _) => Some((png.len(), false)),
            (Source::File(_), Some(p)) => Some((p.len(), true)),
            _ => None,
        };
        if let Some((len, is_preview)) = png {
            if fresh && visible && self.fresh <= DECODE_NOW_PICTURES && len <= DECODE_NOW_BYTES {
                let (img, real) = if is_preview { (decode_png(preview.expect("checked"), u32::MAX), 0) } else { decode_source(source, want) };
                match img {
                    Some(img) => {
                        let label = source.label();
                        if is_preview {
                            e.preview = Some(ctx.load_texture(format!("osn_picture_preview_{label}"), img, TextureOptions::LINEAR));
                        } else {
                            e.full = real.max(1);
                            e.tex = Some((want, ctx.load_texture(format!("osn_picture_{label}"), img, TextureOptions::LINEAR)));
                        }
                    }
                    None if !is_preview => e.broken = true,
                    None => {}
                }
            }
        }

        let want = side_for(shown, e.full);
        let have = e.tex.as_ref().map_or(0, |(s, _)| *s);
        let request = (visible && !e.broken && have < want && e.pending < want).then_some(want);
        let ask_preview = visible && e.preview.is_none() && !e.preview_pending && e.tex.is_none() && preview.is_some();
        let id = e.tex.as_ref().map(|(_, t)| t.id()).or_else(|| e.preview.as_ref().map(|t| t.id()));
        if ask_preview {
            e.preview_pending = true;
            self.ask(Job { source: source.clone(), side: 0, preview: preview.cloned(), asked: tick + 1 });
        }
        if let Some(side) = request {
            let e = self.entries.get_mut(source).expect("still here");
            e.pending = side;
            self.ask(Job { source: source.clone(), side, preview: None, asked: tick });
        }
        id
    }

    /// A picture kept in a slide is now stored as `to`
    /// (`canvas::picture_store`): what was loaded for it carries over,
    /// rather than showing its preview while the file loads. Call before
    /// `begin_frame`, so the texture it may free was not made this frame.
    pub fn carry(&mut self, from: &Arc<Vec<u8>>, to: &PictureRef) {
        let (old, new) = (Source::Inline(Arc::clone(from)), Source::File(to.clone()));
        let Some(mut e) = self.entries.remove(&old) else { return };
        let ready = self.ready.remove(&old);
        if self.entries.contains_key(&new) {
            return;
        }
        // Loads asked for under the old name are dropped when they land.
        e.pending = 0;
        e.full = to.size[0].max(to.size[1]);
        if let Some(r) = ready {
            self.ready.insert(new.clone(), r);
        }
        self.entries.insert(new, e);
    }

    /// Count the `texture` calls that follow towards `shown`, or not (a
    /// second window's pictures). On unless turned off.
    pub fn count_shown(&mut self, on: bool) {
        self.counting = on;
    }

    /// Pictures are still being loaded or uploaded, so frames should keep
    /// coming for them to sharpen.
    pub fn busy(&self) -> bool {
        !self.ready.is_empty() || self.entries.values().any(|e| e.pending > 0 || e.preview_pending)
    }

    /// Of the pictures on screen last frame, how many have something to
    /// show, and how many there are.
    pub fn shown(&self) -> (usize, usize) {
        let on = self
            .entries
            .values()
            .filter(|e| e.on_screen && e.shown_at.is_some_and(|t| self.tick.saturating_sub(t) <= 1));
        on.fold((0, 0), |(ready, all), e| (ready + usize::from(e.showable() || e.broken), all + 1))
    }

    /// For the perf log: pictures known, loads in flight, copies waiting
    /// to upload, broken files, and MB of textures.
    pub fn stats(&self) -> String {
        let pending = self.entries.values().filter(|e| e.pending > 0).count();
        let broken = self.entries.values().filter(|e| e.broken).count();
        let bytes: usize = self.entries.values().filter_map(|e| e.tex.as_ref()).map(|(_, t)| t.byte_size()).sum();
        let (ready, on_screen) = self.shown();
        format!(
            "pics {} on screen {ready}/{on_screen} pending {pending} ready {} broken {broken} tex {} MB",
            self.entries.len(),
            self.ready.len(),
            bytes >> 20
        )
    }

    fn ask(&mut self, job: Job) {
        if !self.workers_started {
            self.workers_started = true;
            for i in 0..workers() {
                let queue = Arc::clone(&self.queue);
                let done = self.done_tx.clone();
                let spawned = std::thread::Builder::new().name(format!("osn-picture-{i}")).spawn(move || worker(&queue, &done));
                if let Err(e) = spawned {
                    log::warn!("could not start a picture loader: {e}");
                }
            }
        }
        let mut jobs = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
        match jobs.iter_mut().find(|j| j.source == job.source && (j.side == 0) == (job.side == 0)) {
            Some(j) => {
                j.side = job.side;
                j.asked = job.asked;
            }
            None => jobs.push(job),
        }
        drop(jobs);
        self.queue.ready.notify_one();
    }
}

fn worker(queue: &Queue, done: &Sender<Loaded>) {
    loop {
        let job = {
            let mut jobs = queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                // Newest request first: that is what is on screen now.
                if let Some(i) = (0..jobs.len()).max_by_key(|&i| jobs[i].asked) {
                    break jobs.swap_remove(i);
                }
                jobs = queue.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
            }
        };
        let (image, full) = match &job.preview {
            Some(png) => (decode_png(png, u32::MAX), 0),
            None => decode_source(&job.source, job.side),
        };
        if done.send(Loaded { source: job.source, side: job.side, full, image }).is_err() {
            return;
        }
    }
}

/// Decode a picture so its long side is at most `side`, and say how long
/// its long side really is.
fn decode_source(source: &Source, side: u32) -> (Option<ColorImage>, u32) {
    match source {
        Source::File(picture) => match load_file(picture) {
            Some(img) => {
                let full = img.width().max(img.height());
                (Some(color_image(picture.view(&img, side))), full)
            }
            None => (None, 0),
        },
        Source::Inline(png) => match image::load_from_memory(png) {
            Ok(img) => {
                let img = img.to_rgba8();
                let full = img.width().max(img.height());
                (Some(color_image(fit(img, side))), full)
            }
            Err(e) => {
                log::warn!("decode raster shape failed: {e}");
                (None, 0)
            }
        },
    }
}

fn decode_png(png: &[u8], side: u32) -> Option<ColorImage> {
    Some(color_image(fit(image::load_from_memory(png).ok()?.to_rgba8(), side)))
}

/// `img` scaled so its long side is at most `side` (never up).
fn fit(img: image::RgbaImage, side: u32) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= side || long == 0 {
        return img;
    }
    let s = f64::from(side) / f64::from(long);
    let (tw, th) = (((f64::from(w) * s).round() as u32).max(1), ((f64::from(h) * s).round() as u32).max(1));
    image::imageops::thumbnail(&img, tw, th)
}

fn color_image(img: image::RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

/// Decode a stored picture's file.
fn load_file(picture: &PictureRef) -> Option<image::RgbaImage> {
    let path = crate::persistence::paths::media_dir().ok()?.join(&picture.file);
    let decoded = image::ImageReader::open(&path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())
        .and_then(|r| r.decode().map_err(|e| e.to_string()));
    match decoded {
        Ok(img) => Some(img.to_rgba8()),
        Err(e) => {
            log::warn!("could not load picture {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, run: impl FnMut(&egui::Context)) -> egui::TexturesDelta {
        let mut d = ctx.run(egui::RawInput::default(), run).textures_delta;
        // The font atlas is egui's own.
        d.set.retain(|(id, _)| *id != TextureId::default());
        d
    }

    fn copy(side: usize) -> Option<ColorImage> {
        Some(ColorImage::new([side, side], egui::Color32::RED))
    }

    fn png(w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut out = Vec::new();
        image::RgbaImage::new(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        Arc::new(out)
    }

    /// egui-wgpu fails the frame (and the app aborts) when a texture is
    /// made and freed in the same frame; flipping between two slides of
    /// pictures used to do that.
    #[test]
    fn no_texture_is_made_and_freed_in_one_frame() {
        let ctx = egui::Context::default();
        let mut cache = PictureCache::default();
        let p = Source::File(PictureRef { file: "x.png".into(), size: [4000, 3000], crop: None, mirror: false });
        let preview = png(4, 3);
        // `visible: false` keeps the loader threads out of it; the copies
        // below stand in for what they would deliver.
        let made_and_freed = |d: &egui::TexturesDelta| d.free.iter().any(|f| d.set.iter().any(|(s, _)| s == f));

        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &p, Some(&preview), 200.0, 0, false);
        });
        assert_eq!((d.set.len(), d.free.len()), (0, 0), "nothing asked for off screen");
        cache.done_tx.send(Loaded { source: p.clone(), side: 0, full: 0, image: copy(4) }).unwrap();
        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &p, Some(&preview), 200.0, 0, false);
        });
        assert_eq!((d.set.len(), d.free.len()), (1, 0), "the preview");

        // Two copies of the picture finish in the same frame.
        cache.done_tx.send(Loaded { source: p.clone(), side: 128, full: 4000, image: copy(128) }).unwrap();
        cache.done_tx.send(Loaded { source: p.clone(), side: 256, full: 4000, image: copy(256) }).unwrap();
        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &p, Some(&preview), 200.0, 0, false);
        });
        assert_eq!((d.set.len(), d.free.len()), (1, 0), "one copy, the bigger");
        assert_eq!(cache.entries[&p].tex.as_ref().map(|(s, _)| *s), Some(256));

        // It goes off screen past `DROP_AFTER` while a bigger copy
        // finishes: the old copy goes, the new one is not made.
        let e = cache.entries.get_mut(&p).unwrap();
        e.seen_at -= DROP_AFTER * 2;
        e.seen = cache.tick.saturating_sub(10);
        cache.done_tx.send(Loaded { source: p.clone(), side: 512, full: 4000, image: copy(512) }).unwrap();
        let d = frame(&ctx, |ctx| cache.begin_frame(ctx));
        assert!(!made_and_freed(&d));
        assert_eq!((d.set.len(), d.free.len()), (0, 1));
    }

    #[test]
    fn the_first_new_pictures_decode_at_once_the_rest_in_the_background() {
        let ctx = egui::Context::default();
        let mut cache = PictureCache::default();
        let big: Vec<Source> = (0..6).map(|_| Source::Inline(Arc::new(vec![0; 1000]))).collect();
        let small = Source::Inline(png(300, 200));
        let _ = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            assert!(cache.texture(ctx, &small, None, 300.0, 300, true).is_some(), "a fresh small picture shows at once");
            for s in &big {
                cache.texture(ctx, s, None, 300.0, 0, true);
            }
        });
        // The first of the others was tried here too (and is no PNG); the
        // rest went to the workers.
        let tried_here = big.iter().filter(|s| cache.entries[*s].broken).count();
        assert_eq!(tried_here, 1);
        assert_eq!(cache.shown(), (2, 7), "the small one and the broken one count as done");
        assert!(cache.busy());
    }

    #[test]
    fn pictures_in_a_slide_go_only_after_frames_and_time_both_pass() {
        let ctx = egui::Context::default();
        let mut cache = PictureCache::default();
        let (a, b) = (Source::Inline(png(2, 2)), Source::Inline(png(2, 2)));
        let _ = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &a, None, 2.0, 2, true);
            cache.texture(ctx, &b, None, 2.0, 2, true);
        });
        assert!(cache.entries[&a].tex.is_some(), "small enough to decode at once");
        // Both many frames back; `a` long ago too, `b` recently (the
        // overlay was hidden meanwhile).
        cache.tick += FORGET_FRAMES * 2;
        cache.entries.get_mut(&a).unwrap().seen_at = Instant::now().checked_sub(FORGET_AFTER * 2).expect("clock far enough along");
        let _ = frame(&ctx, |ctx| cache.begin_frame(ctx));
        assert!(!cache.entries.contains_key(&a), "idle slide's picture is let go");
        assert!(cache.entries[&b].tex.is_some(), "recently drawn picture stays");
    }

    /// The slide window shares this cache; the overlay's "loading
    /// pictures" progress must count only what the overlay draws.
    #[test]
    fn a_second_windows_pictures_stay_out_of_the_progress() {
        let ctx = egui::Context::default();
        let mut cache = PictureCache::default();
        let (mine, theirs) = (Source::Inline(png(2, 2)), Source::Inline(png(2, 2)));
        let draw = |cache: &mut PictureCache, overlay_too: bool| {
            let _ = frame(&ctx, |ctx| {
                cache.begin_frame(ctx);
                cache.texture(ctx, &mine, None, 2.0, 2, true);
                if overlay_too {
                    cache.texture(ctx, &theirs, None, 2.0, 2, true);
                }
                cache.count_shown(false);
                cache.texture(ctx, &theirs, None, 2.0, 2, true);
                cache.count_shown(true);
            });
        };
        draw(&mut cache, false);
        assert_eq!(cache.shown(), (1, 1), "only the overlay's picture");
        draw(&mut cache, true);
        assert_eq!(cache.shown(), (2, 2), "both windows show it now");
        draw(&mut cache, false);
        draw(&mut cache, false);
        assert_eq!(cache.shown(), (1, 1), "the overlay moved on; the other window still draws it");
    }

    #[test]
    fn pictures_in_a_slide_are_known_by_their_buffer() {
        let a = png(2, 2);
        let same = Source::Inline(Arc::clone(&a));
        assert_eq!(Source::Inline(a), same);
        assert_ne!(Source::Inline(png(2, 2)), same, "equal bytes, another picture");
    }

    #[test]
    fn sizes_step_in_powers_of_two_within_the_picture() {
        assert_eq!(side_for(10.0, 4000), MIN_SIDE);
        assert_eq!(side_for(300.0, 4000), 512);
        assert_eq!(side_for(512.0, 4000), 512);
        assert_eq!(side_for(513.0, 4000), 1024);
        assert_eq!(side_for(9000.0, 4000), 4000, "never more than the file has");
        assert_eq!(side_for(300.0, 100), 100);
    }

    #[test]
    fn a_picture_moved_to_a_file_keeps_its_texture() {
        let ctx = egui::Context::default();
        let mut cache = PictureCache::default();
        let bytes = png(8, 8);
        let inline = Source::Inline(Arc::clone(&bytes));
        // Small, new and on screen: decoded at once, no loader threads.
        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &inline, None, 8.0, 8, true);
        });
        assert_eq!(d.set.len(), 1);
        let id = cache.entries[&inline].tex.as_ref().map(|(_, t)| t.id());
        assert!(id.is_some());

        let to = PictureRef { file: "moved.png".into(), size: [8, 8], crop: None, mirror: false };
        cache.carry(&bytes, &to);
        let preview = png(2, 2);
        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            assert_eq!(cache.texture(ctx, &Source::File(to.clone()), Some(&preview), 8.0, 0, true), id);
        });
        assert!(d.set.is_empty() && d.free.is_empty(), "nothing loaded again");
        assert!(!cache.entries.contains_key(&inline));
        assert!(!cache.busy());
    }
}
