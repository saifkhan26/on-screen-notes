//! Canvas subsystem: data, manager, rasteriser.
//!
//! Sub-modules:
//!   * [`stroke`] — raw pen samples and stroke records.
//!   * [`shape`] — rectangle / ellipse / line / arrow records.
//!   * [`canvas`] — one drawable surface holding strokes + shapes + transform.
//!   * [`render`] — tiny-skia rasteriser turning a `Canvas` into pixels.
//!
//! [`CanvasManager`] holds the user's collection of canvases and tracks
//! which one is "active" (visible + accepting input).

pub mod affine;
pub mod canvas;
pub mod hit;
pub mod ink_gpu;
pub mod render;
pub mod shape;
pub mod smoothing;
pub mod stroke;

use crate::canvas::canvas::Canvas;
use crate::config::AppConfig;
use crate::persistence::SlideView;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

/// Top-level holder for all canvases.
pub struct CanvasManager {
    /// One entry per slide. `None` is a slide whose file the background
    /// loader is still reading: nothing of it is in memory, so nothing
    /// can draw on it, and no save can write over its file. `active`
    /// always points at a `Some`.
    canvases: Vec<Option<Canvas>>,
    active: usize,
    /// One human-readable line per slide that could not be read at
    /// startup. Drained by the app into a toast.
    load_errors: Vec<String>,
    /// Slots whose file was unreadable *and* could not be copied aside.
    /// The save paths never write these indices, so the only copy of
    /// whatever is in the file stays on disk untouched.
    protected: Vec<usize>,
    /// Set by `delete_active`: the files past the new count are stale
    /// and the next save may remove them. Nothing else is allowed to
    /// trigger that cleanup — a short in-memory list on its own proves
    /// nothing about which files are safe to delete.
    prune_orphans: bool,
    /// Video / GIF files that deleted slides used. Each is deleted from
    /// the media folder once no slide, undo history or clipboard uses it.
    media_to_check: HashSet<String>,
    /// Set by `delete_active`: the next save rewrites the slide files
    /// without the deleted slide, and only after that may its media go —
    /// a crash in between must not leave a slide whose videos are gone.
    media_cleanup_due: bool,
    /// Slides still being read after startup; `None` once all are in.
    loading: Option<BackgroundLoad>,
    /// Per slide, its view as `views.ron` last received it (`None`: not
    /// known yet), to tell when the file needs writing again.
    saved_views: Vec<Option<SlideView>>,
    /// The views `views.ron` held at startup, given to each slide as it
    /// is read.
    file_views: Vec<SlideView>,
    /// A background save is writing. Only one runs at a time: two
    /// writing the same slide share its `.tmp` file and could leave a
    /// mix of both renamed over the slide, and on a big slide (a 1 GB
    /// one takes ~15 s) back-to-back edits stacked up several at once.
    saving: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Stand-in for a slot whose file is missing or unreadable. It keeps
/// every later slide at its own index, and starts clean (`dirty =
/// false`) so it is only written to disk if the user actually draws on
/// it.
fn placeholder_canvas() -> Canvas {
    let mut c = Canvas::default();
    c.dirty = false;
    c
}

/// What reading one slot produced, with an unreadable file already
/// copied aside — done before the slot reaches the manager, so no save
/// can get to the file first.
struct SlotLoad {
    /// The slide, or a blank placeholder when the file is missing or
    /// could not be read.
    canvas: Canvas,
    /// One line for the user when the file could not be read.
    error: Option<String>,
    /// The file could not be read *and* could not be copied aside: the
    /// slot must never be written.
    protected: bool,
}

/// Read slot `idx` from disk. A missing file becomes a placeholder; an
/// unreadable one is copied aside and becomes a placeholder.
fn load_slot(idx: usize, passes: usize) -> SlotLoad {
    use crate::persistence::LoadOutcome;
    match crate::persistence::load_canvas(idx, passes) {
        LoadOutcome::Loaded(canvas) => SlotLoad { canvas, error: None, protected: false },
        LoadOutcome::Missing => {
            log::warn!("canvas_{idx}.ron is missing; leaving a blank slide in its place");
            SlotLoad { canvas: placeholder_canvas(), error: None, protected: false }
        }
        LoadOutcome::Unreadable(e) => {
            log::warn!("canvas_{idx}.ron could not be read: {e}");
            let (error, protected) = match crate::persistence::preserve_unreadable(idx) {
                Ok(kept) => (
                    format!(
                        "Slide {} could not be read — original kept as {}",
                        idx + 1,
                        kept.file_name().and_then(|n| n.to_str()).unwrap_or("a copy"),
                    ),
                    false,
                ),
                Err(copy_err) => {
                    log::warn!("could not copy canvas_{idx}.ron aside: {copy_err}");
                    (format!("Slide {} could not be read — its file is left untouched", idx + 1), true)
                }
            };
            SlotLoad { canvas: placeholder_canvas(), error: Some(error), protected }
        }
    }
}

/// The slides read on background threads after startup, so the window
/// opens as soon as the first one is in.
struct BackgroundLoad {
    /// Per slot: someone — a loader thread, or the UI thread when the
    /// user goes to a slide before its turn — has started reading it.
    /// Whoever flips it first reads the file; nobody else touches it.
    claims: Arc<Vec<AtomicBool>>,
    results: mpsc::Receiver<(usize, SlotLoad)>,
    /// Smoothing passes for slides read from now on.
    passes: usize,
    /// Stroke caches were rebuilt (smoothing or pressure curve changed)
    /// after the threads started; slides they deliver are rebuilt too.
    caches_stale: bool,
    started: Instant,
}

/// How many threads read slides in the background: half the cores, at
/// most four, so the UI keeps its share.
fn background_workers() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4))
}

