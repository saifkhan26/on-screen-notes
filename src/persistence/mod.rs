//! Persistent storage: reads and writes `AppConfig` and per-canvas files.
//!
//! Strategy:
//!   * One human-readable RON file per "thing" (config, canvas).
//!   * Atomic writes: write to a sibling `*.tmp` file, then rename onto the
//!     real path. On a crash we either keep the old file fully intact or
//!     replace it cleanly — no half-written state.
//!   * We try hard not to crash on a corrupted file; instead we log a warning
//!     and fall back to defaults.

mod blobs;
pub mod paths;

use crate::canvas::canvas::Canvas;
use crate::config::AppConfig;
use crate::error::{Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

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
pub fn load_canvas(index: usize, position_passes: usize, progress: &LoadProgress) -> LoadOutcome {
    let path = match paths::canvases_dir() {
        Ok(d) => d.join(format!("canvas_{index}.ron")),
        Err(e) => return LoadOutcome::Unreadable(e.to_string()),
    };
    if !path.exists() {
        return LoadOutcome::Missing;
    }
    let s = match read_counting(&path, progress) {
        Ok(s) => s,
        Err(e) => return LoadOutcome::Unreadable(e.to_string()),
    };
    match parse_canvas_counting(&s, position_passes, Some(&progress.parsed)) {
        Ok(c) => LoadOutcome::Loaded(c),
        Err(e) => LoadOutcome::Unreadable(e),
    }
}

/// How far reading one slide file has got, for a progress bar. Written
/// by the thread reading it, read by the UI.
#[derive(Debug, Default)]
pub struct LoadProgress {
    /// The file's size; 0 until known.
    pub size: AtomicU64,
    /// Bytes read from disk so far.
    pub read: AtomicU64,
    /// Bytes of picture lists parsed so far, which is nearly all of a
    /// slide big enough to need a progress bar.
    pub parsed: AtomicU64,
}

impl LoadProgress {
    /// 0..=1. Reading the file takes about as long as parsing it.
    pub fn fraction(&self) -> f32 {
        let size = self.size.load(Ordering::Relaxed);
        if size == 0 {
            return 0.0;
        }
        let part = |a: &AtomicU64| (a.load(Ordering::Relaxed) as f64 / size as f64).min(1.0);
        (0.4 * part(&self.read) + 0.6 * part(&self.parsed)) as f32
    }

    /// The file's size in bytes, once known.
    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Relaxed)
    }
}

/// `read_to_string`, counting the bytes into `progress` as they come.
fn read_counting(path: &Path, progress: &LoadProgress) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata().map_or(0, |m| m.len());
    progress.size.store(size, Ordering::Relaxed);
    let mut buf = Vec::with_capacity(size as usize);
    loop {
        let n = (&mut f).take(8 << 20).read_to_end(&mut buf)?;
        if n == 0 {
            break;
        }
        progress.read.fetch_add(n as u64, Ordering::Relaxed);
    }
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Parse one canvas file's text and bring it to the in-memory shape the
/// rest of the app expects (layers migrated, stroke caches built).
#[cfg(test)]
pub(crate) fn parse_canvas(s: &str, position_passes: usize) -> std::result::Result<Canvas, String> {
    parse_canvas_counting(s, position_passes, None)
}

/// `parse_canvas`, counting the picture bytes parsed into `parsed`.
fn parse_canvas_counting(s: &str, position_passes: usize, parsed: Option<&AtomicU64>) -> std::result::Result<Canvas, String> {
    // Slides of pictures go the fast way when it accounts for every
    // picture; anything else, or anything it is unsure of, the plain way.
    let mut c = match blobs::parse(s, parsed) {
        Some(c) => c,
        None => ron::from_str::<Canvas>(s).map_err(|e| e.to_string())?,
    };
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

/// Write every pin's place to `canvases/pins.ron`. Kept apart from the
/// slides for the same reason as the views: moving a pin is not an edit
/// of the slide its objects live on. Each slide stores which of its
/// objects are pinned, so without this file the pins still show, just
/// back where their objects are on their own slide.
pub fn save_pins(pins: &[crate::canvas::pins::Pin]) -> Result<()> {
    let path = paths::canvases_dir()?.join("pins.ron");
    atomic_write(&path, ron::ser::to_string(pins)?.as_bytes())
}

/// The places `save_pins` wrote, or none if the file is missing or
/// unreadable.
pub fn load_pins() -> Vec<crate::canvas::pins::Pin> {
    let Ok(path) = paths::canvases_dir().map(|d| d.join("pins.ron")) else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&path) else { return Vec::new() };
    ron::from_str(&s).unwrap_or_else(|e| {
        log::warn!("pins.ron could not be read ({e}); pins go back over their objects");
        Vec::new()
    })
}

