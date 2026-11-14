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
pub mod picture_store;
pub mod pins;
pub mod render;
pub mod shape;
pub mod smoothing;
pub mod stroke;

use crate::canvas::canvas::Canvas;
use crate::config::AppConfig;
use crate::persistence::{LoadProgress, SlideView};
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
    /// Media files (videos, GIFs, stored pictures) that deleted slides
    /// used. Each is deleted from
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
    /// Pins: objects of one slide kept on screen, on that slide or on
    /// every slide.
    pins: pins::PinBoard,
    /// One name per slide, empty for an unnamed one; always as long as
    /// `canvases`. Kept here rather than on `Canvas` so a slide still
    /// being read has its name too, and renaming never dirties a slide.
    names: Vec<String>,
    /// The names as `names.ron` last received them.
    saved_names: Vec<String>,
    /// Visited slides, most recent first. The active slide leads, except
    /// during a recently-used walk, which settles the order when it ends.
    recent: Vec<usize>,
    /// Where in `recent` a recently-used walk (Ctrl held, Tab pressed
    /// once or more) stands; `None` when no walk is under way.
    recent_walk: Option<usize>,
    /// One flag per slide, always as long as `canvases`: a locked slide
    /// takes no edits, only panning and zooming. Kept here rather than
    /// on `Canvas` for the same reasons as `names`.
    locks: Vec<bool>,
    /// The locks as `locks.ron` last received them.
    saved_locks: Vec<bool>,
    /// What shows while the active slide is still being read: a blank
    /// slide at the slide's stored view. The app takes no edits while a
    /// slide opens, and this is never saved.
    opening: Canvas,
    /// Moves big pictures kept in slides out to the media folder.
    pictures: picture_store::PictureStore,
    /// The running save deletes unused media. Until it ends no picture is
    /// stored or swapped in, as one could name a file about to go.
    deleting_media: bool,
    /// Slides were reordered and their files are yet to follow:
    /// `pending_move[j]` is the slot whose file holds the slide now at
    /// `j`. The renames run before the next save writes anything
    /// (`apply_pending_move`), never alongside one.
    pending_move: Option<Vec<usize>>,
    /// The last try at those renames failed; until one goes through,
    /// no further reorder is taken (it would no longer match the files).
    move_stuck: bool,
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
fn load_slot(idx: usize, passes: usize, progress: &LoadProgress) -> SlotLoad {
    use crate::persistence::LoadOutcome;
    match crate::persistence::load_canvas(idx, passes, progress) {
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

/// `load_slot` on a reader thread. A panic while reading is treated like
/// an unreadable file that could not be copied aside, so whoever waits
/// for the slot still gets it.
fn read_slot(idx: usize, passes: usize, progress: &LoadProgress) -> SlotLoad {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_slot(idx, passes, progress))).unwrap_or_else(|_| {
        log::warn!("reading canvas_{idx}.ron failed; its file is left untouched");
        SlotLoad {
            canvas: placeholder_canvas(),
            error: Some(format!("Slide {} could not be read — its file is left untouched", idx + 1)),
            protected: true,
        }
    })
}

