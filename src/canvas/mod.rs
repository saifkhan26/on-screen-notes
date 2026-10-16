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
pub mod render;
pub mod shape;
pub mod smoothing;
pub mod stroke;

use crate::canvas::canvas::Canvas;
use crate::config::AppConfig;
use crate::persistence::SlideView;
use std::collections::HashSet;

/// Top-level holder for all canvases.
pub struct CanvasManager {
    canvases: Vec<Canvas>,
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
    /// Every slide's view as `views.ron` last received it, to tell when
    /// the file needs writing again.
    saved_views: Vec<SlideView>,
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

impl CanvasManager {
    /// Build a manager from disk, falling back to `cfg.initial_canvas_count`
    /// blank canvases for first-time launches.
    pub fn load_or_default(cfg: &AppConfig) -> Self {
        use crate::persistence::LoadOutcome;
        let passes = cfg.smoothing_level.position_passes();
        let mut canvases = Vec::new();
        let mut load_errors = Vec::new();
        let mut protected = Vec::new();
        // Walk every slot up to the highest file on disk. A gap or an
        // unreadable file becomes a blank placeholder instead of ending
        // the walk, so one bad slide can never hide the ones after it.
        if let Some(max) = crate::persistence::max_canvas_index() {
            for idx in 0..=max {
                match crate::persistence::load_canvas(idx, passes) {
                    LoadOutcome::Loaded(c) => canvases.push(c),
                    LoadOutcome::Missing => {
                        log::warn!("canvas_{idx}.ron is missing; leaving a blank slide in its place");
                        canvases.push(placeholder_canvas());
                    }
                    LoadOutcome::Unreadable(e) => {
                        log::warn!("canvas_{idx}.ron could not be read: {e}");
                        match crate::persistence::preserve_unreadable(idx) {
                            Ok(kept) => load_errors.push(format!(
                                "Slide {} could not be read — original kept as {}",
                                idx + 1,
                                kept.file_name().and_then(|n| n.to_str()).unwrap_or("a copy"),
                            )),
                            Err(copy_err) => {
                                log::warn!("could not copy canvas_{idx}.ron aside: {copy_err}");
                                protected.push(idx);
                                load_errors.push(format!(
                                    "Slide {} could not be read — its file is left untouched",
                                    idx + 1,
                                ));
                            }
                        }
                        canvases.push(placeholder_canvas());
                    }
                }
            }
        }
        // First-time boot: spin up some empty canvases.
        if canvases.is_empty() {
            for _ in 0..cfg.initial_canvas_count.max(1) {
                canvases.push(Canvas::default());
            }
        }
        apply_views(&mut canvases, &crate::persistence::load_views());
        let saved_views = views_of(&canvases);
        Self {
            canvases,
            active: 0,
            load_errors,
            protected,
            prune_orphans: false,
            media_to_check: HashSet::new(),
            media_cleanup_due: false,
            saved_views,
        }
    }

    /// A manager over `canvases` that has touched no file.
    #[cfg(test)]
    pub(crate) fn for_tests(canvases: Vec<Canvas>) -> Self {
        let saved_views = views_of(&canvases);
        Self {
            canvases,
            active: 0,
            load_errors: Vec::new(),
            protected: Vec::new(),
            prune_orphans: false,
            media_to_check: HashSet::new(),
            media_cleanup_due: false,
            saved_views,
        }
    }

    /// Every slide's view, if any changed since `views.ron` was last
    /// written; they then count as written.
    fn take_changed_views(&mut self) -> Option<Vec<SlideView>> {
        let views = views_of(&self.canvases);
        if views == self.saved_views {
            return None;
        }
        self.saved_views = views.clone();
        Some(views)
    }

    /// Slides that failed to load at startup, one message each. Returns
    /// them once; later calls get an empty list.
    pub fn take_load_errors(&mut self) -> Vec<String> {
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
        let c = &mut self.canvases[self.active];
        c.dirty = true;
        c
    }

    /// The active canvas's pan and zoom, for moving the view. Unlike
    /// `active_mut` this does not flag the canvas for the autosave:
    /// looking around is not an edit, and rewriting a big slide every
    /// few seconds while the user pans and zooms cost seconds of CPU
    /// per save. The autosave writes views to their own small file
    /// instead (`persistence::save_views`).
    pub fn active_view_mut(&mut self) -> (&mut [f32; 2], &mut f32) {
        let c = &mut self.canvases[self.active];
        (&mut c.pan, &mut c.zoom)
    }

