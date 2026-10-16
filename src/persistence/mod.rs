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
    let serialized = serialize_canvas(canvas)?;
    atomic_write(&path, serialized.as_bytes())
}

/// The exact text `save_canvas` writes. Split out so tests can round-trip
/// a canvas without touching the disk.
pub(crate) fn serialize_canvas(canvas: &Canvas) -> Result<String> {
    let cfg = ron::ser::PrettyConfig::default().compact_arrays(true);
    Ok(ron::ser::to_string_pretty(canvas, cfg)?)
}

/// What `load_canvas` found in a canvas slot.
///
/// "No file" and "a file we could not read" are kept apart on purpose:
/// the first is a normal end-of-list / first launch, the second is the
/// user's work in a state we failed to understand. Folding them together
/// (as an `Option` did) let one bad file look like the end of the list,
/// and the save path then treated every later slide as an orphan.
pub enum LoadOutcome {
    Loaded(Canvas),
    Missing,
    Unreadable(String),
}

/// Highest `N` for which a `canvas_N.ron` exists in the canvases folder,
/// or `None` when there are none. The loader walks `0..=N` rather than
/// stopping at the first gap, so a missing or unreadable slide in the
/// middle cannot hide the ones after it.
pub fn max_canvas_index() -> Option<usize> {
    let dir = paths::canvases_dir().ok()?;
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_prefix("canvas_")?
                .strip_suffix(".ron")?
                .parse::<usize>()
                .ok()
        })
        .max()
}

/// Copy an unreadable canvas file aside so a later save into the same
/// slot cannot overwrite the only copy of whatever is in it. Returns the
/// copy's path. Best-effort: a failure here is logged by the caller and
/// the original file is still left in place until something saves over
/// that slot.
pub fn preserve_unreadable(index: usize) -> Result<std::path::PathBuf> {
    let dir = paths::canvases_dir()?;
    let src = dir.join(format!("canvas_{index}.ron"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst = dir.join(format!("canvas_{index}.ron.unreadable-{stamp}"));
    std::fs::copy(&src, &dst).with_context(|| format!("copying {src:?} aside"))?;
    Ok(dst)
}

/// Load a single canvas slot. `position_passes` controls the smoothing
/// budget baked into each stroke's render cache — passed in from the
/// user's current smoothing level so loaded strokes match what they
/// would draw fresh today.
pub fn load_canvas(index: usize, position_passes: usize) -> LoadOutcome {
    let path = match paths::canvases_dir() {
        Ok(d) => d.join(format!("canvas_{index}.ron")),
        Err(e) => return LoadOutcome::Unreadable(e.to_string()),
    };
    if !path.exists() {
        return LoadOutcome::Missing;
    }
    let s = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return LoadOutcome::Unreadable(e.to_string()),
    };
    match parse_canvas(&s, position_passes) {
        Ok(c) => LoadOutcome::Loaded(c),
        Err(e) => LoadOutcome::Unreadable(e),
    }
}

/// Parse one canvas file's text and bring it to the in-memory shape the
/// rest of the app expects (layers migrated, stroke caches built).
pub(crate) fn parse_canvas(s: &str, position_passes: usize) -> std::result::Result<Canvas, String> {
    let mut c = ron::from_str::<Canvas>(s).map_err(|e| e.to_string())?;
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
    Ok(c)
}

/// One slide's pan and zoom, as kept in `views.ron`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlideView {
    pub pan: [f32; 2],
    pub zoom: f32,
}

/// Write every slide's view, in slide order, to `canvases/views.ron`.
///
/// Panning and zooming are not edits, so they no longer mark a slide
/// for the autosave — rewriting a big slide just to record where the
/// user looked cost seconds of CPU per save. The views go here instead:
/// a few bytes per slide, written only when one changes. Each
/// `canvas_N.ron` still carries the view it was last saved with, so the
/// files read fine without this one.
pub fn save_views(views: &[SlideView]) -> Result<()> {
    let path = paths::canvases_dir()?.join("views.ron");
    atomic_write(&path, ron::ser::to_string(views)?.as_bytes())
}