/// The slides read on background threads after startup, so the window
/// opens as soon as the first one is in.
struct BackgroundLoad {
    /// Per slot: someone — a loader thread, or the UI thread when the
    /// user goes to a slide before its turn — has started reading it.
    /// Whoever flips it first reads the file; nobody else touches it.
    claims: Arc<Vec<AtomicBool>>,
    results: mpsc::Receiver<(usize, SlotLoad)>,
    /// For reading a slide out of turn (`open`). Dropped once every slot
    /// is claimed, so the channel closes when the last reader is done.
    tx: Option<mpsc::Sender<(usize, SlotLoad)>>,
    /// Per slot, how far reading it has got.
    progress: Arc<Vec<LoadProgress>>,
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
        // A reorder a crash cut short is finished before anything is read.
        crate::persistence::finish_slide_move();
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
            pins: pins::PinBoard::new(crate::persistence::load_pins()),
            names: crate::persistence::load_names(),
            saved_names: Vec::new(),
            recent: vec![0],
            recent_walk: None,
            locks: crate::persistence::load_locks(),
            saved_locks: Vec::new(),
            opening: placeholder_canvas(),
            pictures: picture_store::PictureStore::new(crate::persistence::paths::media_dir().ok()),
            deleting_media: false,
            pending_move: None,
            move_stuck: false,
        };
        // Every slot up to the highest file on disk gets an entry. A gap
        // or an unreadable file becomes a blank placeholder instead of
        // ending the list, so one bad slide can never hide the ones after
        // it.
        if let Some(max) = crate::persistence::max_canvas_index() {
            let started = Instant::now();
            m.canvases.resize_with(max + 1, || None);
            let progress: Arc<Vec<LoadProgress>> = Arc::new((0..=max).map(|_| LoadProgress::default()).collect());
            m.install(0, load_slot(0, passes, &progress[0]));
            log::info!("slide 1 read in {:.0} ms; {max} more loading in the background", started.elapsed().as_secs_f64() * 1000.0);
            if max > 0 {
                let claims: Arc<Vec<AtomicBool>> = Arc::new((0..=max).map(|i| AtomicBool::new(i == 0)).collect());
                let order = Arc::new(load_order(max));
                let (tx, results) = mpsc::channel();
                for _ in 0..workers {
                    let (claims, order, tx, progress) = (claims.clone(), order.clone(), tx.clone(), progress.clone());
                    let spawned = std::thread::Builder::new().name("slide-loader".into()).spawn(move || {
                        for &idx in order.iter() {
                            if claims[idx].swap(true, Ordering::AcqRel) {
                                continue; // another thread, or the UI, has it
                            }
                            if tx.send((idx, read_slot(idx, passes, &progress[idx]))).is_err() {
                                return; // the manager is gone
                            }
                        }
                    });
                    if let Err(e) = spawned {
                        // The slides still load, on the UI thread, when visited.
                        log::warn!("could not start a slide loader thread: {e}");
                    }
                }
                m.loading = Some(BackgroundLoad { claims, results, tx: Some(tx), progress, passes, caches_stale: false, started });
            }
        }
        // First-time boot: spin up some empty canvases.
        if m.canvases.is_empty() {
            for _ in 0..cfg.initial_canvas_count.max(1) {
                m.canvases.push(Some(Canvas::default()));
            }
            m.saved_views = m.canvases.iter().map(|c| c.as_ref().map(view_of)).collect();
        }
        // A short or missing `names.ron` leaves the rest unnamed, and
        // `locks.ron` the rest unlocked.
        m.names.resize(m.canvases.len(), String::new());
        m.saved_names = m.names.clone();
        m.locks.resize(m.canvases.len(), false);
        m.saved_locks = m.locks.clone();
        m
    }

    /// A manager over `canvases`, all loaded, that has touched no file.
    #[cfg(test)]
    pub(crate) fn for_tests(canvases: Vec<Canvas>) -> Self {
        let saved_views = canvases.iter().map(|c| Some(view_of(c))).collect();
        let names = vec![String::new(); canvases.len()];
        let locks = vec![false; canvases.len()];
        Self {
            saved_names: names.clone(),
            names,
            saved_locks: locks.clone(),
            locks,
            opening: placeholder_canvas(),
            recent: vec![0],
            recent_walk: None,
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
            pins: pins::PinBoard::default(),
            pictures: picture_store::PictureStore::new(None),
            deleting_media: false,
            pending_move: None,
            move_stuck: false,
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

    /// Every slide's name, if any changed since `names.ron` was last
    /// written; they then count as written. Unlike the views these are
    /// known for every slide from the start, read or not.
    fn take_changed_names(&mut self) -> Option<Vec<String>> {
        if self.names == self.saved_names {
            return None;
        }
        self.saved_names = self.names.clone();
        Some(self.names.clone())
    }

    /// Every slide's lock, if any changed since `locks.ron` was last
    /// written; they then count as written.
    fn take_changed_locks(&mut self) -> Option<Vec<bool>> {
        if self.locks == self.saved_locks {
            return None;
        }
        self.saved_locks = self.locks.clone();
        Some(self.locks.clone())
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
        if let Some(bg) = self.loading.as_mut() {
            if bg.tx.is_some() && bg.claims.iter().all(|c| c.load(Ordering::Acquire)) {
                bg.tx = None;
            }
        }
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

    /// Move big pictures kept in the loaded slides out to the media
    /// folder (see `picture_store`). Never blocks; call once a frame.
    /// Returns each swapped picture's old bytes and new reference, so
    /// what was loaded for it carries over.
    pub fn store_pictures(&mut self) -> Vec<(Arc<Vec<u8>>, shape::PictureRef)> {
        if self.deleting_media && self.saving.load(Ordering::Acquire) {
            return Vec::new();
        }
        self.deleting_media = false;
        self.pictures.tick(&mut self.canvases, self.active)
    }

    /// Make sure slot `idx` is in memory: read it now if no loader
    /// thread has started on it, or wait for the thread that has.
    fn ensure_loaded(&mut self, idx: usize) {
        if self.canvases[idx].is_some() {
            return;
        }
        let bg = self.loading.as_ref().expect("an unread slide while no loader is running");
        if !bg.claims[idx].swap(true, Ordering::AcqRel) {
            let load = load_slot(idx, bg.passes, &bg.progress[idx]);
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

    /// Get slot `idx` on its way into memory without waiting: if nobody
    /// is reading it yet, a thread starts on it now rather than when the
    /// loader threads get to it. Until `poll_loads` takes it in, the
    /// active slide shows the `opening` stand-in.
    fn open(&mut self, idx: usize) {
        if self.canvases[idx].is_some() {
            return;
        }
        self.opening = placeholder_canvas();
        if let Some(v) = self.file_views.get(idx) {
            apply_view(&mut self.opening, v);
        }
        self.read_in_background(idx);
    }

    /// Start reading slide `idx` on a thread of its own, unless it is in
    /// already or a loader thread has it.
    fn read_in_background(&mut self, idx: usize) {
        if self.canvases[idx].is_some() {
            return;
        }
        let bg = self.loading.as_ref().expect("an unread slide while no loader is running");
        if bg.claims[idx].swap(true, Ordering::AcqRel) {
            return; // a loader thread has it
        }
        let (passes, progress) = (bg.passes, Arc::clone(&bg.progress));
        // Every slot is claimed once `tx` is gone, and this one was not.
        let tx = bg.tx.clone().expect("a slot is unclaimed");
        let theirs = Arc::clone(&progress);
        let spawned = std::thread::Builder::new().name("slide-opener".into()).spawn(move || {
            let _ = tx.send((idx, read_slot(idx, passes, &theirs[idx])));
        });
        if let Err(e) = spawned {
            log::warn!("could not start a thread to open slide {}: {e}", idx + 1);
            let load = load_slot(idx, passes, &progress[idx]);
            self.install(idx, load);
            self.check_loading_done();
        }
    }

    /// The active slide is still being read: how far along (0..=1), and
    /// the size of its file in bytes.
    pub fn opening(&self) -> Option<(f32, u64)> {
        if self.canvases[self.active].is_some() {
            return None;
        }
        let p = self.loading.as_ref()?.progress.get(self.active)?;
        Some((p.fraction(), p.size()))
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
    ///
    /// A slide still opening is waited for: an edit must never land on
    /// the stand-in, where it would be lost. The app takes no edits while
    /// a slide opens, so this wait is a last resort.
    pub fn active_mut(&mut self) -> &mut Canvas {
        self.ensure_loaded(self.active);
        let c = self.canvases[self.active].as_mut().expect("the active slide is loaded");
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
    /// Waits for a slide still opening, like `active_mut`.
    pub fn edit_active<R>(&mut self, f: impl FnOnce(&mut Canvas) -> R) -> R {
        self.ensure_loaded(self.active);
        let c = self.canvases[self.active].as_mut().expect("the active slide is loaded");
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
    /// While the slide opens, the stand-in's view moves.
    pub fn active_view_mut(&mut self) -> (&mut [f32; 2], &mut f32) {
        let c = self.canvases[self.active].as_mut().unwrap_or(&mut self.opening);
        (&mut c.pan, &mut c.zoom)
    }

    /// Read-only access to the active canvas (used by renderer +
    /// screenshot); a blank stand-in while it opens.
    pub fn active(&self) -> &Canvas {
        self.canvases[self.active].as_ref().unwrap_or(&self.opening)
    }

    /// Slide `idx` to look at, if it is read (see `prefetch`).
    pub fn slide(&self, idx: usize) -> Option<&Canvas> {
        self.canvases.get(idx)?.as_ref()
    }

    /// Run `f` on slide `idx` as seen through `pan` / `zoom` instead of
    /// its own view: how the slide window edits the slide it shows. The
    /// tools read the zoom (pens and the eraser work at their size on
    /// screen), and the Select tool its handle sizes. The slide's own view
    /// is put back when `f` returns, so looking around in the window never
    /// moves it. Flags the slide for the autosave only if `f` edited it
    /// (see `edit_active`). `None` for a slide not read yet, which this
    /// never waits for.
    pub fn edit_slide_in_view<R>(&mut self, idx: usize, pan: [f32; 2], zoom: f32, f: impl FnOnce(&mut Canvas) -> R) -> Option<R> {
        let c = self.canvases.get_mut(idx)?.as_mut()?;
        let (own_pan, own_zoom) = (c.pan, c.zoom);
        let before = c.edit_stamp();
        c.pan = pan;
        c.zoom = zoom;
        let r = f(c);
        c.pan = own_pan;
        c.zoom = own_zoom;
        if c.edit_stamp() != before {
            c.dirty = true;
        }
        Some(r)
    }

    /// Have slide `idx` read soon, without going to it: a slide shown
    /// somewhere other than the canvas (the slide window) asks for itself
    /// here. Does nothing for a slide that is in, or being read.
    pub fn prefetch(&mut self, idx: usize) {
        if idx < self.canvases.len() {
            self.read_in_background(idx);
        }
    }

    /// Slide `idx` is locked: no edits, only panning and zooming.
    pub fn is_locked(&self, idx: usize) -> bool {
        self.locks.get(idx).copied().unwrap_or(false)
    }

    /// Lock or unlock slide `idx`. Not an edit of the slide: only
    /// `locks.ron` is written.
    pub fn set_locked(&mut self, idx: usize, locked: bool) {
        if let Some(l) = self.locks.get_mut(idx) {
            *l = locked;
        }
    }

    /// The active slide takes edits: it is read and not locked.
    pub fn active_editable(&self) -> bool {
        self.canvases[self.active].is_some() && !self.is_locked(self.active)
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
            self.names.push(String::new());
            self.locks.push(false);
        }
        self.active += 1;
        self.open(self.active);
    }

    /// Move to the next canvas, wrapping from the last to the first —
    /// cycling never grows the deck, unlike `next`.
    pub fn cycle_next(&mut self) {
        self.go_to((self.active + 1) % self.canvases.len());
    }

    /// Append a ready-made canvas (an opened PureRef board) as the last
    /// slide and move into it. Returns its index. The canvas keeps its
    /// own `dirty` flag; a `Canvas::default()`-built one is dirty, so the
    /// next autosave writes it.
    pub fn push_canvas(&mut self, canvas: Canvas) -> usize {
        self.canvases.push(Some(canvas));
        self.names.push(String::new());
        self.locks.push(false);
        self.active = self.canvases.len() - 1;
        self.active
    }

    /// Go to slide `idx`. One still being read shows as opening until
    /// it is in (see `open`).
    pub fn go_to(&mut self, idx: usize) {
        if idx < self.canvases.len() {
            self.active = idx;
            self.open(idx);
        }
    }

    /// Note the active slide as the most recently visited. Call once a
    /// frame; held back while a recently-used walk is under way.
    pub fn note_visit(&mut self) {
        if self.recent_walk.is_none() && self.recent.first() != Some(&self.active) {
            let a = self.active;
            self.recent.retain(|&i| i != a);
            self.recent.insert(0, a);
        }
    }

    /// Step through the visited slides in recently-used order, like
    /// Alt+Tab: `older` goes back in time (Ctrl+Tab), else forward
    /// (Ctrl+Shift+Tab). The first step back from the active slide goes
    /// to the one visited before it, so a single tap flips between the
    /// last two; more taps before `end_recent_walk` go further back.
    /// With fewer than two slides visited it steps by slide number.
    pub fn walk_recent(&mut self, older: bool) {
        self.note_visit();
        let n = self.recent.len();
        if n < 2 {
            if older { self.cycle_next() } else { self.prev() }
            return;
        }
        let pos = self.recent_walk.unwrap_or(0);
        let pos = if older { (pos + 1) % n } else { (pos + n - 1) % n };
        self.recent_walk = Some(pos);
        self.go_to(self.recent[pos]);
    }

    /// The walk is over (Ctrl let go): the slide it stopped on becomes
    /// the most recent.
    pub fn end_recent_walk(&mut self) {
        if self.recent_walk.take().is_some() {
            self.note_visit();
        }
    }

    /// Move slide `from` to place `to`, the slides between shifting one
    /// place to make room; the slide shown stays shown. Its name, lock,
    /// view, undo history and pins go with it. Not an edit of any slide:
    /// on disk only files are renamed (see `persistence::SlideMove`), at
    /// the next save. Refused, with the reason, while that cannot be
    /// done safely.
    pub fn move_slide(&mut self, from: usize, to: usize) -> Result<(), &'static str> {
        let n = self.canvases.len();
        if from >= n || to >= n || from == to {
            return Ok(());
        }
        if self.loading.is_some() {
            return Err("The slides are still opening — move it in a moment");
        }
        if !self.protected.is_empty() {
            return Err("A slide that could not be read is kept in place, so slides can't be moved");
        }
        if self.prune_orphans {
            return Err("Still saving after the deleted slide — move it in a moment");
        }
        if self.move_stuck {
            return Err("The last move is still being written — try again in a moment");
        }
        let map = |i: usize| moved_index(from, to, i);
        // Where each slide now is came from: `order[j]` is the old place
        // of the slide now at `j`.
        let mut order: Vec<usize> = (0..n).collect();
        let o = order.remove(from);
        order.insert(to, o);
        let slide = self.canvases.remove(from);
        self.canvases.insert(to, slide);
        let name = self.names.remove(from);
        self.names.insert(to, name);
        let lock = self.locks.remove(from);
        self.locks.insert(to, lock);
        self.active = map(self.active);
        for i in &mut self.recent {
            *i = map(*i);
        }
        self.recent_walk = None;
        // Pins find their slide by number.
        self.pins.forget_scans();
        // Fold into a move whose renames are still to run: the files are
        // where that one found them.
        let disk: Vec<usize> = match self.pending_move.take() {
            Some(mut pending) => {
                pending.extend(pending.len()..n);
                order.iter().map(|&i| pending[i]).collect()
            }
            None => order,
        };
        self.pending_move = disk.iter().enumerate().any(|(j, &i)| i != j).then_some(disk);
        Ok(())
    }

    /// Run the renames a reorder is waiting on (`move_slide`), with the
    /// names, locks and views in the new order. False when they failed:
    /// nothing else should be written then, and the next call tries again.
    fn apply_pending_move(&mut self) -> bool {
        let Some(order) = self.pending_move.clone() else { return true };
        // Never on the real slides from a test.
        #[cfg(test)]
        assert!(std::env::var_os("OSN_DATA_DIR").is_some(), "a test renamed slide files without OSN_DATA_DIR");
        let views: Vec<SlideView> = self.canvases.iter().flatten().map(view_of).collect();
        let mv = crate::persistence::SlideMove { order, names: self.names.clone(), locks: self.locks.clone(), views: views.clone(), placing: false };
        let t = Instant::now();
        match crate::persistence::move_slide_files(&mv) {
            Ok(()) => {
                crate::perf::line(format!("slides reordered on disk in {:.1} ms", crate::perf::ms(t.elapsed())));
                self.pending_move = None;
                self.move_stuck = false;
                self.saved_names = mv.names;
                self.saved_locks = mv.locks;
                self.saved_views = views.into_iter().map(Some).collect();
                true
            }
            Err(e) => {
                log::warn!("could not reorder the slide files yet: {e:#}");
                self.move_stuck = true;
                false
            }
        }
    }

    /// Slide `idx`'s name; empty when it has none.
    pub fn name(&self, idx: usize) -> &str {
        self.names.get(idx).map_or("", String::as_str)
    }

    /// Name slide `idx` (surrounding spaces dropped; empty unnames it).
    /// Not an edit of the slide: only `names.ron` is written.
    pub fn rename(&mut self, idx: usize, name: &str) {
        if let Some(n) = self.names.get_mut(idx) {
            *n = name.trim().to_string();
        }
    }

    // ---- pins ---------------------------------------------------------

    /// Bring the pins up to date with the slides: once a frame, and after
    /// anything that pins or unpins objects.
    pub fn refresh_pins(&mut self) {
        self.pins.refresh(&self.canvases);
    }

    /// The pins with objects to show on the active slide, bottom to top.
    pub fn pinned(&self) -> Vec<pins::Pinned<'_>> {
        self.pins.pinned(&self.canvases, self.active)
    }

    /// Move the chosen objects of the active slide's layer `li` to slide
    /// `to`, onto its active layer, where they show now: through `to`'s
    /// own pan and zoom they land on the same spot of the screen, at the
    /// same size. One undo step on each slide. Moved objects leave their
    /// pin, like copies (a pin shows objects of one slide). Returns how
    /// many moved: none to the slide itself, a slide that does not
    /// exist, or a locked one, and none from a locked slide.
    pub fn move_objects_to(&mut self, to: usize, li: usize, strokes: &[usize], shapes: &[usize]) -> usize {
        let from = self.active;
        if to >= self.canvases.len() {
            return 0;
        }
        self.ensure_loaded(from);
        self.ensure_loaded(to);
        let (Some(src), Some(dst)) = (self.canvases[from].as_ref(), self.canvases[to].as_ref()) else { return 0 };
        // Slide `from` point p shows at pan_f + p·zoom_f; slide `to`
        // shows the point (pan_f − pan_t + p·zoom_f) / zoom_t there.
        let k = src.zoom / dst.zoom.max(1e-6);
        let d = [(src.pan[0] - dst.pan[0]) / dst.zoom.max(1e-6), (src.pan[1] - dst.pan[1]) / dst.zoom.max(1e-6)];
        let m = affine::Affine::scale_about([0.0, 0.0], k, k).then(affine::Affine::translate(d));
        self.transfer_objects(from, to, li, strokes, shapes, &m).map_or(0, |placed| placed.strokes.len() + placed.shapes.len())
    }

    /// Objects dragged from one window and let go over the other:
    /// `transfer_objects` when the windows show different slides, or a
    /// move in place (one undo step) when both show the same one. Returns
    /// the dropped objects, to select.
    pub fn drop_objects(&mut self, from: usize, to: usize, li: usize, strokes: &[usize], shapes: &[usize], m: &affine::Affine) -> Option<hit::Selection> {
        if from != to {
            return self.transfer_objects(from, to, li, strokes, shapes, m);
        }
        if self.is_locked(from) || (strokes.is_empty() && shapes.is_empty()) {
            return None;
        }
        self.canvases.get_mut(from)?.as_mut()?.transform_objects(li, strokes, shapes, m);
        Some(hit::Selection { strokes: strokes.to_vec(), shapes: shapes.to_vec() })
    }

    /// Move the chosen objects of slide `from`'s layer `li` to slide
    /// `to`'s active layer, each point taken through `m` (slide `from`'s
    /// units to slide `to`'s): what "Move to slide" and dragging between
    /// the canvas and the slide window do. One undo step on each slide;
    /// moved objects leave their pin. Returns where they landed on `to`,
    /// ready to select; `None` (and nothing moved) when the slides are
    /// the same, either is locked or not read yet, or nothing was chosen.
    pub fn transfer_objects(&mut self, from: usize, to: usize, li: usize, strokes: &[usize], shapes: &[usize], m: &affine::Affine) -> Option<hit::Selection> {
        if from == to || self.is_locked(to) || self.is_locked(from) {
            return None;
        }
        let src = self.canvases.get(from)?.as_ref()?;
        self.canvases.get(to)?.as_ref()?;
        let layer = src.layers.get(li)?;
        let mut moved_strokes: Vec<stroke::Stroke> = strokes.iter().filter_map(|&i| layer.strokes.get(i).cloned()).collect();
        let mut moved_shapes: Vec<shape::Shape> = shapes.iter().filter_map(|&i| layer.shapes.get(i).cloned()).collect();
        if moved_strokes.is_empty() && moved_shapes.is_empty() {
            return None;
        }
        for s in &mut moved_strokes {
            s.apply_affine(m);
            s.pin = 0;
        }
        for s in &mut moved_shapes {
            s.apply_affine(m);
            s.set_pin(0);
        }
        self.canvases[from].as_mut()?.delete_objects(li, strokes, shapes);
        let dst = self.canvases[to].as_mut()?;
        let target = dst.active_layer;
        let placed = dst.insert_objects(target, moved_strokes, moved_shapes);
        self.refresh_pins();
        Some(placed)
    }

    /// The global pins, for the slide window to show in place of the
    /// canvas (see `set_globals_away`).
    pub fn global_pins(&self) -> Vec<pins::Pinned<'_>> {
        self.pins.globals(&self.canvases)
    }

    /// Show the global pins in the slide window instead of on the canvas:
    /// `pinned` then leaves them out. Not an edit; not saved.
    pub fn set_globals_away(&mut self, away: bool) {
        self.pins.set_globals_away(away);
    }

    /// The canvas area is `area` points across: pins show wholly inside
    /// it (see `PinBoard::set_area`). Their kept places do not change.
    pub fn set_pin_area(&mut self, area: [f32; 2]) {
        self.pins.set_area(area);
    }

    /// Some pin has objects to show on the active slide, whether or not
    /// pins are hidden.
    pub fn has_pins(&self) -> bool {
        self.pins.any(self.active)
    }

    pub fn pins_hidden(&self) -> bool {
        self.pins.hidden()
    }

    /// Hide every pin, or show them again (not an edit; not saved).
    pub fn set_pins_hidden(&mut self, hidden: bool) {
        self.pins.set_hidden(hidden);
    }

    /// Pin places, for moving, resizing and turning pins. Not an edit of
    /// any slide.
    pub fn pin_board(&mut self) -> &mut pins::PinBoard {
        &mut self.pins
    }

    /// Pin the chosen objects of the active slide's layer `li` together as
    /// one new pin, placed `offset` screen points from where they show
    /// now; `local` shows it on this slide only. Objects in another pin
    /// leave it. One undo step on the slide. Returns the new pin's id, or
    /// `None` when nothing was chosen.
    pub fn pin_objects(&mut self, li: usize, strokes: &[usize], shapes: &[usize], offset: [f32; 2], local: bool) -> Option<u64> {
        let id = self.pins.fresh_id();
        let c = self.canvases[self.active].as_mut()?;
        let bounds = pins::objects_bounds(c.layers.get(li)?, strokes, shapes)?;
        if !c.set_pin(li, strokes, shapes, id) {
            return None;
        }
        let a = pins::center_of(bounds);
        self.pins.add(pins::Pin {
            id,
            center: [c.pan[0] + a[0] * c.zoom + offset[0], c.pan[1] + a[1] * c.zoom + offset[1]],
            scale: c.zoom,
            angle: 0.0,
            local,
        });
        self.refresh_pins();
        Some(id)
    }

    /// Take the chosen objects of the active slide's layer `li` out of
    /// whatever pin they are in. One undo step; false if none was pinned.
    pub fn unpin_objects(&mut self, li: usize, strokes: &[usize], shapes: &[usize]) -> bool {
        let unpinned = self.active_mut().set_pin(li, strokes, shapes, 0);
        self.refresh_pins();
        unpinned
    }

    /// Take every object out of pin `id`, on whichever slide it is (an
    /// undo step there). The objects stay on their slide. False if the
    /// pin has nothing to show.
    pub fn unpin(&mut self, id: u64) -> bool {
        let Some((slide, members)) = self.pins.members(id) else { return false };
        let members = members.to_vec();
        let Some(c) = self.canvases.get_mut(slide).and_then(Option::as_mut) else { return false };
        let mut layers: Vec<usize> = members.iter().map(|m| m.layer).collect();
        layers.dedup();
        for li in layers {
            let (mut strokes, mut shapes) = (Vec::new(), Vec::new());
            for m in members.iter().filter(|m| m.layer == li) {
                match m.obj {
                    hit::ObjRef::Stroke(i) => strokes.push(i),
                    hit::ObjRef::Shape(i) => shapes.push(i),
                }
            }
            strokes.sort_unstable();
            shapes.sort_unstable();
            c.set_pin(li, &strokes, &shapes, 0);
        }
        self.refresh_pins();
        true
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
        self.open(self.active);
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
            self.names[0].clear();
            self.locks[0] = false;
            self.active = 0;
            return;
        }
        let i = self.active;
        self.canvases.remove(i);
        self.names.remove(i);
        self.locks.remove(i);
        // Every later slide moved down one place in the history too.
        self.recent_walk = None;
        self.recent.retain(|&j| j != i);
        for j in &mut self.recent {
            if *j > i { *j -= 1; }
        }
        if self.active >= self.canvases.len() {
            self.active = self.canvases.len() - 1;
        }
        // Pins are found by slide index, and every later slide just moved.
        self.pins.forget_scans();
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
        // Files first follow a reorder, so what is written next lands
        // in the right place.
        if !self.apply_pending_move() {
            return;
        }
        if let Some(views) = self.take_changed_views() {
            std::thread::spawn(move || {
                if let Err(e) = crate::persistence::save_views(&views) {
                    log::warn!("failed to save slide views: {e}");
                }
            });
        }
        if let Some(pins) = self.pins.take_changed(self.loading.is_none()) {
            std::thread::spawn(move || {
                if let Err(e) = crate::persistence::save_pins(&pins) {
                    log::warn!("failed to save pins: {e}");
                }
            });
        }
        if let Some(names) = self.take_changed_names() {
            std::thread::spawn(move || {
                if let Err(e) = crate::persistence::save_names(&names) {
                    log::warn!("failed to save slide names: {e}");
                }
            });
        }
        if let Some(locks) = self.take_changed_locks() {
            std::thread::spawn(move || {
                if let Err(e) = crate::persistence::save_locks(&locks) {
                    log::warn!("failed to save slide locks: {e}");
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
        // A picture being stored may have its file in place with no slide
        // naming it yet.
        let media = match media_in_use {
            Some(keep) if !self.pictures.busy() && std::mem::take(&mut self.media_cleanup_due) => self.unused_media(keep),
            _ => Vec::new(),
        };
        self.deleting_media = !media.is_empty();
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
        // Should the renames fail, their record finishes them at the next
        // start; the slides below are written all the same, and a file
        // written meanwhile wins over the older copy it parked.
        self.apply_pending_move();
        if let Some(views) = self.take_changed_views() {
            if let Err(e) = crate::persistence::save_views(&views) {
                log::warn!("failed to save slide views: {e}");
            }
        }
        self.refresh_pins();
        if let Some(pins) = self.pins.take_changed(self.loading.is_none()) {
            if let Err(e) = crate::persistence::save_pins(&pins) {
                log::warn!("failed to save pins: {e}");
            }
        }
        if let Some(names) = self.take_changed_names() {
            if let Err(e) = crate::persistence::save_names(&names) {
                log::warn!("failed to save slide names: {e}");
            }
        }
        if let Some(locks) = self.take_changed_locks() {
            if let Err(e) = crate::persistence::save_locks(&locks) {
                log::warn!("failed to save slide locks: {e}");
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

/// Where the slide at `i` ends up when the slide at `from` moves to `to`.
pub fn moved_index(from: usize, to: usize, i: usize) -> usize {
    if i == from {
        to
    } else if from < to && (from + 1..=to).contains(&i) {
        i - 1
    } else if to < from && (to..from).contains(&i) {
        i + 1
    } else {
        i
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
            pin: 0,
            flip: false,
            group: 0,
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

    #[test]
    fn cycling_wraps_both_ways_without_growing_the_deck() {
        let mut m = CanvasManager::for_tests(vec![clean(), clean(), clean()]);
        m.prev();
        assert_eq!(m.active_index(), 2);
        m.cycle_next();
        assert_eq!((m.active_index(), m.count()), (0, 3));
        // `next` off the end still starts a new slide.
        m.go_to(2);
        m.next();
        assert_eq!((m.active_index(), m.count()), (3, 4));
    }

    #[test]
    fn recently_used_walk_flips_with_one_tap_and_goes_deeper_while_held() {
        let mut m = CanvasManager::for_tests((0..6).map(|_| clean()).collect());
        // Before a second slide is visited it steps by number.
        m.walk_recent(true);
        m.end_recent_walk();
        m.note_visit();
        assert_eq!(m.active_index(), 1);
        // Visit 3, then 5 (one frame each).
        for i in [3, 5] {
            m.go_to(i);
            m.note_visit();
        }
        // Tap: back to 3; tap again: back to 5.
        m.walk_recent(true);
        m.end_recent_walk();
        assert_eq!(m.active_index(), 3);
        m.walk_recent(true);
        m.end_recent_walk();
        assert_eq!(m.active_index(), 5);
        // Held: 3, then 1, then 0 — frames in between change nothing.
        m.walk_recent(true);
        m.note_visit();
        m.walk_recent(true);
        assert_eq!(m.active_index(), 1);
        m.walk_recent(true);
        assert_eq!(m.active_index(), 0);
        // Shift steps forward again.
        m.walk_recent(false);
        assert_eq!(m.active_index(), 1);
        m.end_recent_walk();
        assert_eq!(m.recent, [1, 5, 3, 0], "where the walk stopped is now the most recent");

        // Deleting a slide drops it and renumbers the rest.
        m.go_to(3);
        m.note_visit();
        m.delete_active();
        assert_eq!(m.recent, [1, 4, 0]);
    }

    #[test]
    fn locks_follow_their_slides_and_are_not_edits() {
        let mut m = CanvasManager::for_tests(vec![clean(), clean(), clean()]);
        assert_eq!(m.take_changed_locks(), None);
        m.set_locked(1, true);
        assert!(m.is_locked(1) && !m.is_locked(0));
        assert!(m.canvases.iter().flatten().all(|c| !c.dirty), "locking is not an edit");
        assert_eq!(m.take_changed_locks(), Some(vec![false, true, false]));
        assert_eq!(m.take_changed_locks(), None, "written once, then quiet until the next change");
        m.go_to(1);
        assert!(!m.active_editable());
        // It still pans and zooms.
        *m.active_view_mut().1 = 2.0;
        assert_eq!(m.active().zoom, 2.0);
        assert!(!m.active().dirty);

        // New slides come unlocked; a deleted slide takes its lock along.
        m.push_canvas(clean());
        m.go_to(0);
        m.delete_active();
        assert_eq!((m.is_locked(0), m.is_locked(1), m.is_locked(2)), (true, false, false));
        m.go_to(2);
        m.next();
        assert_eq!((m.count(), m.is_locked(3)), (4, false));
    }

    #[test]
    fn names_follow_their_slides() {
        let mut m = CanvasManager::for_tests(vec![clean(), clean(), clean()]);
        assert_eq!(m.take_changed_names(), None);
        m.rename(1, "  Intro  ");
        m.rename(2, "Outro");
        assert_eq!(m.name(1), "Intro", "surrounding spaces dropped");
        assert!(m.canvases.iter().flatten().all(|c| !c.dirty), "naming is not an edit");
        assert_eq!(m.take_changed_names(), Some(vec!["".into(), "Intro".into(), "Outro".into()]));
        assert_eq!(m.take_changed_names(), None, "written once, then quiet until the next change");

        // New slides come unnamed; a deleted slide takes its name along.
        m.push_canvas(clean());
        m.go_to(1);
        m.delete_active();
        assert_eq!((m.name(0), m.name(1), m.name(2)), ("", "Outro", ""));
        m.go_to(2);
        m.next();
        assert_eq!((m.count(), m.name(3)), (4, ""));

        // Deleting the only slide clears it, name included.
        let mut one = CanvasManager::for_tests(vec![clean()]);
        one.rename(0, "Only");
        one.delete_active();
        assert_eq!((one.count(), one.name(0)), (1, ""));
    }

    /// Slides told apart by their pan.
    fn tagged(n: usize) -> CanvasManager {
        let mut m = CanvasManager::for_tests((0..n).map(|i| {
            let mut c = clean();
            c.pan = [i as f32, 0.0];
            c
        }).collect());
        for i in 0..n {
            m.rename(i, &format!("s{i}"));
        }
        m.take_changed_names();
        m
    }

    fn tags(m: &CanvasManager) -> Vec<usize> {
        m.canvases.iter().flatten().map(|c| c.pan[0] as usize).collect()
    }

    #[test]
    fn a_moved_slide_takes_its_name_and_lock_and_the_one_shown_stays_shown() {
        let mut m = tagged(5);
        m.set_locked(1, true);
        m.go_to(3);
        m.note_visit();
        m.go_to(1);
        m.note_visit();
        // Slide 2 (index 1) to the end, then the last to the front.
        m.move_slide(1, 4).unwrap();
        assert_eq!(tags(&m), [0, 2, 3, 4, 1]);
        assert_eq!((m.name(4), m.is_locked(4), m.is_locked(1)), ("s1", true, false));
        assert_eq!(m.active_index(), 4, "still showing the slide it showed");
        assert_eq!(m.recent[..2], [4, 2], "recently used follows the slides");
        assert!(m.canvases.iter().flatten().all(|c| !c.dirty), "moving is not an edit of any slide");
        m.move_slide(4, 0).unwrap();
        assert_eq!(tags(&m), [1, 0, 2, 3, 4]);
        assert_eq!(m.active_index(), 0);
        // The files: one rename list from where they were, not two.
        assert_eq!(m.pending_move, Some(vec![1, 0, 2, 3, 4]));
        // Moving it back leaves nothing to rename.
        m.move_slide(0, 1).unwrap();
        assert_eq!(tags(&m), [0, 1, 2, 3, 4]);
        assert_eq!(m.pending_move, None);
    }

    #[test]
    fn slides_added_after_a_move_fold_into_it_and_a_fresh_delete_holds_moves_back() {
        let mut m = tagged(3);
        m.move_slide(0, 2).unwrap();
        m.push_canvas(clean());
        m.move_slide(3, 0).unwrap();
        // The new slide (no file yet, slot 3) first, then 1 2 0.
        assert_eq!(m.pending_move, Some(vec![3, 1, 2, 0]));
        assert_eq!(moved_index(1, 3, 0), 0);
        assert_eq!(moved_index(1, 3, 1), 3);
        assert_eq!(moved_index(1, 3, 3), 2);
        assert_eq!(moved_index(3, 1, 1), 2);

        let mut m = tagged(3);
        m.go_to(1);
        m.delete_active();
        assert!(m.move_slide(0, 1).is_err(), "the delete is not on disk yet");
        assert_eq!(tags(&m), [0, 2]);
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
        assert!(!dir.join("names.ron").exists(), "nothing renamed, nothing to write");
        assert!(!dir.join("locks.ron").exists(), "nothing locked, nothing to write");

        // Looking around a slide, or naming one, writes only the views
        // and names, which the next start picks up.
        let slide_0 = std::fs::read_to_string(dir.join("canvas_0.ron")).unwrap();
        *m.active_view_mut().1 = 4.0;
        m.rename(3, "Last");
        m.set_locked(0, true);
        m.save_all_blocking();
        assert_eq!(std::fs::read_to_string(dir.join("canvas_0.ron")).unwrap(), slide_0);
        let m = CanvasManager::load_or_default(&AppConfig::default());
        assert_eq!(m.active().zoom, 4.0);
        assert_eq!((m.name(0), m.name(3)), ("", "Last"));
        assert!(m.is_locked(0) && !m.is_locked(3), "locks come back with their slides");

        slides_still_loading_are_never_written(&dir);
        moved_slides_are_renamed_not_rewritten(&dir);

        std::env::remove_var("OSN_DATA_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Take in slides as the app does, once a frame, until the active one
    /// is in.
    fn until_open(m: &mut CanvasManager) {
        let t = Instant::now();
        while m.opening().is_some() {
            assert!(t.elapsed() < std::time::Duration::from_secs(10), "the slide never opened");
            m.poll_loads();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Moving a slide renames the files at the next save and rewrites
    /// none; the order, names and views are there on the next start. Runs
    /// inside the test above, which owns `OSN_DATA_DIR`.
    fn moved_slides_are_renamed_not_rewritten(dir: &std::path::Path) {
        for e in std::fs::read_dir(dir).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
        }
        for i in 0..3 {
            let mut c = Canvas::default();
            c.pan = [i as f32, 0.0];
            std::fs::write(dir.join(format!("canvas_{i}.ron")), crate::persistence::serialize_canvas(&c).unwrap()).unwrap();
        }
        let modified = |i: usize| std::fs::metadata(dir.join(format!("canvas_{i}.ron"))).unwrap().modified().unwrap();
        let before: Vec<_> = (0..3).map(modified).collect();

        let mut m = CanvasManager::load_or_default(&AppConfig::default());
        m.rename(0, "First");
        m.move_slide(0, 2).unwrap();
        m.save_all(Some(&HashSet::new()));
        while m.saving.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(m.pending_move.is_none());
        let pan = |i: usize| crate::persistence::parse_canvas(&std::fs::read_to_string(dir.join(format!("canvas_{i}.ron"))).unwrap(), 3).unwrap().pan[0];
        assert_eq!((pan(0), pan(1), pan(2)), (1.0, 2.0, 0.0));
        // Renamed, not rewritten: each file is the one it was.
        assert_eq!([modified(0), modified(1), modified(2)], [before[1], before[2], before[0]]);
        assert!(!dir.join("moving.ron").exists());

        let m = CanvasManager::load_or_default(&AppConfig::default());
        assert_eq!((m.name(0), m.name(2)), ("", "First"));
        assert_eq!(m.slide(2).unwrap().pan, [0.0, 0.0]);
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

        // Back from the first slide wraps to the last, which opens then,
        // without holding up the caller.
        let mut wrap = CanvasManager::start_loading_with(&AppConfig::default(), 0);
        wrap.prev();
        assert_eq!(wrap.active_index(), 3);
        assert!(wrap.opening().is_some(), "still being read");
        assert!(!wrap.active_editable(), "nothing can be drawn on it yet");
        until_open(&mut wrap);
        assert_eq!(wrap.active().pan, [13.0, 0.0]);
        assert!(wrap.active_editable());

        // A slide can be named before it is read.
        m.rename(3, "Four");
        assert_eq!(file(3), three, "naming is not an edit of the slide");

        // Delete slide 2: slides 3 and 4 are read first, then all shift.
        m.next();
        until_open(&mut m);
        assert_eq!(m.active().pan, [11.0, 0.0]);
        m.delete_active();
        assert_eq!(m.count(), 3);
        m.save_all_blocking();
        assert_eq!(pan(0), [20.0, 0.0]);
        assert_eq!(pan(1), Canvas::default().pan, "the gap is now slot 1");
        assert_eq!(pan(2), [13.0, 0.0], "the last slide moved down, not lost");
        assert!(!dir.join("canvas_3.ron").exists());
        assert_eq!(crate::persistence::load_names(), ["", "", "Four"], "its name moved down with it");
        assert!(m.take_load_errors().is_empty());
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;
    use crate::canvas::shape::{BorderStyle, Shape};
    use crate::canvas::stroke::{PenSample, Stroke};

    fn line(x: f32) -> Shape {
        Shape::Line { a: [x, 0.0], b: [x + 10.0, 0.0], color: [255; 4], stroke_width: 4.0, border: BorderStyle::Solid, pin: 0, group: 0 }
    }

    fn stroke(x: f32) -> Stroke {
        let mut s = Stroke::with_style([255; 4], 3.0, Default::default());
        s.push(PenSample::from_pos([x, 5.0]));
        s.push(PenSample::from_pos([x + 20.0, 5.0]));
        s
    }

    fn slide() -> Canvas {
        let mut c = Canvas::default();
        c.layers[0].shapes.extend([line(0.0), line(100.0)]);
        c.layers[0].strokes.push(stroke(0.0));
        c.dirty = false;
        c
    }

    #[test]
    fn pinning_is_one_undo_step_on_its_slide_and_shows_everywhere() {
        let mut m = CanvasManager::for_tests(vec![slide(), slide()]);
        let id = m.pin_objects(0, &[0], &[1], [24.0, 24.0], false).expect("pinned");
        assert!(m.active().dirty, "the tags are part of the slide");
        assert_eq!(m.active().layers[0].shapes[1].pin(), id);
        assert_eq!(m.active().layers[0].strokes[0].pin, id);
        assert_eq!(m.active().layers[0].shapes[0].pin(), 0);
        let pinned = m.pinned();
        assert_eq!(pinned.len(), 1);
        assert_eq!((pinned[0].pin.id, pinned[0].slide, pinned[0].members.len()), (id, 0, 2));
        drop(pinned);

        // Seen from the other slide too, and moving it there is no edit.
        m.next();
        m.pin_board().get_mut(id).unwrap().center = [500.0, 500.0];
        assert!(!m.active().dirty);
        assert_eq!(m.pinned().len(), 1);

        // Undo on its slide takes the objects out; redo puts them back,
        // where the pin was left.
        m.prev();
        m.active_mut().undo();
        m.refresh_pins();
        assert!(m.pinned().is_empty());
        m.active_mut().redo();
        m.refresh_pins();
        assert_eq!(m.pinned()[0].pin.center, [500.0, 500.0]);
    }

    #[test]
    fn moving_objects_to_another_slide_keeps_them_on_the_same_spot() {
        let mut a = slide();
        a.pan = [100.0, 50.0];
        a.zoom = 2.0;
        let mut b = slide();
        b.pan = [-40.0, 10.0];
        b.zoom = 0.5;
        let mut m = CanvasManager::for_tests(vec![a, b]);
        m.pin_objects(0, &[0], &[1], [0.0, 0.0], false).expect("pinned");
        let screen = |c: &Canvas, p: [f32; 2]| [c.pan[0] + p[0] * c.zoom, c.pan[1] + p[1] * c.zoom];
        let Shape::Line { a: start, .. } = m.active().layers[0].shapes[1] else { panic!("a line") };
        let before = screen(m.active(), start);
        let undo_a = m.active().edit_stamp().0;

        assert_eq!(m.move_objects_to(1, 0, &[0], &[1]), 2);
        let a = m.slide(0).unwrap();
        assert_eq!((a.layers[0].strokes.len(), a.layers[0].shapes.len()), (0, 1), "gone from slide 1");
        assert_eq!(a.edit_stamp().0, undo_a + 1, "one undo step there");
        let b = m.slide(1).unwrap();
        assert_eq!((b.layers[0].strokes.len(), b.layers[0].shapes.len()), (2, 3), "added to slide 2");
        let moved = &b.layers[0].shapes[2];
        let Shape::Line { a: start, .. } = moved else { panic!("a line") };
        let after = screen(b, *start);
        assert!((before[0] - after[0]).abs() < 1e-3 && (before[1] - after[1]).abs() < 1e-3, "same spot on screen");
        assert_eq!(moved.pin(), 0, "out of its pin");
        assert!(b.dirty);

        // Not onto itself, nor to or from a locked slide.
        assert_eq!(m.move_objects_to(0, 0, &[], &[0]), 0);
        m.set_locked(1, true);
        assert_eq!(m.move_objects_to(1, 0, &[], &[0]), 0);
    }

    #[test]
    fn pins_show_inside_the_canvas_area_without_losing_their_place() {
        let mut m = CanvasManager::for_tests(vec![slide()]);
        let id = m.pin_objects(0, &[0], &[], [0.0, 0.0], false).expect("pinned");
        // Put it half past the right edge of an 800 × 600 area.
        m.pin_board().get_mut(id).unwrap().center = [790.0, 300.0];
        m.set_pin_area([800.0, 600.0]);
        let shown = m.pinned()[0];
        let [x0, y0, x1, y1] = shown.frame().aabb();
        assert!(x0 >= -1e-3 && y0 >= -1e-3 && x1 <= 800.0 + 1e-3 && y1 <= 600.0 + 1e-3, "wholly inside: {:?}", [x0, y0, x1, y1]);
        assert_eq!(m.pin_board().get_mut(id).unwrap().center, [790.0, 300.0], "its own place is kept");

        // A bigger window shows it where it was put.
        m.set_pin_area([2000.0, 1200.0]);
        assert_eq!(m.pinned()[0].pin.center, [790.0, 300.0]);

        // Too big for the area: its top-left corner shows.
        m.pin_board().get_mut(id).unwrap().scale = 100.0;
        m.set_pin_area([300.0, 200.0]);
        let [x0, y0, _, _] = m.pinned()[0].frame().aabb();
        assert!(x0.abs() < 1e-3 && y0.abs() < 1e-3, "from its top-left: {:?}", [x0, y0]);
    }

    #[test]
    fn global_pins_can_move_to_the_slide_window_and_back() {
        let mut m = CanvasManager::for_tests(vec![slide(), slide()]);
        let global = m.pin_objects(0, &[0], &[], [0.0, 0.0], false).expect("pinned");
        let local = m.pin_objects(0, &[], &[1], [0.0, 0.0], true).expect("pinned");
        let ids = |v: Vec<pins::Pinned>| v.iter().map(|p| p.pin.id).collect::<Vec<_>>();
        assert_eq!(ids(m.pinned()), [global, local]);

        m.set_globals_away(true);
        assert_eq!(ids(m.pinned()), [local], "the canvas keeps its local pin only");
        assert_eq!(ids(m.global_pins()), [global], "the window gets the global one");
        m.next();
        assert!(m.pinned().is_empty(), "on another slide nothing is left on the canvas");
        assert_eq!(ids(m.global_pins()), [global], "the window shows it whatever the slide");

        m.set_pins_hidden(true);
        assert!(m.global_pins().is_empty(), "hiding pins hides them in the window too");
        m.set_pins_hidden(false);
        m.set_globals_away(false);
        assert_eq!(ids(m.pinned()), [global], "back on the canvas");
    }

    #[test]
    fn a_local_pin_stays_on_its_slide_until_made_global() {
        let mut m = CanvasManager::for_tests(vec![slide(), slide()]);
        let id = m.pin_objects(0, &[], &[0], [0.0, 0.0], true).unwrap();
        assert_eq!(m.pinned().len(), 1);
        m.next();
        assert!(m.pinned().is_empty() && !m.has_pins(), "not on the other slide");
        m.prev();
        m.pin_board().get_mut(id).unwrap().local = false;
        m.next();
        assert_eq!(m.pinned().len(), 1);
        assert!(!m.active().dirty, "scope is the pin's, not a slide edit");
    }

    #[test]
    fn unpinning_from_another_slide_leaves_the_objects_where_they_are() {
        let mut m = CanvasManager::for_tests(vec![slide(), slide()]);
        let id = m.pin_objects(0, &[0], &[0, 1], [0.0, 0.0], false).unwrap();
        let before = format!("{:?}", m.active().layers[0].shapes.iter().map(Shape::obb).collect::<Vec<_>>());
        m.next();
        assert!(m.unpin(id));
        assert!(m.pinned().is_empty());
        m.prev();
        let c = m.active();
        assert!(c.layers[0].shapes.iter().all(|s| s.pin() == 0) && c.layers[0].strokes[0].pin == 0);
        assert_eq!(format!("{:?}", c.layers[0].shapes.iter().map(Shape::obb).collect::<Vec<_>>()), before);
        // The unpin is an undo step on the objects' slide.
        m.active_mut().undo();
        m.refresh_pins();
        assert_eq!(m.pinned().len(), 1);
    }

    #[test]
    fn copies_are_in_no_pin_and_a_deleted_slide_takes_its_pins() {
        let mut m = CanvasManager::for_tests(vec![slide(), slide(), slide()]);
        let id = m.pin_objects(0, &[], &[0], [0.0, 0.0], false).unwrap();
        let shapes = m.active().layers[0].shapes.clone();
        let (copy, _) = Canvas::with_objects(Vec::new(), shapes, [0.0, 0.0], 1.0);
        assert!(copy.layers[0].shapes.iter().all(|s| s.pin() == 0));

        // Pins of slide 3 survive slide 2 going; slide 1's go with it.
        m.go_to(2);
        let later = m.pin_objects(0, &[], &[1], [0.0, 0.0], false).unwrap();
        m.go_to(0);
        m.delete_active();
        m.refresh_pins();
        let pinned = m.pinned();
        assert_eq!(pinned.iter().map(|p| (p.pin.id, p.slide)).collect::<Vec<_>>(), vec![(later, 1)]);
        assert!(!pinned.iter().any(|p| p.pin.id == id));
    }

    #[test]
    fn unpinned_objects_save_exactly_as_before() {
        let c = slide();
        let text = crate::persistence::serialize_canvas(&c).unwrap();
        assert!(!text.contains("pin"), "no pin field written for unpinned objects");
        let mut pinned = slide();
        pinned.set_pin(0, &[0], &[0], 42);
        let text = crate::persistence::serialize_canvas(&pinned).unwrap();
        let back = crate::persistence::parse_canvas(&text, 3).unwrap();
        assert_eq!((back.layers[0].shapes[0].pin(), back.layers[0].strokes[0].pin), (42, 42));
        assert_eq!(back.layers[0].shapes[1].pin(), 0);
    }

    #[test]
    fn screenshots_draw_pins_where_the_overlay_does() {
        let mut c = Canvas::default();
        // A thick horizontal line, pinned and turned a quarter: on
        // screen it stands upright through (50, 40..60).
        c.layers[0].shapes.push(line(0.0));
        let mut m = CanvasManager::for_tests(vec![c]);
        let id = m.pin_objects(0, &[], &[0], [0.0, 0.0], false).unwrap();
        {
            let pin = m.pin_board().get_mut(id).unwrap();
            pin.center = [50.0, 50.0];
            pin.scale = 2.0;
            pin.angle = std::f32::consts::FRAC_PI_2;
        }
        let mut pixmap = tiny_skia::Pixmap::new(100, 100).unwrap();
        crate::canvas::render::render_pins(&mut pixmap.as_mut(), &m.pinned(), 1.0, None);
        let alpha = |x: u32, y: u32| pixmap.pixel(x, y).unwrap().alpha();
        assert!(alpha(50, 44) > 0 && alpha(50, 56) > 0, "drawn upright");
        assert_eq!(alpha(42, 50), 0, "not where the unturned line would be");
    }

    #[test]
    fn a_faded_or_hidden_layer_leaves_its_pins_showing() {
        use crate::canvas::shape::RasterKind;
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut c = Canvas::default();
        c.layers[0].shapes.push(Shape::Raster {
            pos: [0.0, 0.0], size: [20.0, 20.0], png: std::sync::Arc::new(png.into_inner()), px_size: [4, 4],
            color: [0; 4], rotation: 0.0, kind: RasterKind::Image, media: None, picture: None, pin: 0,
            flip: false,
            group: 0,
        });
        c.layers[0].opacity = 0.25;
        let mut m = CanvasManager::for_tests(vec![c]);
        let id = m.pin_objects(0, &[], &[0], [0.0, 0.0], false).unwrap();
        m.pin_board().get_mut(id).unwrap().center = [50.0, 50.0];

        // The slide itself draws the picture faded…
        let mut slide = tiny_skia::Pixmap::new(100, 100).unwrap();
        crate::canvas::render::render_canvas(&mut slide.as_mut(), m.active(), None, None, 1.0, None);
        assert!(slide.pixel(10, 10).unwrap().alpha() < 128);
        // …its pin does not.
        let mut pinned = tiny_skia::Pixmap::new(100, 100).unwrap();
        crate::canvas::render::render_pins(&mut pinned.as_mut(), &m.pinned(), 1.0, None);
        assert_eq!(pinned.pixel(50, 50).unwrap().alpha(), 255);

        // Faded all the way out, and hidden, the pin still shows and can
        // be taken.
        m.active_mut().layers[0].opacity = 0.0;
        m.active_mut().layers[0].visible = false;
        let mut pinned = tiny_skia::Pixmap::new(100, 100).unwrap();
        crate::canvas::render::render_pins(&mut pinned.as_mut(), &m.pinned(), 1.0, None);
        assert_eq!(pinned.pixel(50, 50).unwrap().alpha(), 255);
        let mut tool = crate::canvas::pins::PinTool::default();
        assert_eq!(tool.press([50.0, 50.0], &m.pinned()), crate::canvas::pins::Press::Took);
    }
}