    /// Read-only access to the active canvas (used by renderer + screenshot).
    pub fn active(&self) -> &Canvas {
        &self.canvases[self.active]
    }

    /// Index of the active canvas (for status bar display).
    pub fn active_index(&self) -> usize { self.active }

    /// Total canvas count (for status bar display: "2 / 5").
    pub fn count(&self) -> usize { self.canvases.len() }

    /// Rebuild every stroke's render cache with the given position-pass
    /// budget. Called when the user switches smoothing level so the
    /// existing strokes get rerendered with the new look.
    pub fn rebuild_all_caches(&mut self, position_passes: usize) {
        for canvas in &mut self.canvases {
            for layer in &mut canvas.layers {
                for stroke in &mut layer.strokes {
                    stroke.cache = None;
                    stroke.build_cache_with(position_passes);
                }
            }
        }
    }

    /// Move to the next canvas. If we're already on the *last* canvas, a
    /// fresh blank canvas is appended and we move into it. This gives the
    /// "infinite canvases on right-arrow" feel — the user can keep pressing
    /// → to start a new sheet whenever they run out of space.
    pub fn next(&mut self) {
        if self.active + 1 >= self.canvases.len() {
            // At the end → grow a new blank canvas.
            self.canvases.push(Canvas::default());
        }
        self.active += 1;
    }

    /// Append a ready-made canvas (an opened PureRef board) as the last
    /// slide and move into it. Returns its index. The canvas keeps its
    /// own `dirty` flag; a `Canvas::default()`-built one is dirty, so the
    /// next autosave writes it.
    pub fn push_canvas(&mut self, canvas: Canvas) -> usize {
        self.canvases.push(canvas);
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
    }

    /// Delete the active canvas.
    /// * If only one canvas exists we *clear* it instead of deleting (keep
    ///   the user always on at least one sheet).
    /// * Otherwise we remove it and shift the active index back by one
    ///   (or stay at 0 if we just removed canvas 0).
    pub fn delete_active(&mut self) {
        // Deleting shifts every later slide down a file slot. With a
        // protected slot in play that shift would point the "never write
        // this index" guard at the wrong file, so refuse instead.
        if !self.protected.is_empty() {
            log::warn!("delete canvas ignored: an unreadable slide file is being kept in place");
            return;
        }
        // Its videos and GIFs become candidates for deletion; the next
        // save decides which are really unused.
        self.canvases[self.active].media_files(&mut self.media_to_check);
        self.media_cleanup_due = true;
        if self.canvases.len() == 1 {
            self.canvases[0] = Canvas::default();
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
        for c in &mut self.canvases {
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
        let mut dirty: Vec<(usize, Canvas)> = Vec::new();
        for (i, c) in self.canvases.iter_mut().enumerate() {
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
        std::thread::spawn(move || {
            for (i, c) in dirty {
                if let Err(e) = crate::persistence::save_canvas(i, &c) {
                    log::warn!("failed to save canvas {i}: {e}");
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
        let mut used = HashSet::new();
        for c in &self.canvases {
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
        if let Some(views) = self.take_changed_views() {
            if let Err(e) = crate::persistence::save_views(&views) {
                log::warn!("failed to save slide views: {e}");
            }
        }
        for (i, c) in self.canvases.iter_mut().enumerate() {
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

fn views_of(canvases: &[Canvas]) -> Vec<SlideView> {
    canvases.iter().map(|c| SlideView { pan: c.pan, zoom: c.zoom }).collect()
}

/// Give each slide the view `views.ron` has for it. Anything that is not
/// a usable view (a damaged or hand-edited file) is skipped, leaving the
/// view stored in the slide's own file.
fn apply_views(canvases: &mut [Canvas], views: &[SlideView]) {
    use crate::interaction::zoom_pan::{MAX_ZOOM, MIN_ZOOM};
    for (c, v) in canvases.iter_mut().zip(views) {
        let usable = v.pan.iter().all(|p| p.is_finite()) && (MIN_ZOOM..=MAX_ZOOM).contains(&v.zoom);
        if usable {
            c.pan = v.pan;
            c.zoom = v.zoom;
        }
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

        std::env::remove_var("OSN_DATA_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}