/// Slots `1..=max` in the order the loader threads take them: the
/// slides the arrow keys reach from slide 1 (the next one, and the last
/// one `prev` wraps to) first, then the rest in order.
fn load_order(max: usize) -> Vec<usize> {
    let mut order = vec![1];
    if max > 1 {
        order.push(max);
    }
    order.extend(2..max);
    order
}

impl CanvasManager {
    /// Build a manager from disk, falling back to `cfg.initial_canvas_count`
    /// blank canvases for first-time launches.
    ///
    /// Only the first slide is read before this returns; the others are
    /// read on background threads (`poll_loads` takes them in), and a
    /// slide the user goes to before then is read on the spot.
    pub fn start_loading(cfg: &AppConfig) -> Self {
        Self::start_loading_with(cfg, background_workers())
    }

    /// Read every slide before returning.
    #[cfg(test)]
    pub fn load_or_default(cfg: &AppConfig) -> Self {
        let mut m = Self::start_loading(cfg);
        m.finish_loading();
        m
    }

    /// `start_loading` with `workers` loader threads. With none, slides
    /// are only read when visited or by `finish_loading`.
    fn start_loading_with(cfg: &AppConfig, workers: usize) -> Self {
        let passes = cfg.smoothing_level.position_passes();
        let mut m = Self {
            canvases: Vec::new(),
            active: 0,
            load_errors: Vec::new(),
            protected: Vec::new(),
            prune_orphans: false,
            media_to_check: HashSet::new(),
            media_cleanup_due: false,
            loading: None,
            saved_views: Vec::new(),
            file_views: crate::persistence::load_views(),
            saving: Default::default(),
        };
        // Every slot up to the highest file on disk gets an entry. A gap
        // or an unreadable file becomes a blank placeholder instead of
        // ending the list, so one bad slide can never hide the ones after
        // it.
        if let Some(max) = crate::persistence::max_canvas_index() {
            let started = Instant::now();
            m.canvases.resize_with(max + 1, || None);
            m.install(0, load_slot(0, passes));
            log::info!("slide 1 read in {:.0} ms; {max} more loading in the background", started.elapsed().as_secs_f64() * 1000.0);
            if max > 0 {
                let claims: Arc<Vec<AtomicBool>> = Arc::new((0..=max).map(|i| AtomicBool::new(i == 0)).collect());
                let order = Arc::new(load_order(max));
                let (tx, results) = mpsc::channel();
                for _ in 0..workers {
                    let (claims, order, tx) = (claims.clone(), order.clone(), tx.clone());
                    let spawned = std::thread::Builder::new().name("slide-loader".into()).spawn(move || {
                        for &idx in order.iter() {
                            if claims[idx].swap(true, Ordering::AcqRel) {
                                continue; // another thread, or the UI, has it
                            }
                            if tx.send((idx, load_slot(idx, passes))).is_err() {
                                return; // the manager is gone
                            }
                        }
                    });
                    if let Err(e) = spawned {
                        // The slides still load, on the UI thread, when visited.
                        log::warn!("could not start a slide loader thread: {e}");
                    }
                }
                m.loading = Some(BackgroundLoad { claims, results, passes, caches_stale: false, started });
            }
        }
        // First-time boot: spin up some empty canvases.
        if m.canvases.is_empty() {
            for _ in 0..cfg.initial_canvas_count.max(1) {
                m.canvases.push(Some(Canvas::default()));
            }
            m.saved_views = m.canvases.iter().map(|c| c.as_ref().map(view_of)).collect();
        }
        m
    }

    /// A manager over `canvases`, all loaded, that has touched no file.
    #[cfg(test)]
    pub(crate) fn for_tests(canvases: Vec<Canvas>) -> Self {
        let saved_views = canvases.iter().map(|c| Some(view_of(c))).collect();
        Self {
            canvases: canvases.into_iter().map(Some).collect(),
            active: 0,
            load_errors: Vec::new(),
            protected: Vec::new(),
            prune_orphans: false,
            media_to_check: HashSet::new(),
            media_cleanup_due: false,
            loading: None,
            saved_views,
            file_views: Vec::new(),
            saving: Default::default(),
        }
    }

    /// Every slide's view, if any changed since `views.ron` was last
    /// written; they then count as written. Nothing while slides are
    /// still being read: an unread slide has no view to write yet, and a
    /// made-up one would replace its real one.
    fn take_changed_views(&mut self) -> Option<Vec<SlideView>> {
        if self.loading.is_some() {
            return None;
        }
        let views: Vec<SlideView> = self.canvases.iter().flatten().map(view_of).collect();
        let same = views.len() == self.saved_views.len() && views.iter().zip(&self.saved_views).all(|(v, s)| s.as_ref() == Some(v));
        if same {
            return None;
        }
        self.saved_views = views.iter().copied().map(Some).collect();
        Some(views)
    }

