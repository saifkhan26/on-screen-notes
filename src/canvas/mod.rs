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

pub mod canvas;
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
}

impl CanvasManager {
    /// Build a manager from disk, falling back to `cfg.initial_canvas_count`
    /// blank canvases for first-time launches.
    pub fn load_or_default(cfg: &AppConfig) -> Self {
        let passes = cfg.smoothing_level.position_passes();
        let mut canvases = Vec::new();
        // We try indices 0..N until we hit a missing file; that bounds the
        // number of canvases the user can have at startup. Saving a new
        // canvas later just bumps the count.
        let mut idx = 0;
        loop {
            match crate::persistence::load_canvas(idx, passes) {
                Some(c) => {
                    canvases.push(c);
                    idx += 1;
                }
                None => break,
            }
        }
        // First-time boot: spin up some empty canvases.
        if canvases.is_empty() {
            for _ in 0..cfg.initial_canvas_count.max(1) {
                canvases.push(Canvas::default());
            }
        }
        Self { canvases, active: 0 }
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
    /// Also cleans up orphaned files past `canvases.len()` so a deleted
    /// canvas slot doesn't linger.
    pub fn save_all(&mut self) {
        // Collect dirty canvases as (index, clone). We need to clone
        // because the worker thread owns them while serialising — the
        // UI thread keeps drawing meanwhile. A clone of a PNG-bearing
        // canvas is cheap relative to RON serialisation because it is
        // just an `Arc`-less deep copy of bytes; serialisation
        // formats those bytes into ASCII which is 5-10× larger.
        let mut dirty: Vec<(usize, Canvas)> = Vec::new();
        for (i, c) in self.canvases.iter_mut().enumerate() {
            if c.dirty {
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
        std::thread::spawn(move || {
            for (i, c) in dirty {
                if let Err(e) = crate::persistence::save_canvas(i, &c) {
                    log::warn!("failed to save canvas {i}: {e}");
                }
            }
            // Clean up stale files past the current count.
            if let Ok(dir) = crate::persistence::paths::canvases_dir() {
                for extra in count..(count + 10) {
                    let p = dir.join(format!("canvas_{extra}.ron"));
                    if p.exists() {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        });
    }

    /// Synchronous variant used at app shutdown — blocks until every
    /// dirty canvas hits disk so a forced kill after `on_exit` can't
    /// drop edits.
    pub fn save_all_blocking(&mut self) {
        for (i, c) in self.canvases.iter_mut().enumerate() {
            if !c.dirty { continue; }
            if let Err(e) = crate::persistence::save_canvas(i, c) {
                log::warn!("failed to save canvas {i}: {e}");
            } else {
                c.dirty = false;
            }
        }
        if let Ok(dir) = crate::persistence::paths::canvases_dir() {
            for extra in self.canvases.len()..(self.canvases.len() + 10) {
                let p = dir.join(format!("canvas_{extra}.ron"));
                if p.exists() {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
}