/// The views `save_views` wrote, or none if the file is missing or
/// unreadable (each slide then keeps the view stored in its own file).
pub fn load_views() -> Vec<SlideView> {
    let Ok(path) = paths::canvases_dir().map(|d| d.join("views.ron")) else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&path) else { return Vec::new() };
    ron::from_str(&s).unwrap_or_else(|e| {
        log::warn!("views.ron could not be read ({e}); keeping each slide's saved view");
        Vec::new()
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap fingerprint of everything a slide holds that must survive
    /// a load → save → load cycle.
    fn summary(c: &Canvas) -> Vec<String> {
        let mut out = vec![format!("pan={:?} zoom={} active={}", c.pan, c.zoom, c.active_layer)];
        for l in &c.layers {
            let samples: usize = l.strokes.iter().map(|s| s.samples.len()).sum();
            let raster_bytes: usize = l
                .shapes
                .iter()
                .map(|s| match s {
                    crate::canvas::shape::Shape::Raster { png, .. } => png.len(),
                    _ => 0,
                })
                .sum();
            out.push(format!(
                "{:?} visible={} opacity={} strokes={} samples={} shapes={} raster_bytes={} image_bytes={} moved={:?} scaled={}",
                l.name, l.visible, l.opacity, l.strokes.len(), samples, l.shapes.len(),
                raster_bytes, l.image.as_ref().map_or(0, |i| i.png.len()), l.moved_by, l.scaled_by,
            ));
        }
        out
    }

    /// Diagnostic over a real data folder: per slide, where the objects
    /// live (which layer, plate image or not), and whether every stroke
    /// is found by the Select tool's hit-test at one of its own points.
    ///
    ///   OSN_TEST_SLIDES_DIR=<copy>/canvases cargo test real_slides_layout -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_slides_layout() {
        let dir = std::path::PathBuf::from(std::env::var_os("OSN_TEST_SLIDES_DIR").expect("set OSN_TEST_SLIDES_DIR"));
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "ron"))
            .collect();
        files.sort();
        let (mut strokes_total, mut strokes_missed) = (0usize, 0usize);
        let (mut on_active, mut off_active, mut plates) = (0usize, 0usize, 0usize);
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let c = parse_canvas(&std::fs::read_to_string(path).unwrap(), 3).unwrap();
            let mut row = Vec::new();
            for (li, l) in c.layers.iter().enumerate() {
                for s in &l.strokes {
                    strokes_total += 1;
                    let mid = s.samples[s.samples.len() / 2].pos;
                    if !crate::canvas::hit::stroke_hit(s, mid, 6.0) { strokes_missed += 1; }
                }
                let n = l.strokes.len() + l.shapes.len();
                if li == c.active_layer { on_active += n } else { off_active += n }
                if l.image.is_some() { plates += 1; }
                row.push(format!("{}{}:{}s/{}sh{}", if li == c.active_layer { "*" } else { "" }, li,
                    l.strokes.len(), l.shapes.len(), if l.image.is_some() { "+plate" } else { "" }));
            }
            println!("{name}: {}", row.join("  "));
        }
        println!("strokes: {strokes_total}, missed by hit-test at own midpoint: {strokes_missed}");
        println!("objects on the active layer: {on_active}; on other layers: {off_active}; layer plate images: {plates}");
    }

    /// Over a real data folder: everything on every visible layer can be
    /// picked by the Select tool, and turning a background plate into
    /// an image object changes neither the picture nor — once undone —
    /// a single byte of the save.
    ///
    ///   OSN_TEST_SLIDES_DIR=<copy>/canvases cargo test real_slides_pick -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_slides_pick_and_promote() {
        use crate::canvas::hit::{pick, CanvasHit};
        let dir = std::path::PathBuf::from(std::env::var_os("OSN_TEST_SLIDES_DIR").expect("set OSN_TEST_SLIDES_DIR"));
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "ron"))
            .collect();
        files.sort();
        let render = |c: &Canvas| {
            let mut pixmap = tiny_skia::Pixmap::new(960, 540).unwrap();
            crate::canvas::render::render_canvas(&mut pixmap.as_mut(), c, None, None, 1.0, None);
            pixmap.data().to_vec()
        };
        let (mut strokes, mut unpickable, mut plates, mut plate_unpickable) = (0, 0, 0, 0);
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let mut c = parse_canvas(&std::fs::read_to_string(path).unwrap(), 3).unwrap();
            for (li, l) in c.layers.iter().enumerate() {
                if !l.visible || l.opacity <= 0.0 { continue; }
                for s in &l.strokes {
                    strokes += 1;
                    let mid = s.samples[s.samples.len() / 2].pos;
                    if pick(&c, mid, 6.0, false).is_none() { unpickable += 1; }
                }
                if let Some(img) = l.image.as_ref() {
                    plates += 1;
                    // Somewhere on this plate must pick it (or whatever
                    // is drawn above it): probe a grid of points.
                    let [w, h] = img.extent();
                    let found = (1..8).flat_map(|i| (1..8).map(move |j| (i, j))).any(|(i, j)| {
                        let p = [img.canvas_origin[0] + w * i as f32 / 8.0, img.canvas_origin[1] + h * j as f32 / 8.0];
                        match pick(&c, p, 6.0, true) {
                            Some(CanvasHit::Plate(hit)) => hit == li,
                            _ => false,
                        }
                    });
                    if !found { plate_unpickable += 1; println!("{name}: plate on layer {li} never topmost on its grid"); }
                }
            }

            let saved_before = serialize_canvas(&c).unwrap();
            let pixels_before = render(&c);
            let n_plates = c.layers.iter().filter(|l| l.image.is_some()).count();
            for li in 0..c.layers.len() { c.promote_plate(li); }
            assert!(c.layers.iter().all(|l| l.image.is_none()));
            assert!(render(&c) == pixels_before, "{name}: promoting its plates changed the picture");
            // The promoted form saves and loads.
            let promoted = parse_canvas(&serialize_canvas(&c).unwrap(), 3).unwrap();
            assert!(render(&promoted) == pixels_before, "{name}: promoted slide does not reload to the same picture");
            while !c.undo_log.is_empty() { c.undo(); }
            assert!(serialize_canvas(&c).unwrap() == saved_before, "{name}: undo did not restore the plates exactly");
            println!("{name}: {n_plates} plate(s) promoted, picture identical, undo exact");
        }
        println!("strokes: {strokes}, not pickable: {unpickable}; plates: {plates}, never topmost anywhere: {plate_unpickable}");
        assert_eq!(unpickable, 0);
    }

    /// Loads every slide in a real data folder, writes it back out in
    /// memory and loads that again. Ignored by default because it needs
    /// the user's files; nothing is written to disk. Point it at a COPY:
    ///
    ///   OSN_TEST_SLIDES_DIR=<copy>/canvases cargo test real_slides -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_slides_round_trip() {
        let dir = std::path::PathBuf::from(
            std::env::var_os("OSN_TEST_SLIDES_DIR").expect("set OSN_TEST_SLIDES_DIR to a copy of the canvases folder"),
        );
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "ron"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no .ron files in {dir:?}");
        let mut identical = 0;
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(path).unwrap();
            let c = parse_canvas(&text, 3).unwrap_or_else(|e| panic!("{name} does not load: {e}"));
            let saved = serialize_canvas(&c).unwrap();
            let c2 = parse_canvas(&saved, 3).unwrap_or_else(|e| panic!("{name} does not reload: {e}"));
            assert_eq!(summary(&c), summary(&c2), "{name} changed across a save");
            assert_eq!(saved, serialize_canvas(&c2).unwrap(), "{name} is not stable across saves");
            if saved == text {
                identical += 1;
            }
            println!("{name}: ok  {}", summary(&c).join(" | "));
        }
        println!("{} slides loaded; {identical} would be written back byte-for-byte identical", files.len());
    }
}