    /// Put a read slot in place and note what went wrong with it.
    fn install(&mut self, idx: usize, load: SlotLoad) {
        if let Some(e) = load.error {
            self.load_errors.push(e);
        }
        if load.protected {
            self.protected.push(idx);
        }
        // The view in `views.ron` is newer than the one in the slide's
        // own file, which is only rewritten when the slide is edited.
        let mut canvas = load.canvas;
        if let Some(v) = self.file_views.get(idx) {
            apply_view(&mut canvas, v);
        }
        if self.saved_views.len() <= idx {
            self.saved_views.resize(idx + 1, None);
        }
        self.saved_views[idx] = Some(view_of(&canvas));
        self.canvases[idx] = Some(canvas);
    }

    /// `install` for a slot a loader thread read, which may predate a
    /// smoothing or pressure change.
    fn install_from_thread(&mut self, idx: usize, mut load: SlotLoad) {
        if let Some(bg) = self.loading.as_ref().filter(|bg| bg.caches_stale) {
            for layer in &mut load.canvas.layers {
                for stroke in &mut layer.strokes {
                    stroke.cache = None;
                    stroke.build_cache_with(bg.passes);
                }
            }
        }
        self.install(idx, load);
    }

    /// Every loader thread has stopped. A slot one of them claimed but
    /// never delivered (the thread panicked while reading it) is treated
    /// like an unreadable file that could not be copied aside: a
    /// placeholder that is never written, so the file stays as it is.
    fn loaders_gone(&mut self) {
        let Some(bg) = self.loading.as_ref() else { return };
        let lost: Vec<usize> = (0..bg.claims.len())
            .filter(|&i| self.canvases[i].is_none() && bg.claims[i].load(Ordering::Acquire))
            .collect();
        for idx in lost {
            log::warn!("reading canvas_{idx}.ron failed; its file is left untouched");
            self.install(idx, SlotLoad {
                canvas: placeholder_canvas(),
                error: Some(format!("Slide {} could not be read — its file is left untouched", idx + 1)),
                protected: true,
            });
        }
    }

    /// Drop the loader once every slot is in.
    fn check_loading_done(&mut self) {
        if let Some(bg) = self.loading.as_ref() {
            if self.canvases.iter().all(Option::is_some) {
                log::info!("all {} slides read in {:.0} ms", bg.claims.len(), bg.started.elapsed().as_secs_f64() * 1000.0);
                crate::perf::line(format!("background load: all {} slides in {:.1} ms", bg.claims.len(), crate::perf::ms(bg.started.elapsed())));
                self.loading = None;
            }
        }
    }

