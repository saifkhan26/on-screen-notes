//! Full-size pixels for stored pictures (`PictureRef`: pictures whose
//! file lives in the media folder, as PureRef boards bring in).
//!
//! A board can put hundreds of large pictures on one slide. Decoding them
//! all on the UI thread and uploading each at full size would stall the
//! app for seconds and fill the GPU, so instead:
//!   * the slide's small preview shows at once;
//!   * worker threads decode the file and scale it to about the size the
//!     picture is shown at (the next power of two up), newest request
//!     first, so what is on screen sharpens first;
//!   * zooming in asks for a bigger copy; pictures off screen or on a
//!     hidden layer ask for nothing;
//!   * a texture not drawn for a few seconds is dropped, and when the
//!     total grows past a budget, oversized ones are swapped for smaller.

use crate::canvas::shape::PictureRef;
use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Smallest copy asked for, so a tiny on-screen picture does not reload
/// every time it grows by a few pixels.
const MIN_SIDE: u32 = 128;
/// How long a picture may go undrawn before its full-size texture goes.
/// Time, not frames: the frame rate swings from ~90 to several hundred,
/// and flipping between two slides should not reload their pictures.
const DROP_AFTER: Duration = Duration::from_secs(3);
/// How long before its preview and bookkeeping go too.
const FORGET_AFTER: Duration = Duration::from_secs(60);
/// Texture memory above which oversized copies are replaced by smaller.
const BUDGET_BYTES: usize = 768 << 20;
/// Most pixel bytes uploaded per frame, to keep frames even.
const UPLOAD_PER_FRAME: usize = 48 << 20;
const WORKERS: usize = 2;

struct Entry {
    /// Full-size texture and the long side it was made at.
    tex: Option<(u32, TextureHandle)>,
    preview: Option<TextureHandle>,
    /// Long side last asked for (0 = nothing in flight).
    pending: u32,
    /// Long side the picture is shown at now.
    want: u32,
    /// Frame it was last drawn in, and when.
    seen: u64,
    seen_at: Instant,
    /// The file could not be read; the preview stays.
    broken: bool,
}

struct Job {
    picture: PictureRef,
    side: u32,
    /// Frame it was asked for; newest first.
    asked: u64,
}

struct Loaded {
    picture: PictureRef,
    side: u32,
    image: Option<ColorImage>,
}

#[derive(Default)]
struct Queue {
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
}

pub struct PictureCache {
    /// Keyed by the whole reference: two crops of one file are two
    /// textures.
    entries: HashMap<PictureRef, Entry>,
    queue: Arc<Queue>,
    done_tx: Sender<Loaded>,
    done_rx: Receiver<Loaded>,
    /// Finished copies not uploaded yet (the per-frame upload budget ran
    /// out): one per picture, the biggest.
    ready: HashMap<PictureRef, (u32, ColorImage)>,
    workers_started: bool,
    tick: u64,
}

impl Default for PictureCache {
    fn default() -> Self {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        Self { entries: HashMap::new(), queue: Arc::default(), done_tx, done_rx, ready: HashMap::new(), workers_started: false, tick: 0 }
    }
}

