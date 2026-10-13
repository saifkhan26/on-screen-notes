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
        Self { canvases, active: 0, load_errors, protected, prune_orphans: false }
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
    /// past `canvases.len()` so the deleted slot doesn't linger.
    pub fn save_all(&mut self) {
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
        std::thread::spawn(move || {
            for (i, c) in dirty {
                if let Err(e) = crate::persistence::save_canvas(i, &c) {
                    log::warn!("failed to save canvas {i}: {e}");
                }
            }
            if prune {
                remove_stale_canvas_files(count);
            }
        });
    }

    /// Synchronous variant used at app shutdown — blocks until every
    /// dirty canvas hits disk so a forced kill after `on_exit` can't
    /// drop edits.
    pub fn save_all_blocking(&mut self) {
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

        std::env::remove_var("OSN_DATA_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}