    /// Take in the slides the loader threads have finished. Never
    /// blocks; call once a frame.
    pub fn poll_loads(&mut self) {
        while let Some(bg) = self.loading.as_ref() {
            match bg.results.try_recv() {
                Ok((idx, load)) => self.install_from_thread(idx, load),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.loaders_gone();
                    break;
                }
            }
        }
        self.check_loading_done();
    }

    /// Make sure slot `idx` is in memory: read it now if no loader
    /// thread has started on it, or wait for the thread that has.
    fn ensure_loaded(&mut self, idx: usize) {
        if self.canvases[idx].is_some() {
            return;
        }
        let bg = self.loading.as_ref().expect("an unread slide while no loader is running");
        if !bg.claims[idx].swap(true, Ordering::AcqRel) {
            let load = load_slot(idx, bg.passes);
            self.install(idx, load);
        }
        while self.canvases[idx].is_none() {
            let Some(bg) = self.loading.as_ref() else { break };
            match bg.results.recv() {
                Ok((i, load)) => self.install_from_thread(i, load),
                Err(mpsc::RecvError) => self.loaders_gone(),
            }
        }
        self.check_loading_done();
    }

    /// Slides are still being read in the background.
    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Wait until every slide is in memory.
    pub fn finish_loading(&mut self) {
        for idx in 0..self.canvases.len() {
            self.ensure_loaded(idx);
        }
    }

    /// Slides that failed to load at startup, one message each. Returns
    /// them all at once when every slide has been read; later calls get
    /// an empty list.
    pub fn take_load_errors(&mut self) -> Vec<String> {
        if self.loading.is_some() {
            return Vec::new();
        }
        std::mem::take(&mut self.load_errors)
    }

    /// Mutable access to the active canvas (used by tools).
    ///
    /// This is the *only* way to obtain a `&mut Canvas` (the `canvases`
    /// field is private), which makes it the single choke point for
    /// "something changed" — so it is where the autosave's dirty flag
    /// gets set. Marking on hand-out rather than on actual mutation
    /// deliberately errs toward over-saving: a caller that takes `&mut`
    /// and changes nothing costs one redundant write, whereas a missed
    /// mark would silently lose the user's work.
    pub fn active_mut(&mut self) -> &mut Canvas {
        let c = self.canvases[self.active].as_mut().expect("the active slide is always loaded");
        c.dirty = true;
        c
    }

    /// Run an input event (a pen or mouse gesture step, a key) that may
    /// or may not edit the active canvas, flagging it for the autosave
    /// only if it did. Most presses edit nothing — a click that only
    /// selects, a laser-pointer stroke — and on a slide of hundreds of
    /// megabytes each needless save is seconds of work. Every edit
    /// records an undo step or changes an object count, a layer's `rev`
    /// or the active layer, which `Canvas::edit_stamp` covers.
    pub fn edit_active<R>(&mut self, f: impl FnOnce(&mut Canvas) -> R) -> R {
        let c = self.canvases[self.active].as_mut().expect("the active slide is always loaded");
        let before = c.edit_stamp();
        let r = f(c);
        if c.edit_stamp() != before {
            c.dirty = true;
        }
        r
    }

    /// The active canvas's pan and zoom, for moving the view. Unlike
    /// `active_mut` this does not flag the canvas for the autosave:
    /// looking around is not an edit, and rewriting a big slide every
    /// few seconds while the user pans and zooms cost seconds of CPU
    /// per save. The autosave writes views to their own small file
    /// instead (`persistence::save_views`).
    pub fn active_view_mut(&mut self) -> (&mut [f32; 2], &mut f32) {
        let c = self.canvases[self.active].as_mut().expect("the active slide is always loaded");
        (&mut c.pan, &mut c.zoom)
    }

    /// Read-only access to the active canvas (used by renderer + screenshot).
    pub fn active(&self) -> &Canvas {
        self.canvases[self.active].as_ref().expect("the active slide is always loaded")
    }

    /// Index of the active canvas (for status bar display).
    pub fn active_index(&self) -> usize { self.active }

    /// Total canvas count (for status bar display: "2 / 5").
    pub fn count(&self) -> usize { self.canvases.len() }

    /// Rebuild every stroke's render cache with the given position-pass
    /// budget. Called when the user switches smoothing level so the
    /// existing strokes get rerendered with the new look.
    pub fn rebuild_all_caches(&mut self, position_passes: usize) {
        for canvas in self.canvases.iter_mut().flatten() {
            for layer in &mut canvas.layers {
                for stroke in &mut layer.strokes {
                    stroke.cache = None;
                    stroke.build_cache_with(position_passes);
                }
            }
        }
        // Slides still being read get the same treatment when they land.
        if let Some(bg) = self.loading.as_mut() {
            bg.passes = position_passes;
            bg.caches_stale = true;
        }
    }

    /// Move to the next canvas. If we're already on the *last* canvas, a
    /// fresh blank canvas is appended and we move into it. This gives the
    /// "infinite canvases on right-arrow" feel — the user can keep pressing
    /// → to start a new sheet whenever they run out of space.
    pub fn next(&mut self) {
        if self.active + 1 >= self.canvases.len() {
            // At the end → grow a new blank canvas.
            self.canvases.push(Some(Canvas::default()));
        }
        self.active += 1;
        self.ensure_loaded(self.active);
    }

    /// Append a ready-made canvas (an opened PureRef board) as the last
    /// slide and move into it. Returns its index. The canvas keeps its
    /// own `dirty` flag; a `Canvas::default()`-built one is dirty, so the
    /// next autosave writes it.
    pub fn push_canvas(&mut self, canvas: Canvas) -> usize {
        self.canvases.push(Some(canvas));
        self.active = self.canvases.len() - 1;
        self.active
    }

    /// Move to the previous canvas. Wraps to the last when at the first
    /// (going-backwards is symmetrical: the user explicitly asked to leave
    /// the first sheet, so wrap them to the most recent one).
    pub fn prev(&mut self) {
        if self.active == 0 {
            self.active = self.canvases.len() - 1;
        } else {
            self.active -= 1;
        }
        self.ensure_loaded(self.active);
    }

    /// Delete the active canvas.
    /// * If only one canvas exists we *clear* it instead of deleting (keep
    ///   the user always on at least one sheet).
    /// * Otherwise we remove it and shift the active index back by one
    ///   (or stay at 0 if we just removed canvas 0).
    pub fn delete_active(&mut self) {
        // Every later slide gets rewritten one file slot down, so each
        // must be in memory first — a slide still being read has nothing
        // to rewrite, and its file would be pruned or overwritten. This
        // also settles which slots end up protected.
        self.finish_loading();
        // Deleting shifts every later slide down a file slot. With a
        // protected slot in play that shift would point the "never write
        // this index" guard at the wrong file, so refuse instead.
        if !self.protected.is_empty() {
            log::warn!("delete canvas ignored: an unreadable slide file is being kept in place");
            return;
        }
        // Its videos and GIFs become candidates for deletion; the next
        // save decides which are really unused.
        if let Some(c) = &self.canvases[self.active] {
            c.media_files(&mut self.media_to_check);
        }
        self.media_cleanup_due = true;
        if self.canvases.len() == 1 {
            self.canvases[0] = Some(Canvas::default());
            self.active = 0;
            return;
        }
        let i = self.active;
        self.canvases.remove(i);
        if self.active >= self.canvases.len() {
            self.active = self.canvases.len() - 1;
        }
        // Removing a canvas shifts every later one down a file slot, so
        // their existing files now hold the wrong content — everything
        // has to be rewritten, not just the deleted index.
        for c in self.canvases.iter_mut().flatten() {
            c.dirty = true;
        }
        // Best-effort: also remove the on-disk file for the removed slot,
        // and shift down everything past it. Simplest is to re-save all,
        // which `save_all` does on the next debounced tick.
        self.prune_orphans = true;
    }

    /// Save every dirty canvas to disk on a background thread. Skipped
    /// canvases stay dirty for the next tick; written canvases clear
    /// their flag.
    ///
    /// Why background: even one PNG-bearing canvas serialises to
    /// multi-MB of RON. Doing that on the UI thread every debounce
    /// stalled the overlay for ~1 s every 1.5 s. The save thread takes
    /// a `Vec<(index, cloned_canvas)>` and writes them at its own
    /// pace.
    ///
    /// After an explicit canvas delete, also removes the now-stale files
    /// past `canvases.len()` so the deleted slot doesn't linger, then the
    /// deleted slide's videos and GIFs that nothing else uses.
    ///
    /// `media_in_use` holds media files referenced outside the slides
    /// (the in-app clipboard); `None` means an import is still running,
    /// whose file name is not known yet, so no media is deleted now —
    /// what is left waits for `save_all_blocking` at exit.
    pub fn save_all(&mut self, media_in_use: Option<&HashSet<String>>) {
        // One save at a time. The slides stay dirty, so the next tick
        // after this one finishes writes their newest state.
        if self.saving.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if let Some(views) = self.take_changed_views() {
            std::thread::spawn(move || {
                if let Err(e) = crate::persistence::save_views(&views) {
                    log::warn!("failed to save slide views: {e}");
                }
            });
        }
        // Collect dirty canvases as (index, clone). We need to clone
        // because the worker thread owns them while serialising — the
        // UI thread keeps drawing meanwhile. A clone of a PNG-bearing
        // canvas is cheap relative to RON serialisation because it is
        // just an `Arc`-less deep copy of bytes; serialisation
        // formats those bytes into ASCII which is 5-10× larger.
        // A slide still being read is `None` here and never written.
        let copy_started = std::time::Instant::now();
        let mut dirty: Vec<(usize, Canvas)> = Vec::new();
        for (i, slot) in self.canvases.iter_mut().enumerate() {
            let Some(c) = slot else { continue };
            if c.dirty && !self.protected.contains(&i) {
                dirty.push((i, c.clone()));
                c.dirty = false;
            }
        }
        let count = self.canvases.len();
        if dirty.is_empty() {
            // Nothing changed — skip the worker entirely. Most ticks
            // hit this path; the heavy work only fires after a real
            // edit.
            return;
        }
        let prune = std::mem::take(&mut self.prune_orphans);
        let media = match media_in_use {
            Some(keep) if std::mem::take(&mut self.media_cleanup_due) => self.unused_media(keep),
            _ => Vec::new(),
        };
        crate::perf::line(format!("autosave: copied {} slide(s) on the UI thread in {:.1} ms", dirty.len(), crate::perf::ms(copy_started.elapsed())));
        self.saving.store(true, std::sync::atomic::Ordering::Release);
        let done = SaveDone(std::sync::Arc::clone(&self.saving));
        std::thread::spawn(move || {
            // Cleared however the thread ends, panic included.
            let _done = done;
            for (i, c) in dirty {
                let t = std::time::Instant::now();
                if let Err(e) = crate::persistence::save_canvas(i, &c) {
                    log::warn!("failed to save canvas {i}: {e}");
                }
                if crate::perf::enabled() {
                    let size = crate::persistence::paths::canvases_dir()
                        .ok()
                        .and_then(|d| std::fs::metadata(d.join(format!("canvas_{i}.ron"))).ok())
                        .map_or(0, |m| m.len());
                    crate::perf::line(format!("autosave: slide {} written in {:.1} ms, {:.1} MB", i + 1, crate::perf::ms(t.elapsed()), size as f64 / 1e6));
                }
            }
            if prune {
                remove_stale_canvas_files(count);
            }
            delete_media_files(&media);
        });
    }

    /// Take out of `media_to_check` the files nothing uses — no slide, no
    /// undo history, nothing in `keep` — and return them for deletion.
    /// Files a slide uses are dropped from the list (they are not
    /// orphans); files only `keep` uses stay for a later check.
    fn unused_media(&mut self, keep: &HashSet<String>) -> Vec<String> {
        // A slide still being read may use any of them; check later.
        // (Deleting a slide reads them all first, so this is a guard.)
        if self.canvases.iter().any(Option::is_none) {
            return Vec::new();
        }
        let mut used = HashSet::new();
        for c in self.canvases.iter().flatten() {
            c.media_files(&mut used);
        }
        self.media_to_check.retain(|f| !used.contains(f));
        let unused: Vec<String> = self.media_to_check.iter().filter(|f| !keep.contains(*f)).cloned().collect();
        for f in &unused {
            self.media_to_check.remove(f);
        }
        unused
    }

    /// Synchronous variant used at app shutdown — blocks until every
    /// dirty canvas hits disk so a forced kill after `on_exit` can't
    /// drop edits.
    pub fn save_all_blocking(&mut self) {
        // A background save may still be writing; writing the same slide
        // alongside it could corrupt the file. Wait it out.
        while self.saving.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if let Some(views) = self.take_changed_views() {
            if let Err(e) = crate::persistence::save_views(&views) {
                log::warn!("failed to save slide views: {e}");
            }
        }
        for (i, slot) in self.canvases.iter_mut().enumerate() {
            let Some(c) = slot else { continue };
            if !c.dirty || self.protected.contains(&i) { continue; }
            if let Err(e) = crate::persistence::save_canvas(i, c) {
                log::warn!("failed to save canvas {i}: {e}");
            } else {
                c.dirty = false;
            }
        }
        if std::mem::take(&mut self.prune_orphans) {
            remove_stale_canvas_files(self.canvases.len());
        }
        // The clipboard and any unfinished import end with the app, so
        // only the slides still count.
        if !self.media_to_check.is_empty() {
            self.media_cleanup_due = false;
            let unused = self.unused_media(&HashSet::new());
            delete_media_files(&unused);
        }
    }
}