/// Write every slide's name, in slide order, to `canvases/names.ron`.
/// Kept apart from the slides like the views: renaming a slide is not
/// an edit of it, and a big slide takes seconds to rewrite. An unnamed
/// slide is an empty string.
pub fn save_names(names: &[String]) -> Result<()> {
    let path = paths::canvases_dir()?.join("names.ron");
    atomic_write(&path, ron::ser::to_string(names)?.as_bytes())
}

/// The names `save_names` wrote, or none if the file is missing or
/// unreadable (every slide is then unnamed).
pub fn load_names() -> Vec<String> {
    let Ok(path) = paths::canvases_dir().map(|d| d.join("names.ron")) else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&path) else { return Vec::new() };
    ron::from_str(&s).unwrap_or_else(|e| {
        log::warn!("names.ron could not be read ({e}); slides show unnamed");
        Vec::new()
    })
}

/// Write which slides are locked, in slide order, to
/// `canvases/locks.ron`. Kept apart from the slides like the names:
/// locking a slide is not an edit of it.
pub fn save_locks(locks: &[bool]) -> Result<()> {
    let path = paths::canvases_dir()?.join("locks.ron");
    atomic_write(&path, ron::ser::to_string(locks)?.as_bytes())
}

/// The locks `save_locks` wrote, or none if the file is missing or
/// unreadable (every slide is then unlocked).
pub fn load_locks() -> Vec<bool> {
    let Ok(path) = paths::canvases_dir().map(|d| d.join("locks.ron")) else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&path) else { return Vec::new() };
    ron::from_str(&s).unwrap_or_else(|e| {
        log::warn!("locks.ron could not be read ({e}); slides show unlocked");
        Vec::new()
    })
}

/// A reorder of the slides whose files are being renamed into place.
///
/// Slide files are named by position (`canvas_N.ron`), so moving a slide
/// renames files rather than rewriting them: a 1 GB slide moves as fast
/// as an empty one, and no slide's contents are touched. This record is
/// `canvases/moving.ron` while the renames run, written before the first
/// one and removed after the last, so a crash in between is finished on
/// the next start (`finish_slide_move`) instead of leaving slides out of
/// order or out of sight.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlideMove {
    /// `order[j]` is the slot whose file becomes `canvas_j.ron`.
    pub order: Vec<usize>,
    /// The names, locks and views in the new order, written last.
    pub names: Vec<String>,
    pub locks: Vec<bool>,
    pub views: Vec<SlideView>,
    /// Every file that moves has left its old name: only putting them
    /// under their new names is left.
    #[serde(default)]
    pub placing: bool,
}

const MOVE_RECORD: &str = "moving.ron";

/// Rename the slide files into `mv.order` and write the names, locks and
/// views that go with it (see `SlideMove`). A move left unfinished by an
/// earlier failed attempt is finished instead — the caller retries the
/// same move until it goes through.
pub fn move_slide_files(mv: &SlideMove) -> Result<()> {
    move_slide_files_in(&paths::canvases_dir()?, mv)
}

pub(crate) fn move_slide_files_in(dir: &Path, mv: &SlideMove) -> Result<()> {
    if dir.join(MOVE_RECORD).exists() {
        return finish_slide_move_in(dir);
    }
    write_move_record(dir, mv)?;
    run_slide_move(dir, mv.clone())
}

/// Finish a slide reorder a crash cut short, before any slide is read.
/// Logged, never fatal: without it the slides still open, some of them
/// out of order.
pub fn finish_slide_move() {
    let Ok(dir) = paths::canvases_dir() else { return };
    if let Err(e) = finish_slide_move_in(&dir) {
        log::warn!("could not finish reordering the slides: {e:#}");
    }
}

pub(crate) fn finish_slide_move_in(dir: &Path) -> Result<()> {
    let path = dir.join(MOVE_RECORD);
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path:?}"))?;
    let mv: SlideMove = ron::from_str(&text).with_context(|| format!("reading {path:?}"))?;
    log::warn!("finishing a slide reorder that was cut short");
    run_slide_move(dir, mv)
}

fn write_move_record(dir: &Path, mv: &SlideMove) -> Result<()> {
    atomic_write(&dir.join(MOVE_RECORD), ron::ser::to_string(mv)?.as_bytes())
}