/// Long side to load a picture at to show it `shown` physical pixels
/// long: the next power of two up, within [`MIN_SIDE`, its real size].
fn side_for(shown: f32, full: u32) -> u32 {
    let need = shown.max(1.0).min(u32::MAX as f32).ceil() as u32;
    need.max(MIN_SIDE).checked_next_power_of_two().unwrap_or(u32::MAX).min(full.max(1))
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

        // Take in finished work, keeping the biggest copy per picture.
        while let Ok(done) = self.done_rx.try_recv() {
            let Some(e) = self.entries.get_mut(&done.picture) else { continue };
            if e.pending <= done.side {
                e.pending = 0;
            }
            match done.image {
                Some(img) => {
                    if self.ready.get(&done.picture).map_or(true, |(side, _)| *side < done.side) {
                        self.ready.insert(done.picture, (done.side, img));
                    }
                }
                None => e.broken = true,
            }
        }

        // Undrawn for a while: free the GPU copy; for longer: forget it.
        self.entries.retain(|_, e| {
            let idle = now.saturating_duration_since(e.seen_at);
            if idle > DROP_AFTER {
                e.tex = None;
                e.pending = 0;
            }
            idle <= FORGET_AFTER
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
        for (picture, (side, img)) in std::mem::take(&mut self.ready) {
            let Some(e) = self.entries.get_mut(&picture) else { continue };
            if tick.saturating_sub(e.seen) > 2 {
                continue;
            }
            if budget == 0 {
                self.ready.insert(picture, (side, img));
                continue;
            }
            // A late, smaller copy never replaces a bigger one.
            if e.tex.as_ref().map_or(true, |(s, _)| *s < side) {
                budget = budget.saturating_sub(img.pixels.len() * 4);
                let handle = ctx.load_texture(format!("osn_picture_{}", picture.file), img, TextureOptions::LINEAR);
                e.tex = Some((side, handle));
            }
        }

        // Requests for pictures no longer on screen are dropped unstarted;
        // they are asked for again if the picture comes back.
        let mut dropped = Vec::new();
        {
            let entries = &self.entries;
            let mut jobs = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            jobs.retain(|j| {
                let keep = entries.get(&j.picture).is_some_and(|e| tick.saturating_sub(e.seen) <= 2 && e.pending == j.side);
                if !keep {
                    dropped.push(j.picture.clone());
                }
                keep
            });
        }
        for picture in dropped {
            if let Some(e) = self.entries.get_mut(&picture) {
                e.pending = 0;
            }
        }
    }

    /// The texture to draw a stored picture with: the best copy loaded so
    /// far, else its preview. `shown` is its long side on screen in
    /// physical pixels; `visible` is false when it is off screen, which
    /// keeps what is loaded but asks for nothing new.
    pub fn texture(&mut self, ctx: &egui::Context, picture: &PictureRef, preview: &[u8], shown: f32, visible: bool) -> Option<TextureId> {
        let tick = self.tick;
        let e = self.entries.entry(picture.clone()).or_insert_with(|| Entry {
            tex: None,
            preview: None,
            pending: 0,
            want: 0,
            seen: tick,
            seen_at: Instant::now(),
            broken: false,
        });
        e.seen = tick;
        e.seen_at = Instant::now();
        let full = picture.size[0].max(picture.size[1]);
        let want = side_for(shown, full);
        e.want = want;
        let have = e.tex.as_ref().map_or(0, |(s, _)| *s);
        let request = (visible && !e.broken && have < want && e.pending < want).then_some(want);
        if e.preview.is_none() {
            e.preview = decode_preview(ctx, &picture.file, preview);
        }
        let id = e.tex.as_ref().map(|(_, t)| t.id()).or_else(|| e.preview.as_ref().map(|t| t.id()));
        if let Some(side) = request {
            e.pending = side;
            self.ask(picture, side);
        }
        id
    }

    /// Pictures are still being loaded or uploaded, so frames should keep
    /// coming for them to sharpen.
    pub fn busy(&self) -> bool {
        !self.ready.is_empty() || self.entries.values().any(|e| e.pending > 0)
    }

    fn ask(&mut self, picture: &PictureRef, side: u32) {
        if !self.workers_started {
            self.workers_started = true;
            for i in 0..WORKERS {
                let queue = Arc::clone(&self.queue);
                let done = self.done_tx.clone();
                let spawned = std::thread::Builder::new().name(format!("osn-picture-{i}")).spawn(move || worker(&queue, &done));
                if let Err(e) = spawned {
                    log::warn!("could not start a picture loader: {e}");
                }
            }
        }
        let mut jobs = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
        match jobs.iter_mut().find(|j| j.picture == *picture) {
            Some(j) => {
                j.side = side;
                j.asked = self.tick;
            }
            None => jobs.push(Job { picture: picture.clone(), side, asked: self.tick }),
        }
        drop(jobs);
        self.queue.ready.notify_one();
    }
}

fn decode_preview(ctx: &egui::Context, file: &str, preview: &[u8]) -> Option<TextureHandle> {
    let img = image::load_from_memory(preview).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(format!("osn_picture_preview_{file}"), color, TextureOptions::LINEAR))
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
        let image = load(&job.picture, job.side);
        if done.send(Loaded { picture: job.picture, side: job.side, image }).is_err() {
            return;
        }
    }
}

/// Decode a stored picture, cut out what the shape shows, and scale it
/// so its long side is at most `side`.
fn load(picture: &PictureRef, side: u32) -> Option<ColorImage> {
    let path = crate::persistence::paths::media_dir().ok()?.join(&picture.file);
    let decoded = image::ImageReader::open(&path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())
        .and_then(|r| r.decode().map_err(|e| e.to_string()));
    let img = match decoded {
        Ok(img) => img.to_rgba8(),
        Err(e) => {
            log::warn!("could not load picture {}: {e}", path.display());
            return None;
        }
    };
    let img = picture.view(&img, side);
    Some(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
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

    /// egui-wgpu fails the frame (and the app aborts) when a texture is
    /// made and freed in the same frame; flipping between two slides of
    /// pictures used to do that.
    #[test]
    fn no_texture_is_made_and_freed_in_one_frame() {
        let ctx = egui::Context::default();
        let mut cache = PictureCache::default();
        let p = PictureRef { file: "x.png".into(), size: [4000, 3000], crop: None, mirror: false };
        let mut preview = Vec::new();
        image::RgbaImage::new(4, 3).write_to(&mut std::io::Cursor::new(&mut preview), image::ImageFormat::Png).unwrap();
        // `visible: false` keeps the loader threads out of it; the copies
        // below stand in for what they would deliver.
        let made_and_freed = |d: &egui::TexturesDelta| d.free.iter().any(|f| d.set.iter().any(|(s, _)| s == f));

        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &p, &preview, 200.0, false);
        });
        assert_eq!((d.set.len(), d.free.len()), (1, 0), "the preview");

        // Two copies of the picture finish in the same frame.
        cache.done_tx.send(Loaded { picture: p.clone(), side: 128, image: copy(128) }).unwrap();
        cache.done_tx.send(Loaded { picture: p.clone(), side: 256, image: copy(256) }).unwrap();
        let d = frame(&ctx, |ctx| {
            cache.begin_frame(ctx);
            cache.texture(ctx, &p, &preview, 200.0, false);
        });
        assert_eq!((d.set.len(), d.free.len()), (1, 0), "one copy, the bigger");
        assert_eq!(cache.entries[&p].tex.as_ref().map(|(s, _)| *s), Some(256));

        // It goes off screen past `DROP_AFTER` while a bigger copy
        // finishes: the old copy goes, the new one is not made.
        let e = cache.entries.get_mut(&p).unwrap();
        e.seen_at -= DROP_AFTER * 2;
        e.seen = cache.tick.saturating_sub(10);
        cache.done_tx.send(Loaded { picture: p.clone(), side: 512, image: copy(512) }).unwrap();
        let d = frame(&ctx, |ctx| cache.begin_frame(ctx));
        assert!(!made_and_freed(&d));
        assert_eq!((d.set.len(), d.free.len()), (0, 1));
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
}