/// Clears `CanvasManager::saving` when the save thread ends.
struct SaveDone(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for SaveDone {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn view_of(c: &Canvas) -> SlideView {
    SlideView { pan: c.pan, zoom: c.zoom }
}

/// Give a slide a view from `views.ron`, unless it is not a usable one
/// (a damaged or hand-edited file); the view stored in the slide's own
/// file then stays.
fn apply_view(c: &mut Canvas, v: &SlideView) {
    use crate::interaction::zoom_pan::{MAX_ZOOM, MIN_ZOOM};
    if v.pan.iter().all(|p| p.is_finite()) && (MIN_ZOOM..=MAX_ZOOM).contains(&v.zoom) {
        c.pan = v.pan;
        c.zoom = v.zoom;
    }
}

#[cfg(test)]
fn apply_views(canvases: &mut [Canvas], views: &[SlideView]) {
    for (c, v) in canvases.iter_mut().zip(views) {
        apply_view(c, v);
    }
}

/// Delete these files from the media folder. Only the app's own copies
/// live there — the file the user originally dropped or picked is never
/// touched. A failure (say, the file is still open somewhere) is logged
/// and the file left in place.
fn delete_media_files(names: &[String]) {
    if names.is_empty() { return; }
    let Ok(dir) = crate::persistence::paths::media_dir() else { return };
    delete_media_files_in(&dir, names);
}

fn delete_media_files_in(dir: &std::path::Path, names: &[String]) {
    for name in names {
        // Names come from slide files; refuse anything that is not a
        // plain file name, so a damaged slide cannot point outside.
        if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
            log::warn!("not deleting media {name:?}: not a plain file name");
            continue;
        }
        let path = dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => log::info!("deleted unused media {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("could not delete unused media {}: {e}", path.display()),
        }
    }
}

/// Remove `canvas_N.ron` files at and past `count`. Only ever called
/// right after the user deleted a canvas, when those slots are known to
/// hold content that has just been rewritten one index lower.
fn remove_stale_canvas_files(count: usize) {
    if let Ok(dir) = crate::persistence::paths::canvases_dir() {
        for extra in count..(count + 10) {
            let p = dir.join(format!("canvas_{extra}.ron"));
            if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

#[cfg(test)]
mod media_cleanup_tests {
    use super::*;
    use crate::canvas::canvas::UndoEntry;
    use crate::canvas::shape::{fresh_media_id, MediaKind, MediaRef, RasterKind, Shape};
    use std::sync::Arc;

    fn video(file: &str) -> Shape {
        Shape::Raster {
            pos: [0.0, 0.0], size: [10.0, 10.0], png: Arc::new(vec![1]), px_size: [1, 1],
            color: [0; 4], rotation: 0.0, kind: RasterKind::Image,
            media: Some(Box::new(MediaRef {
                file: file.into(), kind: MediaKind::Video, source_name: String::new(), meta: None, id: fresh_media_id(),
            })),
            picture: None,
        }
    }

    fn manager(canvases: Vec<Canvas>) -> CanvasManager {
        CanvasManager::for_tests(canvases)
    }

    fn names(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn media_files_include_the_undo_history() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(video("a.mp4"));
        c.undo_log.push(UndoEntry::ObjectsRemoved(0, Vec::new(), vec![(0, video("b.mp4"))]));
        c.redo_log.push(UndoEntry::ShapeErased(0, 0, video("c.gif")));
        let mut out = HashSet::new();
        c.media_files(&mut out);
        assert_eq!(out, names(&["a.mp4", "b.mp4", "c.gif"]));
    }

    #[test]
    fn deleting_a_slide_frees_only_media_nothing_else_uses() {
        let mut gone = Canvas::default();
        gone.layers[0].shapes.extend([video("only-here.mp4"), video("shared.mp4"), video("copied.gif")]);
        gone.undo_log.push(UndoEntry::ShapeErased(0, 0, video("in-undo.mp4")));
        let mut other = Canvas::default();
        other.layers[0].shapes.push(video("shared.mp4"));
        let mut m = manager(vec![gone, other]);

        m.delete_active();
        assert!(m.media_cleanup_due);
        let mut unused = m.unused_media(&names(&["copied.gif"]));
        unused.sort();
        assert_eq!(unused, vec!["in-undo.mp4".to_string(), "only-here.mp4".to_string()]);
        // Still on the clipboard: checked again later. In use on another
        // slide: no longer a candidate.
        assert_eq!(m.media_to_check, names(&["copied.gif"]));
        // At exit the clipboard is gone.
        assert_eq!(m.unused_media(&HashSet::new()), vec!["copied.gif".to_string()]);
    }

    #[test]
    fn clearing_the_last_slide_frees_its_media_too() {
        let mut only = Canvas::default();
        only.layers[0].shapes.push(video("v.mp4"));
        let mut m = manager(vec![only]);
        m.delete_active();
        assert_eq!(m.unused_media(&HashSet::new()), vec!["v.mp4".to_string()]);
    }

    #[test]
    fn deletion_stays_inside_the_media_folder() {
        let root = std::env::temp_dir().join(format!("osn-media-gc-{}", std::process::id()));
        let media = root.join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("x.mp4"), b"x").unwrap();
        std::fs::write(root.join("outside.txt"), b"keep").unwrap();
        delete_media_files_in(&media, &["x.mp4".into(), "../outside.txt".into(), "missing.gif".into()]);
        assert!(!media.join("x.mp4").exists());
        assert!(root.join("outside.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;

    fn clean() -> Canvas {
        let mut c = Canvas::default();
        c.dirty = false;
        c
    }

    #[test]
    fn moving_the_view_saves_the_views_but_not_the_slide() {
        let mut m = CanvasManager::for_tests(vec![clean(), clean()]);
        assert_eq!(m.take_changed_views(), None);
        let (pan, zoom) = m.active_view_mut();
        pan[0] += 10.0;
        *zoom = 2.0;
        assert!(!m.active().dirty, "a view change is not an edit");
        let fresh = SlideView { pan: [0.0, 0.0], zoom: 1.0 };
        assert_eq!(m.take_changed_views(), Some(vec![SlideView { pan: [10.0, 0.0], zoom: 2.0 }, fresh]));
        assert_eq!(m.take_changed_views(), None, "written once, then quiet until the next change");
        // A real edit still flags the slide.
        m.active_mut();
        assert!(m.active().dirty);
    }

    #[test]
    fn a_save_waits_for_the_one_in_flight() {
        let mut m = CanvasManager::for_tests(vec![clean()]);
        m.active_mut();
        m.saving.store(true, std::sync::atomic::Ordering::Release);
        // Returns before touching the disk or the dirty flags.
        m.save_all(None);
        assert!(m.active().dirty, "kept for the save after this one");
        // The flag clears however the save thread ends.
        drop(SaveDone(std::sync::Arc::clone(&m.saving)));
        assert!(!m.saving.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn only_gestures_that_edit_flag_the_slide() {
        use crate::canvas::stroke::PenSample;
        use crate::tools::{ActiveTool, ToolKind};
        let mut m = CanvasManager::for_tests(vec![clean()]);
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        let gesture = |m: &mut CanvasManager, tool: &mut ActiveTool, from: [f32; 2], to: [f32; 2]| {
            m.edit_active(|c| tool.begin(PenSample::from_pos(from), c));
            m.edit_active(|c| tool.update(PenSample::from_pos(to), c));
            m.edit_active(|c| tool.end(PenSample::from_pos(to), c));
        };

        // A laser-pointer stroke leaves nothing on the slide.
        tool.kind = ToolKind::Laser;
        gesture(&mut m, &mut tool, [0.0, 0.0], [100.0, 0.0]);
        assert!(!m.active().dirty, "the laser is not an edit");

        // Ink is.
        tool.kind = ToolKind::Pen;
        gesture(&mut m, &mut tool, [0.0, 0.0], [100.0, 0.0]);
        assert!(m.active().dirty);
        assert_eq!(m.active().layers[0].strokes.len(), 1);
        m.active_mut().dirty = false;

        // Clicking the ink only selects it…
        tool.kind = ToolKind::Select;
        tool.select.sync(true, 0, m.active());
        gesture(&mut m, &mut tool, [50.0, 0.0], [50.0, 0.0]);
        assert!(!tool.select.sel.is_empty());
        assert!(!m.active().dirty, "selecting is not an edit");
        // …dragging it moves it.
        gesture(&mut m, &mut tool, [50.0, 0.0], [80.0, 30.0]);
        assert!(m.active().dirty);
    }

    #[test]
    fn stored_views_apply_unless_unusable() {
        let mut canvases = vec![clean(), clean(), clean()];
        canvases[2].zoom = 3.0;
        let views = [
            SlideView { pan: [5.0, -7.0], zoom: 0.25 },
            SlideView { pan: [f32::NAN, 0.0], zoom: 2.0 },
            SlideView { pan: [1.0, 1.0], zoom: 0.0 },
            SlideView { pan: [9.0, 9.0], zoom: 9.0 }, // no such slide
        ];
        apply_views(&mut canvases, &views);
        assert_eq!((canvases[0].pan, canvases[0].zoom), ([5.0, -7.0], 0.25));
        assert_eq!((canvases[1].pan, canvases[1].zoom), ([0.0, 0.0], 1.0));
        assert_eq!((canvases[2].pan, canvases[2].zoom), ([0.0, 0.0], 3.0));
        assert!(canvases.iter().all(|c| !c.dirty));
    }
}

#[cfg(test)]
mod loader_tests {
    use super::*;

    /// One unreadable slide and one missing slide in the middle of the
    /// list must not hide, overwrite or delete anything.
    ///
    /// This is the only test that sets `OSN_DATA_DIR`; keep it that way,
    /// the variable is process-wide.
    #[test]
    fn a_bad_slide_does_not_cost_the_others() {
        let root = std::env::temp_dir().join(format!("osn-loader-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("canvases");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("OSN_DATA_DIR", &root);

        let good = crate::persistence::serialize_canvas(&Canvas::default()).unwrap();
        std::fs::write(dir.join("canvas_0.ron"), &good).unwrap();
        std::fs::write(dir.join("canvas_1.ron"), "this is not a canvas").unwrap();
        // canvas_2.ron deliberately absent.
        std::fs::write(dir.join("canvas_3.ron"), &good).unwrap();

        let mut m = CanvasManager::load_or_default(&AppConfig::default());
        assert_eq!(m.count(), 4, "every slot up to the highest file is kept");
        assert_eq!(m.take_load_errors().len(), 1);
        let kept_aside = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with("canvas_1.ron.unreadable-"));
        assert!(kept_aside, "the unreadable file is copied aside");

        // A save with nothing edited must leave every file as it was.
        m.save_all_blocking();
        assert_eq!(std::fs::read_to_string(dir.join("canvas_1.ron")).unwrap(), "this is not a canvas");
        assert!(!dir.join("canvas_2.ron").exists());
        assert!(dir.join("canvas_3.ron").exists());
        assert!(!dir.join("views.ron").exists(), "no view moved, nothing to write");

        // Looking around a slide writes only the views, which the next
        // start picks up.
        let slide_0 = std::fs::read_to_string(dir.join("canvas_0.ron")).unwrap();
        *m.active_view_mut().1 = 4.0;
        m.save_all_blocking();
        assert_eq!(std::fs::read_to_string(dir.join("canvas_0.ron")).unwrap(), slide_0);
        let m = CanvasManager::load_or_default(&AppConfig::default());
        assert_eq!(m.active().zoom, 4.0);

        slides_still_loading_are_never_written(&dir);

        std::env::remove_var("OSN_DATA_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A slide the background loader has not read yet is never written,
    /// is read when the user goes to it, and is read before a delete
    /// shifts the slides down a slot. Runs inside the test above, which
    /// owns `OSN_DATA_DIR`.
    fn slides_still_loading_are_never_written(dir: &std::path::Path) {
        for e in std::fs::read_dir(dir).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
        }
        let slide = |x: f32| {
            let mut c = Canvas::default();
            c.pan = [x, 0.0];
            crate::persistence::serialize_canvas(&c).unwrap()
        };
        std::fs::write(dir.join("canvas_0.ron"), slide(10.0)).unwrap();
        std::fs::write(dir.join("canvas_1.ron"), slide(11.0)).unwrap();
        // canvas_2.ron deliberately absent.
        std::fs::write(dir.join("canvas_3.ron"), slide(13.0)).unwrap();
        let file = |i: usize| std::fs::read_to_string(dir.join(format!("canvas_{i}.ron"))).unwrap();
        let pan = |i: usize| crate::persistence::parse_canvas(&file(i), 3).unwrap().pan;
        let (one, three) = (file(1), file(3));

        // No loader threads, so slides 2-4 stay unread until visited.
        let mut m = CanvasManager::start_loading_with(&AppConfig::default(), 0);
        assert_eq!(m.count(), 4);
        assert_eq!(m.active().pan, [10.0, 0.0]);
        m.active_mut().pan = [20.0, 0.0];
        m.save_all_blocking();
        assert_eq!(pan(0), [20.0, 0.0]);
        assert_eq!(file(1), one, "an unread slide is not written");
        assert_eq!(file(3), three, "an unread slide is not written");
        assert!(!dir.join("canvas_2.ron").exists());

        // Back from the first slide wraps to the last, which is read then.
        let mut wrap = CanvasManager::start_loading_with(&AppConfig::default(), 0);
        wrap.prev();
        assert_eq!((wrap.active_index(), wrap.active().pan), (3, [13.0, 0.0]));

        // Delete slide 2: slides 3 and 4 are read first, then all shift.
        m.next();
        assert_eq!(m.active().pan, [11.0, 0.0]);
        m.delete_active();
        assert_eq!(m.count(), 3);
        m.save_all_blocking();
        assert_eq!(pan(0), [20.0, 0.0]);
        assert_eq!(pan(1), Canvas::default().pan, "the gap is now slot 1");
        assert_eq!(pan(2), [13.0, 0.0], "the last slide moved down, not lost");
        assert!(!dir.join("canvas_3.ron").exists());
        assert!(m.take_load_errors().is_empty());
    }
}
