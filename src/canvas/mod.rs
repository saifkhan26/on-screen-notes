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
    pub fn active_mut(&mut self) -> &mut Canvas {
        &mut self.canvases[self.active]
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
        // Best-effort: also remove the on-disk file for the removed slot,
        // and shift down everything past it. Simplest is to re-save all,
        // which `save_all` does on the next debounced tick.
    }

    /// Save every canvas to disk and remove any orphaned files left behind
    /// by deletes (e.g. user used to have 5 canvases, deleted two, now we
    /// only persist 3 — files canvas_3.ron and canvas_4.ron from the old
    /// state would be stale and need cleaning up).
    pub fn save_all(&self) {
        for (i, c) in self.canvases.iter().enumerate() {
            if let Err(e) = crate::persistence::save_canvas(i, c) {
                log::warn!("failed to save canvas {i}: {e}");
            }
        }
        // Clean up files past current count. We try indices N..N+10 — if a
        // gap appears the user must have manually messed with the dir.
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
