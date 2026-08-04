//! Persistent storage: reads and writes `AppConfig` and per-canvas files.
//!
//! Strategy:
//!   * One human-readable RON file per "thing" (config, canvas).
//!   * Atomic writes: write to a sibling `*.tmp` file, then rename onto the
//!     real path. On a crash we either keep the old file fully intact or
//!     replace it cleanly — no half-written state.
//!   * We try hard not to crash on a corrupted file; instead we log a warning
//!     and fall back to defaults.

pub mod paths;

use crate::canvas::canvas::Canvas;
use crate::config::AppConfig;
use crate::error::{Context, Result};
use std::path::Path;

/// Read `AppConfig` from disk, or return `Default::default()` if the file
/// is missing or fails to parse. Logs a warning on parse failure so you can
/// debug it.
pub fn load_config() -> AppConfig {
    let path = match paths::config_file() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("could not resolve config path: {e}; using defaults");
            return AppConfig::default();
        }
    };
    if !path.exists() {
        return AppConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => match ron::from_str::<AppConfig>(&s) {
            Ok(cfg) => cfg,
            Err(e) => {
                log::warn!("config.ron parse failed ({e}); using defaults");
                AppConfig::default()
            }
        },
        Err(e) => {
            log::warn!("could not read config.ron: {e}; using defaults");
            AppConfig::default()
        }
    }
}

/// Write `AppConfig` to disk atomically.
pub fn save_config(cfg: &AppConfig) -> Result<()> {
    let path = paths::config_file()?;
    let serialized = ron::ser::to_string_pretty(cfg, ron::ser::PrettyConfig::default())?;
    atomic_write(&path, serialized.as_bytes())
}

/// Save a single canvas. We name files `canvas_<index>.ron` so the user can
/// inspect / hand-edit them if desired.
///
/// Pretty-prints the structure but with `compact_arrays` so the embedded
/// PNG `Vec<u8>` of a freeze-frame layer stays on one line instead of
/// becoming a multi-megabyte one-number-per-line literal. Those bytes
/// were never hand-readable anyway; everything else remains inspectable.
pub fn save_canvas(index: usize, canvas: &Canvas) -> Result<()> {
    let path = paths::canvases_dir()?.join(format!("canvas_{index}.ron"));
    // `compact_arrays` keeps `Vec<u8>` payloads on a single line. It
    // matters enormously here: a frozen-frame layer holds a full-screen
    // PNG inline, and the default pretty printer emits *one decimal
    // number per line* — a newline plus indentation for every byte,
    // inflating a few MB of image into tens of MB of text and making
    // every autosave a multi-hundred-millisecond stall on the UI
    // thread. Structure stays pretty-printed and hand-editable; only
    // the byte blobs collapse. RON parses both layouts, so existing
    // save files keep loading and no migration is needed.
    let cfg = ron::ser::PrettyConfig::default().compact_arrays(true);
    let serialized = ron::ser::to_string_pretty(canvas, cfg)?;
    atomic_write(&path, serialized.as_bytes())
}

/// Load a single canvas if present; returns `None` for first-time launches.
/// `position_passes` controls the smoothing budget baked into each
/// stroke's render cache — passed in from the user's current smoothing
/// level so loaded strokes match what they would draw fresh today.
pub fn load_canvas(index: usize, position_passes: usize) -> Option<Canvas> {
    let path = paths::canvases_dir().ok()?.join(format!("canvas_{index}.ron"));
    if !path.exists() {
        return None;
    }
    let s = std::fs::read_to_string(&path).ok()?;
    match ron::from_str::<Canvas>(&s) {
        Ok(mut c) => {
            // Old canvases stored flat strokes/shapes; fold them into a
            // first layer before anyone else touches `c.layers`.
            c.ensure_migrated();
            // Stroke render caches don't survive serialisation (they're
            // huge and trivially rebuildable). Build them now so the
            // first-paint after launch is fast.
            for layer in &mut c.layers {
                for stroke in &mut layer.strokes {
                    stroke.build_cache_with(position_passes);
                }
            }
            Some(c)
        }
        Err(e) => {
            log::warn!("canvas_{index}.ron parse failed: {e}; treating as empty");
            None
        }
    }
}

/// Atomic write helper: we write to `<path>.tmp` and rename onto `path`.
///
/// Why: `std::fs::write` is not atomic — if the program crashes mid-write
/// you can end up with a truncated file. Using a temp file + rename means
/// the on-disk file is *always* either the old version or the fully-written
/// new version, never an intermediate.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing tmp file {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming tmp into {path:?}"))?;
    Ok(())
}