/// The renames, in two steps so no file is ever renamed onto another:
/// every file that moves first goes to `canvas_<new>.ron.moving`, then
/// each of those to `canvas_<new>.ron`. Both steps can be run again
/// after a crash: the first only moves files still under their old name,
/// the second only files still parked.
fn run_slide_move(dir: &Path, mut mv: SlideMove) -> Result<()> {
    let moved: Vec<usize> = (0..mv.order.len()).filter(|&j| mv.order[j] != j).collect();
    let file = |i: usize| dir.join(format!("canvas_{i}.ron"));
    let parked = |j: usize| dir.join(format!("canvas_{j}.ron.moving"));
    if !mv.placing {
        for &j in &moved {
            let (src, tmp) = (file(mv.order[j]), parked(j));
            // A slide never saved (a new one) has no file to move.
            if !tmp.exists() && src.exists() {
                std::fs::rename(&src, &tmp).with_context(|| format!("moving {src:?} aside"))?;
            }
        }
        mv.placing = true;
        write_move_record(dir, &mv)?;
    }
    for &j in &moved {
        let (tmp, dst) = (parked(j), file(j));
        if !tmp.exists() {
            continue;
        }
        if dst.exists() {
            // Only the autosave writes a slide file, and only with that
            // slide's newest state: the parked file is an older copy of
            // the same slide. Kept beside it all the same.
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let aside = dir.join(format!("canvas_{j}.ron.replaced-{stamp}"));
            log::warn!("{dst:?} was written before the reorder finished; the older copy is kept as {aside:?}");
            std::fs::rename(&tmp, &aside).with_context(|| format!("keeping {tmp:?} aside"))?;
            continue;
        }
        std::fs::rename(&tmp, &dst).with_context(|| format!("putting {tmp:?} in place"))?;
    }
    atomic_write(&dir.join("names.ron"), ron::ser::to_string(&mv.names)?.as_bytes())?;
    atomic_write(&dir.join("locks.ron"), ron::ser::to_string(&mv.locks)?.as_bytes())?;
    atomic_write(&dir.join("views.ron"), ron::ser::to_string(&mv.views)?.as_bytes())?;
    std::fs::remove_file(dir.join(MOVE_RECORD)).context("removing the reorder record")?;
    Ok(())
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

    /// `canvas_N.ron`, not `views.ron` / `pins.ron` beside it.
    fn is_slide_file(p: &std::path::Path) -> bool {
        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("canvas_") && n.ends_with(".ron"))
    }

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
            .filter(|p| is_slide_file(p))
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
            .filter(|p| is_slide_file(p))
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
            .filter(|p| is_slide_file(p))
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

    /// Times reading and parsing each slide of a real data folder (or
    /// only those named in `OSN_TEST_SLIDES`, comma-separated). Nothing
    /// is written. Point it at a COPY, in release mode for real numbers:
    ///
    ///   OSN_TEST_SLIDES_DIR=<copy>/canvases cargo test --release real_slides_load_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_slides_load_speed() {
        let dir = std::path::PathBuf::from(std::env::var_os("OSN_TEST_SLIDES_DIR").expect("set OSN_TEST_SLIDES_DIR"));
        let only: Vec<String> = std::env::var("OSN_TEST_SLIDES").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_slide_file(p))
            .filter(|p| only.is_empty() || only.iter().any(|n| p.file_name().is_some_and(|f| f == n.as_str())))
            .collect();
        files.sort();
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let t = std::time::Instant::now();
            let text = std::fs::read_to_string(path).unwrap();
            let read = t.elapsed();
            let t = std::time::Instant::now();
            let fast = blobs::parse(&text, None);
            let fast_ms = t.elapsed().as_secs_f64() * 1e3;
            let t = std::time::Instant::now();
            let plain: Canvas = ron::from_str(&text).unwrap();
            let plain_ms = t.elapsed().as_secs_f64() * 1e3;
            let shapes: usize = plain.layers.iter().map(|l| l.shapes.len()).sum();
            println!(
                "{name}: {:.1} MB, read {:.0} ms, ron {plain_ms:.0} ms, fast {}, {shapes} shapes",
                text.len() as f64 / 1e6,
                read.as_secs_f64() * 1e3,
                if fast.is_some() { format!("{fast_ms:.0} ms") } else { format!("not used ({fast_ms:.0} ms)") },
            );
            if let Some(fast) = fast {
                assert!(serialize_canvas(&fast).unwrap() == serialize_canvas(&plain).unwrap(), "{name}: the fast path read something else");
            }
        }
    }
    /// A scratch canvases folder holding `canvas_N.ron` = "slide <tag>".
    fn move_dir(name: &str, slides: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("osn-move-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, tag) in slides.iter().enumerate() {
            std::fs::write(dir.join(format!("canvas_{i}.ron")), format!("slide {tag}")).unwrap();
        }
        dir
    }

    fn slides_in(dir: &Path, n: usize) -> Vec<String> {
        (0..n).map(|i| std::fs::read_to_string(dir.join(format!("canvas_{i}.ron"))).unwrap_or_default()).collect()
    }

    fn a_move(order: Vec<usize>) -> SlideMove {
        let n = order.len();
        SlideMove {
            names: order.iter().map(|i| format!("name {i}")).collect(),
            locks: order.iter().map(|i| *i == 0).collect(),
            views: (0..n).map(|i| SlideView { pan: [i as f32, 0.0], zoom: 1.0 }).collect(),
            order,
            placing: false,
        }
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".moving") || n == MOVE_RECORD)
            .collect();
        v.sort();
        v
    }

    #[test]
    fn moving_a_slide_renames_the_files_and_writes_the_lists_in_the_new_order() {
        let dir = move_dir("plain", &["a", "b", "c", "d"]);
        // Slide 1 to the end: b c d a.
        move_slide_files_in(&dir, &a_move(vec![1, 2, 3, 0])).unwrap();
        assert_eq!(slides_in(&dir, 4), ["slide b", "slide c", "slide d", "slide a"]);
        assert_eq!(std::fs::read_to_string(dir.join("names.ron")).unwrap(), r#"["name 1","name 2","name 3","name 0"]"#);
        assert!(std::fs::read_to_string(dir.join("locks.ron")).unwrap().starts_with("[false,false,false,true]"));
        assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reorder_cut_short_while_moving_files_aside_is_finished_next_start() {
        let dir = move_dir("step1", &["a", "b", "c"]);
        // c a b: the record is written and one file parked, then a crash.
        let mv = a_move(vec![2, 0, 1]);
        write_move_record(&dir, &mv).unwrap();
        std::fs::rename(dir.join("canvas_2.ron"), dir.join("canvas_0.ron.moving")).unwrap();
        finish_slide_move_in(&dir).unwrap();
        assert_eq!(slides_in(&dir, 3), ["slide c", "slide a", "slide b"]);
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reorder_cut_short_while_placing_files_is_finished_next_start() {
        let dir = move_dir("step2", &["a", "b", "c"]);
        // b a c: both files parked, one put back, then a crash.
        let mut mv = a_move(vec![1, 0, 2]);
        std::fs::rename(dir.join("canvas_1.ron"), dir.join("canvas_0.ron.moving")).unwrap();
        std::fs::rename(dir.join("canvas_0.ron"), dir.join("canvas_1.ron.moving")).unwrap();
        mv.placing = true;
        write_move_record(&dir, &mv).unwrap();
        std::fs::rename(dir.join("canvas_0.ron.moving"), dir.join("canvas_0.ron")).unwrap();
        // A retry this run goes the same way as a finish at startup.
        move_slide_files_in(&dir, &mv).unwrap();
        assert_eq!(slides_in(&dir, 3), ["slide b", "slide a", "slide c"]);
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slide_never_saved_moves_without_a_file_and_nothing_is_overwritten() {
        // Slot 2 has no file yet (a new slide); it goes first.
        let dir = move_dir("unsaved", &["a", "b"]);
        move_slide_files_in(&dir, &a_move(vec![2, 0, 1])).unwrap();
        assert!(!dir.join("canvas_0.ron").exists(), "the new slide is written by the autosave");
        assert_eq!(slides_in(&dir, 3)[1..], ["slide a", "slide b"]);

        // The autosave wrote a slide before its parked file was put back:
        // the newer file stays and the older is kept beside it.
        let dir = move_dir("written", &["a", "b"]);
        let mut mv = a_move(vec![1, 0]);
        std::fs::rename(dir.join("canvas_1.ron"), dir.join("canvas_0.ron.moving")).unwrap();
        std::fs::rename(dir.join("canvas_0.ron"), dir.join("canvas_1.ron.moving")).unwrap();
        mv.placing = true;
        write_move_record(&dir, &mv).unwrap();
        std::fs::write(dir.join("canvas_0.ron"), "slide b, newer").unwrap();
        finish_slide_move_in(&dir).unwrap();
        assert_eq!(slides_in(&dir, 2), ["slide b, newer", "slide a"]);
        let kept = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("canvas_0.ron.replaced-"));
        assert!(kept, "the older copy is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
