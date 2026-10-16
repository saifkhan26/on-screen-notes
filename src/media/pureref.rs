//! Opening a PureRef board (`.pur`) as a new slide.
//!
//! Both generations of the file format are read:
//!   * 1.x ("1.10", also written by PureRef 2's "save as legacy"): one
//!     flat big-endian Qt data stream — a header, the board's pictures as
//!     PNG, one record per item, then a table of where each picture is.
//!   * 2.x ("2.1"): a SQLite database with a short header and a JPEG
//!     thumbnail in front, the database's first bytes moved to the end of
//!     the file.
//!
//! The layouts are reverse-engineered (FyorDev/PureRef-format and
//! wolfexplode/obsidian-excalidraw-pureref). Only what is needed to place
//! pictures and notes is read; groups only contribute their transforms,
//! and drawings, opacity and note backgrounds are left out.
//!
//! Boards can be gigabytes, so nothing holds the whole file: 1.x
//! pictures are read one at a time from their byte ranges, and a 2.x
//! database is opened read-only from memory, or for a big board from a
//! temporary reassembled copy that is deleted afterwards. Pictures are
//! decoded on a few worker threads and stored as files in the media
//! folder (`PictureRef`); the slide keeps only their names and small
//! previews. Animated GIFs shown whole become playing GIFs.
//!
//! The board file is only ever read. The slide keeps no path back to it
//! or to the pictures' original files, so deleting the slide cannot
//! touch them — only the app's own copies in the media folder, which
//! the usual slide-delete cleanup removes once nothing uses them.

use super::import::{encode_png, fit_side, shrink, store_bytes_into, IMAGE_SIDE};
use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{fresh_media_id, text_extent, MediaKind, MediaMeta, MediaRef, PictureRef, RasterKind, Shape};
use crate::error::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Font size of a note that names none: PureRef's default.
const NOTE_FONT_PX: f64 = 22.0;
/// Note text colour when the board gives none.
const NOTE_COLOR: [u8; 4] = [255, 255, 255, 255];
/// Bounds against damaged files claiming absurd counts or nesting.
const MAX_COUNT: usize = 100_000;
const MAX_DEPTH: usize = 64;
/// Long side of the preview kept inside the slide for each picture.
const PREVIEW_SIDE: u32 = 96;
/// A 2.x database up to this size is opened in memory; a bigger one is
/// reassembled into a temporary file instead, so opening a
/// multi-gigabyte board does not need that much memory.
const DB_IN_MEMORY_MAX: u64 = 512 << 20;
/// Long side of an animated GIF's poster frame.
const GIF_POSTER_SIDE: u32 = 240;
/// Most pictures decoded at once (each can take a few hundred MB).
const MAX_WORKERS: usize = 6;

/// Whether a file is a PureRef board, from its extension.
pub fn is_pureref(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pur"))
}

/// A board ready to become a slide. Positions and sizes are in PureRef
/// scene units, where a picture at 100 % is one unit per pixel.
pub struct Board {
    /// Paint order, bottom first.
    pub items: Vec<BoardItem>,
    /// Items that could not be brought over: pictures linked from disk
    /// rather than stored in the board, or in a format that cannot be
    /// decoded here.
    pub skipped: usize,
}

pub enum BoardItem {
    Picture {
        /// The shape's `png`: a small preview of a stored picture, or an
        /// animated GIF's poster frame.
        png: Arc<Vec<u8>>,
        px_size: [u32; 2],
        /// Exactly one of these is set.
        picture: Option<PictureRef>,
        media: Option<MediaRef>,
        center: [f64; 2],
        half: [f64; 2],
        /// Radians, clockwise on screen, about `center`.
        rotation: f64,
    },
    Note {
        text: String,
        /// Top-left of the first line; the label turns about it.
        pos: [f64; 2],
        font_size: f64,
        color: [u8; 4],
        rotation: f64,
    },
}

impl Board {
    /// (pictures, notes)
    pub fn counts(&self) -> (usize, usize) {
        let pictures = self.items.iter().filter(|i| matches!(i, BoardItem::Picture { .. })).count();
        (pictures, self.items.len() - pictures)
    }
}

impl BoardItem {
    fn corners(&self) -> [[f64; 2]; 4] {
        let (origin, ext, rotation) = match self {
            BoardItem::Picture { center, half, rotation, .. } => {
                ([center[0], center[1]], [[-half[0], -half[1]], [half[0], -half[1]], [half[0], half[1]], [-half[0], half[1]]], *rotation)
            }
            BoardItem::Note { text, pos, font_size, rotation, .. } => {
                let [w, h] = text_extent(text, *font_size as f32).map(f64::from);
                (*pos, [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]], *rotation)
            }
        };
        let (s, c) = rotation.sin_cos();
        ext.map(|[x, y]| [origin[0] + c * x - s * y, origin[1] + s * x + c * y])
    }
}

/// Where opening a board has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage {
    /// Reading the board's structure; `done` / `total` count bytes of a
    /// 2.x database being loaded.
    #[default]
    Reading,
    /// Decoding and storing pictures; `done` / `total` count pictures.
    Pictures,
}

/// Progress of one open, shared with the UI, which can also cancel it.
#[derive(Default)]
pub struct Progress {
    state: Mutex<(Stage, u64, u64)>,
    cancel: AtomicBool,
}

impl Progress {
    /// (stage, done, total)
    pub fn get(&self) -> (Stage, u64, u64) {
        *lock(&self.state)
    }

    fn set(&self, stage: Stage, done: u64, total: u64) {
        *lock(&self.state) = (stage, done, total);
    }

    fn advance(&self, n: u64) {
        lock(&self.state).1 += n;
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read and decode a board, storing its pictures in `media_dir`. Slow
/// for a big one: run it off the UI thread. On failure or cancel, the
/// files it stored are removed again.
pub fn open(path: &Path, media_dir: &Path, progress: &Progress) -> Result<Board> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let len = file.metadata().context("reading the board")?.len();
    open_from(std::io::BufReader::with_capacity(1 << 20, file), len, media_dir, progress, DB_IN_MEMORY_MAX)
}

struct Shared {
    progress: Progress,
    result: Mutex<Option<std::result::Result<Board, String>>>,
}

/// One board being opened on a background thread.
pub struct OpenJob {
    pub path: PathBuf,
    shared: Arc<Shared>,
}

impl OpenJob {
    pub fn start(path: PathBuf) -> Self {
        let shared = Arc::new(Shared { progress: Progress::default(), result: Mutex::new(None) });
        let s = Arc::clone(&shared);
        let p = path.clone();
        let spawned = std::thread::Builder::new().name("osn-pureref-open".into()).spawn(move || {
            let r = crate::persistence::paths::media_dir()
                .and_then(|dir| open(&p, &dir, &s.progress))
                .map_err(|e| format!("{e:#}"));
            *lock(&s.result) = Some(r);
        });
        if let Err(e) = spawned {
            *lock(&shared.result) = Some(Err(e.to_string()));
        }
        Self { path, shared }
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// (stage, done, total)
    pub fn progress(&self) -> (Stage, u64, u64) {
        self.shared.progress.get()
    }

    pub fn cancel(&self) {
        self.shared.progress.cancel();
    }

    pub fn cancelling(&self) -> bool {
        self.shared.progress.cancelled()
    }

    /// The outcome, once it is in.
    pub fn take(&self) -> Option<std::result::Result<Board, String>> {
        lock(&self.shared.result).take()
    }
}

/// Lay a board out as a fresh slide: everything at its natural on-screen
/// size (one picture pixel per physical screen pixel, as an inserted
/// picture gets), shrunk as one piece to fit 90 % of a `view`-sized
/// window, and centred in it. A new slide starts at pan 0 / zoom 1, so
/// the view's centre is `view / 2`.
pub fn into_canvas(board: Board, view: [f32; 2], pixels_per_point: f32, name: &str) -> Canvas {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for item in &board.items {
        for p in item.corners() {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let view = [f64::from(view[0].max(1.0)), f64::from(view[1].max(1.0))];
    let mut s = 1.0 / f64::from(pixels_per_point.max(0.01));
    let mut mid = [0.0, 0.0];
    if lo[0].is_finite() && hi[0].is_finite() && lo[1].is_finite() && hi[1].is_finite() {
        for k in 0..2 {
            let extent = hi[k] - lo[k];
            if extent > 1e-9 {
                s = s.min(0.9 * view[k] / extent);
            }
            mid[k] = (lo[k] + hi[k]) * 0.5;
        }
    }
    let map = |p: [f64; 2]| [((p[0] - mid[0]) * s + view[0] * 0.5) as f32, ((p[1] - mid[1]) * s + view[1] * 0.5) as f32];

    let shapes = board
        .items
        .into_iter()
        .map(|item| match item {
            BoardItem::Picture { png, px_size, picture, media, center, half, rotation } => {
                let c = map(center);
                let half = [(half[0] * s) as f32, (half[1] * s) as f32];
                Shape::Raster {
                    pos: [c[0] - half[0], c[1] - half[1]],
                    size: [half[0] * 2.0, half[1] * 2.0],
                    png,
                    px_size,
                    color: [0, 0, 0, 0],
                    rotation: rotation as f32,
                    kind: RasterKind::Image,
                    media: media.map(Box::new),
                    picture: picture.map(Box::new),
                }
            }
            BoardItem::Note { text, pos, font_size, color, rotation } => Shape::Text {
                pos: map(pos),
                content: text,
                font_size: (font_size * s) as f32,
                color,
                rotation: rotation as f32,
            },
        })
        .collect();

    let mut canvas = Canvas::default();
    canvas.layers[0].shapes = shapes;
    if !name.is_empty() {
        canvas.layers[0].name = name.to_string();
    }
    canvas
}

// ---------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------

/// A 2-D affine map in Qt's row-vector layout: `x' = a·x + c·y + tx`,
/// `y' = b·x + d·y + ty`, i.e. a=m11, b=m12, c=m21, d=m22. The same
/// shape as `canvas::affine::Affine`, in f64 and with an inverse.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Xf {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

impl Xf {
    const IDENTITY: Xf = Xf { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    fn translate(x: f64, y: f64) -> Self {
        Xf { tx: x, ty: y, ..Self::IDENTITY }
    }

    /// A stored QTransform (m11 m12 m13 m21 m22 m23 dx dy m33). PureRef
    /// never uses the perspective terms, so they are ignored.
    fn from_qt(m: [f64; 9]) -> Self {
        Xf { a: m[0], b: m[1], c: m[3], d: m[4], tx: m[6], ty: m[7] }
    }

    /// The map that applies `self` first and `next` second.
    fn then(self, next: Xf) -> Xf {
        Xf {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            tx: next.a * self.tx + next.c * self.ty + next.tx,
            ty: next.b * self.tx + next.d * self.ty + next.ty,
        }
    }

    fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        [self.a * p[0] + self.c * p[1] + self.tx, self.b * p[0] + self.d * p[1] + self.ty]
    }

    fn apply_vec(&self, v: [f64; 2]) -> [f64; 2] {
        [self.a * v[0] + self.c * v[1], self.b * v[0] + self.d * v[1]]
    }

    fn det(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    fn inverse(&self) -> Option<Xf> {
        let det = self.det();
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Xf { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }

    fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.tx, self.ty].iter().all(|v| v.is_finite())
    }
}

// ---------------------------------------------------------------------
// What both readers produce
// ---------------------------------------------------------------------

#[derive(Default)]
struct RawBoard {
    /// Encoded picture files, each placed by one or more `placements`.
    pictures: Vec<RawPicture>,
    placements: Vec<RawPlacement>,
    notes: Vec<RawNote>,
    skipped: usize,
}

struct RawPicture {
    /// Where its encoded file is; read only when it is decoded.
    at: Locator,
    /// The size PureRef shows it at (2.x records it), which tells
    /// whether PureRef turned a sideways photo upright.
    shown: Option<[u32; 2]>,
}

enum Locator {
    /// A 1.x PNG: byte range in the board file.
    Range(u64, u64),
    /// A 2.x picture: `images.id`.
    Row(i64),
}

struct RawPlacement {
    picture: usize,
    /// Source pixels → scene.
    to_scene: Xf,
    /// Crop outline in source pixels; empty = the whole picture.
    crop: Vec<[f64; 2]>,
    /// z of each ancestor then the item itself: groups and their
    /// children stack as a unit.
    order: Vec<f64>,
}

struct RawNote {
    /// Plain text, lines split on `\n`.
    text: String,
    /// Note space → scene.
    to_scene: Xf,
    font_px: f64,
    color: [u8; 4],
    /// 2.x places a note's centre at its origin, 1.x its top-left.
    centred: bool,
    /// Width the note's text wraps at, in note space, when it was given
    /// a fixed width; otherwise each paragraph is one line.
    width: Option<f64>,
    order: Vec<f64>,
}

fn open_from<R: Read + Seek>(mut src: R, len: u64, media_dir: &Path, progress: &Progress, db_in_memory_max: u64) -> Result<Board> {
    let mut magic = [0u8; 4];
    if len < 12 || src.seek(SeekFrom::Start(0)).and_then(|_| src.read_exact(&mut magic)).is_err() {
        bail!("not a PureRef board (the file is too short)");
    }
    let store = Store { dir: media_dir.to_path_buf(), created: Mutex::new(Vec::new()) };
    let result = match u32::from_be_bytes(magic) {
        8 => parse_v1(&mut src, len).and_then(|raw| build(raw, &mut Fetch::File(&mut src), &store, progress)),
        6 => parse_v2(&mut src, len, progress, db_in_memory_max).and_then(|(raw, db)| build(raw, &mut Fetch::<R>::Db(&db.conn), &store, progress)),
        _ => Err(anyhow!("not a PureRef board, or one from an unsupported PureRef version")),
    };
    match result {
        Ok(board) if !progress.cancelled() => Ok(board),
        other => {
            // Nothing will refer to what this open stored.
            store.remove_created();
            match other {
                Err(e) if !progress.cancelled() => Err(e),
                _ => bail!("cancelled"),
            }
        }
    }
}

/// The media folder, recording which files this open added.
struct Store {
    dir: PathBuf,
    created: Mutex<Vec<String>>,
}

impl Store {
    fn put(&self, bytes: &[u8], ext: &str) -> Result<String> {
        let (name, created) = store_bytes_into(&self.dir, bytes, ext)?;
        if created {
            lock(&self.created).push(name.clone());
        }
        Ok(name)
    }

    /// Remove the files this open added. Files that were already there
    /// (the same picture from an earlier board) belong to other slides
    /// and stay.
    fn remove_created(&self) {
        for name in lock(&self.created).drain(..) {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }
}

/// Reads picture files out of a board.
enum Fetch<'a, R> {
    File(&'a mut R),
    Db(&'a rusqlite::Connection),
}

impl<R: Read + Seek> Fetch<'_, R> {
    fn bytes(&mut self, at: &Locator) -> Result<Vec<u8>> {
        match (self, at) {
            (Fetch::File(src), Locator::Range(start, end)) => {
                read_at(&mut **src, *start, usize::try_from(end - start).context("picture too large")?)
            }
            (Fetch::Db(conn), Locator::Row(id)) => conn
                .query_row("SELECT data FROM images WHERE id = ?1", [id], |row| {
                    Ok(match row.get_ref(0)? {
                        rusqlite::types::ValueRef::Blob(b) => b.to_vec(),
                        _ => Vec::new(),
                    })
                })
                .context("reading a picture"),
            _ => bail!("picture lookup does not match the board format"),
        }
    }
}

fn read_at<R: Read + Seek>(src: &mut R, at: u64, n: usize) -> Result<Vec<u8>> {
    src.seek(SeekFrom::Start(at)).context("reading the board")?;
    let mut buf = vec![0u8; n];
    src.read_exact(&mut buf).map_err(|_| anyhow!("the board is damaged (cut short at byte {at})"))?;
    Ok(buf)
}

/// Big-endian reader over a piece of the file starting at byte `base`.
/// Positions are file offsets. Bounds-checked: a damaged file is an
/// error, never a panic.
struct Reader<'a> {
    buf: &'a [u8],
    base: usize,
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8], base: usize) -> Self {
        Self { buf, base, pos: base }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let i = self.pos - self.base;
        let end = i
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| anyhow!("the board is damaged (cut short at byte {})", self.pos))?;
        let s = &self.buf[i..end];
        self.pos += n;
        Ok(s)
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    fn seek(&mut self, to: usize) -> Result<()> {
        if to < self.base || to - self.base > self.buf.len() {
            bail!("the board is damaged (points past its end)");
        }
        self.pos = to;
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes()?))
    }

    fn offset(&mut self) -> Result<usize> {
        usize::try_from(u64::from_be_bytes(self.bytes()?)).map_err(|_| anyhow!("the board is damaged (offset out of range)"))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.bytes()?))
    }

    fn doubles<const N: usize>(&mut self) -> Result<[f64; N]> {
        let mut a = [0.0; N];
        for v in &mut a {
            *v = self.f64()?;
        }
        Ok(a)
    }

    /// A QString: u32 byte length (`0xFFFFFFFF` = null), UTF-16BE.
    fn qstring(&mut self) -> Result<Option<String>> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Ok(None);
        }
        let raw = self.take(len as usize)?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Ok(Some(String::from_utf16_lossy(&units)))
    }

    /// A QColor: spec, alpha and three 16-bit channels, padding.
    fn qcolor(&mut self) -> Result<Option<[u8; 4]>> {
        let spec = self.u8()?;
        let [a, c1, c2, c3] = [self.u16()?, self.u16()?, self.u16()?, self.u16()?];
        self.u16()?;
        let hi = |v: u16| (v >> 8) as u8;
        Ok(match spec {
            1 => Some([hi(c1), hi(c2), hi(c3), hi(a)]),
            2 => Some(hsv_to_rgba(c1, c2, c3, a)),
            _ => None,
        })
    }
}

/// Qt's HSV: hue in hundredths of a degree (`0xFFFF` = no hue),
/// saturation and value 0..=65535.
fn hsv_to_rgba(h: u16, s: u16, v: u16, a: u16) -> [u8; 4] {
    let h = if h == u16::MAX { 0.0 } else { f64::from(h) / 100.0 };
    let s = f64::from(s) / 65535.0;
    let v = f64::from(v) / 65535.0;
    let c = v * s;
    let hp = (h / 60.0).rem_euclid(6.0);
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [to(r), to(g), to(b), (a >> 8) as u8]
}

// ---------------------------------------------------------------------
// PureRef 1.x
// ---------------------------------------------------------------------

const V1_IMAGE: &str = "GraphicsImageItem";
const V1_TEXT: &str = "GraphicsTextItem";

/// What a 1.x picture-table entry holds.
#[derive(Clone, Copy)]
enum Slot {
    Png(u64, u64),
    /// Shares the PNG of the item with this id.
    Same(u32),
    /// Linked from disk, not stored.
    Linked,
}

/// Picture lookup for 1.x: each image item's id names a table entry.
struct V1Pictures {
    slots: HashMap<u32, Slot>,
    by_range: HashMap<(u64, u64), usize>,
}

impl V1Pictures {
    fn picture_for(&mut self, id: u32, out: &mut RawBoard) -> Option<usize> {
        let mut id = id;
        for _ in 0..MAX_DEPTH {
            match *self.slots.get(&id)? {
                Slot::Png(start, end) => {
                    let next = out.pictures.len();
                    let index = *self.by_range.entry((start, end)).or_insert(next);
                    if index == next {
                        out.pictures.push(RawPicture { at: Locator::Range(start, end), shown: None });
                    }
                    return Some(index);
                }
                Slot::Same(owner) => id = owner,
                Slot::Linked => return None,
            }
        }
        None
    }
}

fn parse_v1<R: Read + Seek>(src: &mut R, len: u64) -> Result<RawBoard> {
    let len = usize::try_from(len).context("the board is too large")?;
    let head = read_at(src, 0, len.min(512))?;
    let mut r = Reader::new(&head, 0);
    let tag = r.qstring()?.unwrap_or_default();
    if tag.trim_end_matches('\0') != "1.10" {
        bail!("PureRef format {tag:?} is not supported");
    }
    let root_count = usize::from(r.u16()?);
    let image_count = usize::from(r.u16()?);
    let table = r.offset()?;
    r.qstring()?; // app version (12 bytes, 10 in PureRef 2's legacy export)
    r.qstring()?; // MD5 of the rest, not checked
    r.skip(4 + 4 * 8 + 9 * 8 + 2 * 4)?; // id counter, scene rect, view transform, scroll
    let header_end = r.pos;

    if table < header_end || table.checked_add(image_count * 20).map_or(true, |e| e > len) {
        bail!("the board is damaged (bad picture table)");
    }
    let entries = read_at(src, table as u64, image_count * 20)?;
    let mut t = Reader::new(&entries, table);
    let mut pictures = V1Pictures { slots: HashMap::new(), by_range: HashMap::new() };
    let mut items_start = header_end;
    for _ in 0..image_count {
        let id = t.u32()?;
        let (start, end) = (t.offset()?, t.offset()?);
        if start < header_end || end <= start || end > table {
            bail!("the board is damaged (bad picture table)");
        }
        items_start = items_start.max(end);
        // A 4-byte entry is a reference, not a picture.
        let slot = if end - start == 4 {
            let owner = read_at(src, start as u64, 4)?;
            match u32::from_be_bytes([owner[0], owner[1], owner[2], owner[3]]) {
                u32::MAX => Slot::Linked,
                owner => Slot::Same(owner),
            }
        } else {
            Slot::Png(start as u64, end as u64)
        };
        pictures.slots.insert(id, slot);
    }

    // The item records sit between the pictures and the table; they are
    // small next to the pictures, so they are read in one go.
    let records = read_at(src, items_start as u64, table - items_start)?;
    let mut out = RawBoard::default();
    let mut r = Reader::new(&records, items_start);
    for _ in 0..root_count {
        read_v1_item(&mut r, table, Xf::IDENTITY, &[], &mut pictures, &mut out, 0)?;
    }
    Ok(out)
}

/// One item record and, after it, its children. `parent` maps the
/// parent's space to the scene.
fn read_v1_item(
    r: &mut Reader,
    limit: usize,
    parent: Xf,
    parent_order: &[f64],
    pictures: &mut V1Pictures,
    out: &mut RawBoard,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("the board is damaged (items nested too deeply)");
    }
    let start = r.pos;
    let end = r.offset()?;
    let kind = r.qstring()?.unwrap_or_default();
    if end <= r.pos || end > limit {
        bail!("the board is damaged (bad item at byte {start})");
    }
    let (to_scene, order, children) = match kind.as_str() {
        V1_IMAGE => {
            // Source path and name; a "BruteForceLoaded" picture has an
            // empty path and that word where the name goes.
            r.qstring()?;
            r.qstring()?;
            r.f64()?; // always 1.0
            let item = Xf::from_qt(r.doubles()?);
            let id = r.u32()?;
            let z = r.f64()?;
            // Source pixels → item space. The last double scales the
            // linear part (always 1.0 in practice).
            let pre = r.doubles::<9>()?;
            let image = Xf { a: pre[0] * pre[8], b: pre[1] * pre[8], c: pre[3] * pre[8], d: pre[4] * pre[8], tx: pre[6], ty: pre[7] };
            let n = r.u32()? as usize;
            if n > MAX_COUNT {
                bail!("the board is damaged (bad crop at byte {start})");
            }
            let mut outline = Vec::with_capacity(n);
            for _ in 0..n {
                r.u32()?; // move-to / line-to
                outline.push([r.f64()?, r.f64()?]);
            }
            r.skip(21)?;
            let children = r.u32()?;
            let to_scene = item.then(parent);
            let mut order = parent_order.to_vec();
            order.push(z);
            match (pictures.picture_for(id, out), image.inverse()) {
                (Some(picture), Some(inv)) if to_scene.is_finite() => out.placements.push(RawPlacement {
                    picture,
                    to_scene: image.then(to_scene),
                    crop: outline.iter().map(|p| inv.apply(*p)).collect(),
                    order: order.clone(),
                }),
                _ => out.skipped += 1,
            }
            (to_scene, order, children)
        }
        V1_TEXT => {
            let text = r.qstring()?.unwrap_or_default();
            let item = Xf::from_qt(r.doubles()?);
            r.u32()?; // id
            let z = r.f64()?;
            let fg = r.qcolor()?;
            r.qcolor()?; // background, not imported
            let children = r.u32()?;
            let to_scene = item.then(parent);
            let mut order = parent_order.to_vec();
            order.push(z);
            if to_scene.is_finite() {
                out.notes.push(RawNote {
                    text: note_text(&text),
                    to_scene,
                    font_px: html_font_px(&text).unwrap_or(NOTE_FONT_PX),
                    color: html_color(&text).or(fg).unwrap_or(NOTE_COLOR),
                    centred: false,
                    width: None,
                    order: order.clone(),
                });
            }
            (to_scene, order, children)
        }
        other => bail!("the board holds an item of an unknown kind ({other:?})"),
    };
    if r.pos > end || children as usize > MAX_COUNT {
        bail!("the board is damaged (bad item at byte {start})");
    }
    // Later PureRef versions append fields past the ones read here.
    r.seek(end)?;
    for _ in 0..children {
        read_v1_item(r, limit, to_scene, &order, pictures, out, depth + 1)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// PureRef 2.x
// ---------------------------------------------------------------------

/// The database inside a 2.x board, open read-only.
struct Db {
    // Declared first so it closes before the temporary file is deleted.
    conn: rusqlite::Connection,
    _temp: Option<TempFile>,
}

/// A temporary file, deleted when dropped.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Reads a 2.x board's database in order. The file holds a header and
/// thumbnail (`front` bytes), then the database from byte `front` on,
/// then the database's own first `front` bytes.
struct Rotated<'a, R> {
    src: &'a mut R,
    /// (file offset, length) of each piece, in database order.
    pieces: [(u64, u64); 2],
    piece: usize,
    left: u64,
    started: bool,
    progress: &'a Progress,
}

impl<R: Read + Seek> Read for Rotated<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.piece >= self.pieces.len() || buf.is_empty() {
                return Ok(0);
            }
            if !self.started {
                let (at, len) = self.pieces[self.piece];
                self.src.seek(SeekFrom::Start(at))?;
                self.left = len;
                self.started = true;
            }
            if self.left == 0 {
                self.piece += 1;
                self.started = false;
                continue;
            }
            if self.progress.cancelled() {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
            }
            let n = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
            let got = self.src.read(&mut buf[..n])?;
            if got == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.left -= got as u64;
            self.progress.advance(got as u64);
            return Ok(got);
        }
    }
}

fn parse_v2<R: Read + Seek>(src: &mut R, len: u64, progress: &Progress, db_in_memory_max: u64) -> Result<(RawBoard, Db)> {
    if len < 124 {
        bail!("not a PureRef board (the file is too short)");
    }
    let head = read_at(src, 0, 124)?;
    let units: Vec<u16> = head[4..12].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let tag = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
    if !tag.starts_with("2.") {
        bail!("PureRef format {tag:?} is not supported");
    }
    let thumb = u64::from(u32::from_be_bytes([head[104], head[105], head[106], head[107]]));
    let front = 108 + thumb;
    if front >= len {
        bail!("the board is damaged (bad header)");
    }
    let db_len = len - front;
    let stated = u64::from_be_bytes([head[14], head[15], head[16], head[17], head[18], head[19], head[20], head[21]]);
    if stated != db_len || db_len < front {
        bail!("the board is damaged (its database size does not add up)");
    }
    let db_head = read_at(src, db_len, 100)?;
    if !db_head.starts_with(b"SQLite format 3\0") {
        bail!("the board is damaged (no database inside)");
    }
    let page = match u16::from_be_bytes([db_head[16], db_head[17]]) {
        1 => 65536,
        p => u64::from(p),
    };
    let pages = u64::from(u32::from_be_bytes([db_head[28], db_head[29], db_head[30], db_head[31]]));
    if page.checked_mul(pages) != Some(db_len) {
        bail!("the board is damaged (its database is incomplete)");
    }

    progress.set(Stage::Reading, 0, db_len);
    let rotated = Rotated { src, pieces: [(db_len, front), (front, db_len - front)], piece: 0, left: 0, started: false, progress };
    // Either way the board file itself is only read: the database is
    // opened read-only from memory or from a reassembled temporary copy.
    let db = if db_len <= db_in_memory_max {
        let mut conn = rusqlite::Connection::open_in_memory().context("opening the board's database")?;
        let size = usize::try_from(db_len).context("the board is too large")?;
        conn.deserialize_read_exact(rusqlite::MAIN_DB, rotated, size, true).context("reading the board's database")?;
        Db { conn, _temp: None }
    } else {
        temp_db(rotated)?
    };
    let raw = read_v2_db(&db.conn).context("reading the board's database")?;
    Ok((raw, db))
}

/// Reassemble the database into a temporary file and open that.
fn temp_db<R: Read + Seek>(mut rotated: Rotated<'_, R>) -> Result<Db> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir();
    remove_stale_temp_dbs(&dir);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = TempFile(dir.join(format!("osn-pureref-{}-{n}.sqlite", std::process::id())));
    {
        let mut out = std::fs::File::create(&temp.0).context("making a temporary copy of the board's database")?;
        let mut buf = vec![0u8; 4 << 20];
        loop {
            let got = rotated.read(&mut buf).context("reading the board's database")?;
            if got == 0 {
                break;
            }
            out.write_all(&buf[..got]).context("making a temporary copy of the board's database")?;
        }
        out.flush().context("making a temporary copy of the board's database")?;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(&temp.0, flags).context("opening the board's database")?;
    Ok(Db { conn, _temp: Some(temp) })
}

/// Temporary databases left behind by an open that never finished (the
/// app was closed or crashed mid-way). Only old ones go, so another
/// running copy of the app keeps the one it is reading.
fn remove_stale_temp_dbs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("osn-pureref-") && name.ends_with(".sqlite")) {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 6 * 3600);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

struct V2Item {
    parent: i64,
    z: f64,
    xf: Option<Xf>,
}

fn read_v2_db(conn: &rusqlite::Connection) -> Result<RawBoard> {
    let tables: HashSet<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for t in ["items", "images", "items_images"] {
        if !tables.contains(t) {
            bail!("not a PureRef board (no {t} table)");
        }
    }

    let mut items: HashMap<i64, V2Item> = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, parent, z, CAST(transform AS BLOB) FROM items")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let blob: Option<Vec<u8>> = row.get(3)?;
        items.insert(
            row.get(0)?,
            V2Item {
                parent: row.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                z: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                xf: blob.as_deref().and_then(|b| qt_value(b, qtransform)).filter(Xf::is_finite),
            },
        );
    }
    // Item space → scene, through every enclosing group.
    let scene_of = |id: i64| -> Option<Xf> {
        let mut xf = Xf::IDENTITY;
        let mut cur = id;
        for _ in 0..MAX_DEPTH {
            let item = items.get(&cur)?;
            xf = xf.then(item.xf?);
            if !items.contains_key(&item.parent) {
                return Some(xf);
            }
            cur = item.parent;
        }
        None
    };
    let order_of = |id: i64| -> Vec<f64> {
        let mut zs = Vec::new();
        let mut cur = id;
        while let Some(item) = items.get(&cur) {
            zs.push(item.z);
            if zs.len() >= MAX_DEPTH {
                break;
            }
            cur = item.parent;
        }
        zs.reverse();
        zs
    };

    let mut out = RawBoard::default();
    let mut picture_index: HashMap<i64, usize> = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, width, height FROM images WHERE typeof(data) = 'blob' AND length(data) > 0")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        // A picture linked from disk stores no data and is not listed;
        // its placements are counted as skipped below. The data itself
        // is read later, one picture at a time.
        let id: i64 = row.get(0)?;
        let size = |v: Option<i64>| v.and_then(|v| u32::try_from(v).ok());
        let shown = match (size(row.get(1)?), size(row.get(2)?)) {
            (Some(w), Some(h)) => Some([w, h]),
            _ => None,
        };
        picture_index.insert(id, out.pictures.len());
        out.pictures.push(RawPicture { at: Locator::Row(id), shown });
    }

    let mut stmt = conn.prepare("SELECT id, image, CAST(image_transform AS BLOB), CAST(image_bounds AS BLOB) FROM items_images")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let picture = row.get::<_, Option<i64>>(1)?.and_then(|i| picture_index.get(&i).copied());
        let image_xf = row.get::<_, Option<Vec<u8>>>(2)?.as_deref().and_then(|b| qt_value(b, qtransform)).filter(Xf::is_finite);
        let outline = row.get::<_, Option<Vec<u8>>>(3)?.as_deref().and_then(|b| qt_value(b, qpainterpath)).unwrap_or_default();
        match (picture, image_xf, scene_of(id)) {
            (Some(picture), Some(image_xf), Some(to_scene)) => {
                let crop = match image_xf.inverse() {
                    Some(inv) => outline.iter().map(|p| inv.apply(*p)).collect(),
                    None => Vec::new(),
                };
                out.placements.push(RawPlacement { picture, to_scene: image_xf.then(to_scene), crop, order: order_of(id) });
            }
            _ => out.skipped += 1,
        }
    }

    if tables.contains("items_notes") {
        let mut stmt = conn.prepare("SELECT * FROM items_notes")?;
        let id_col = stmt.column_index("id")?;
        let text_col = stmt.column_index("text").ok();
        let color_col = stmt.column_index("text_color").ok();
        let size_col = stmt.column_index("fixed_size").ok();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let string = |col: Option<usize>| -> rusqlite::Result<String> {
                let Some(col) = col else { return Ok(String::new()) };
                Ok(match row.get_ref(col)? {
                    rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => String::from_utf8_lossy(t).into_owned(),
                    _ => String::new(),
                })
            };
            let id: i64 = row.get(id_col)?;
            let html = string(text_col)?;
            // QSizeF, (-1, -1) when the note sizes itself.
            let width = match size_col.map(|c| row.get_ref(c)).transpose()? {
                Some(rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b)) => qt_value(b, qsizef).map(|[w, _]| w),
                _ => None,
            }
            .filter(|w| *w > 0.0 && w.is_finite());
            let Some(to_scene) = scene_of(id) else {
                out.skipped += 1;
                continue;
            };
            out.notes.push(RawNote {
                text: note_text(&html),
                to_scene,
                font_px: html_font_px(&html).unwrap_or(NOTE_FONT_PX),
                color: html_color(&html).or_else(|| hex_color(string(color_col).ok()?.trim())).unwrap_or(NOTE_COLOR),
                centred: true,
                width,
                order: order_of(id),
            });
        }
    }
    Ok(out)
}

/// Decode a Qt value stored in the database. PureRef keeps these as
/// raw bytes in newer files and, in older ones, as TEXT whose
/// characters are the bytes (Latin-1 code points, stored as UTF-8).
fn qt_value<T>(blob: &[u8], parse: fn(&[u8]) -> Option<T>) -> Option<T> {
    if let Some(v) = parse(blob) {
        return Some(v);
    }
    let text = std::str::from_utf8(blob).ok()?;
    let bytes = text.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect::<Option<Vec<u8>>>()?;
    parse(&bytes)
}

/// QVariant(QTransform): type 80, not-null flag, nine doubles. The
/// exact length tells raw bytes from the TEXT-wrapped form.
fn qtransform(b: &[u8]) -> Option<Xf> {
    if b.len() != 77 || b[..5] != [0, 0, 0, 0x50, 0] {
        return None;
    }
    let mut m = [0.0; 9];
    for (i, v) in m.iter_mut().enumerate() {
        *v = f64::from_be_bytes(b[5 + i * 8..13 + i * 8].try_into().ok()?);
    }
    Some(Xf::from_qt(m))
}

/// QVariant(QSizeF): type 22, not-null flag, width and height.
fn qsizef(b: &[u8]) -> Option<[f64; 2]> {
    if b.len() != 21 || b[..5] != [0, 0, 0, 0x16, 0] {
        return None;
    }
    Some([f64::from_be_bytes(b[5..13].try_into().ok()?), f64::from_be_bytes(b[13..21].try_into().ok()?)])
}

/// QVariant(QPainterPath), a user type: name "QPainterPath\0", element
/// count, then (type, x, y) per element and 8 bytes of fill data.
fn qpainterpath(b: &[u8]) -> Option<Vec<[f64; 2]>> {
    if b.len() < 9 || b[..4] != [0, 0, 4, 0] {
        return None;
    }
    let name_len = u32::from_be_bytes(b[5..9].try_into().ok()?) as usize;
    let count_at = 9usize.checked_add(name_len)?;
    if b.get(9..count_at)? != b"QPainterPath\0" {
        return None;
    }
    let count = u32::from_be_bytes(b.get(count_at..count_at + 4)?.try_into().ok()?) as usize;
    let start = count_at + 4;
    if count > MAX_COUNT || b.len() != start + count * 20 + 8 {
        return None;
    }
    let f = |at: usize| f64::from_be_bytes(b[at..at + 8].try_into().unwrap_or([0; 8]));
    Some((0..count).map(|i| [f(start + i * 20 + 4), f(start + i * 20 + 12)]).collect())
}

// ---------------------------------------------------------------------
// Notes: text, size and colour from Qt rich text
// ---------------------------------------------------------------------

fn looks_like_html(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with("<!DOCTYPE") || t.starts_with("<html") || s.contains("<body")
}

/// A note's plain text. PureRef 2 stores Qt rich text (HTML); 1.x
/// stores plain text.
fn note_text(raw: &str) -> String {
    if !looks_like_html(raw) {
        return raw.replace("\r\n", "\n").trim_end().to_string();
    }
    let mut body = raw;
    if let Some(i) = raw.find("<body") {
        let start = raw[i..].find('>').map_or(raw.len(), |j| i + j + 1);
        let end = raw[start..].find("</body>").map_or(raw.len(), |j| start + j);
        body = &raw[start..end];
    }
    let mut out = String::new();
    let mut rest = body;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            let end = rest.find('>').unwrap_or(rest.len());
            let tag = &rest[1..end];
            rest = &rest[(end + 1).min(rest.len())..];
            let closing = tag.starts_with('/');
            let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
            match name.as_str() {
                // Qt ends an empty paragraph with a <br />; it already
                // got its line from the <p>.
                "br" if !rest.starts_with("</p>") => out.push('\n'),
                "p" | "div" | "li" if !closing && !out.is_empty() => out.push('\n'),
                _ => {}
            }
        } else if c == '&' {
            let end = rest.find(';').filter(|&e| e <= 10);
            match end.and_then(|e| entity(&rest[1..e]).map(|ch| (e, ch))) {
                Some((e, ch)) => {
                    out.push(ch);
                    rest = &rest[e + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        } else {
            // Line breaks in the HTML source are layout, not text.
            if c != '\n' && c != '\r' {
                out.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    out.trim_end().to_string()
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let num = name.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

/// The innermost font size the note's HTML sets, in pixels.
fn html_font_px(html: &str) -> Option<f64> {
    if !looks_like_html(html) {
        return None;
    }
    let at = html.rfind("font-size:")?;
    let rest = html[at + "font-size:".len()..].trim_start();
    let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let value: f64 = rest[..num_len].parse().ok()?;
    let px = match rest[num_len..].trim_start() {
        u if u.starts_with("pt") => value * 4.0 / 3.0,
        _ => value,
    };
    (px > 0.0 && px.is_finite()).then_some(px)
}

/// The innermost text colour the note's HTML sets (not a background).
fn html_color(html: &str) -> Option<[u8; 4]> {
    if !looks_like_html(html) {
        return None;
    }
    let mut found = None;
    let mut from = 0;
    while let Some(i) = html[from..].find("color:") {
        let at = from + i;
        if !html[..at].ends_with('-') {
            let rest = html[at + "color:".len()..].trim_start();
            let end = rest.find(|c: char| !(c == '#' || c.is_ascii_hexdigit())).unwrap_or(rest.len());
            if let Some(c) = hex_color(&rest[..end]) {
                found = Some(c);
            }
        }
        from = at + "color:".len();
    }
    found
}

/// `#RRGGBB` or Qt's `#AARRGGBB`.
fn hex_color(s: &str) -> Option<[u8; 4]> {
    let hex = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(2)?, byte(4)?, byte(6)?, byte(0)?]),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Pictures: decode, crop, mirror, store
// ---------------------------------------------------------------------

/// One picture's placements, with its encoded file, for a worker.
type PictureJob = (Vec<u8>, Option<[u32; 2]>, Vec<RawPlacement>);
/// A slide item with its paint-order key.
type Placed = (Vec<f64>, BoardItem);

fn build<R: Read + Seek>(raw: RawBoard, fetch: &mut Fetch<'_, R>, store: &Store, progress: &Progress) -> Result<Board> {
    // Placements grouped by picture, so each is read and decoded once.
    let mut by_picture: Vec<Vec<RawPlacement>> = (0..raw.pictures.len()).map(|_| Vec::new()).collect();
    for p in raw.placements {
        if let Some(list) = by_picture.get_mut(p.picture) {
            list.push(p);
        }
    }
    let jobs: Vec<(RawPicture, Vec<RawPlacement>)> = raw.pictures.into_iter().zip(by_picture).filter(|(_, p)| !p.is_empty()).collect();
    progress.set(Stage::Pictures, 0, jobs.len() as u64);

    let items: Mutex<Vec<Placed>> = Mutex::new(Vec::new());
    let skipped = AtomicUsize::new(raw.skipped);
    let failed: Mutex<Option<crate::error::Error>> = Mutex::new(None);
    // Leave a core for the UI and one for this reading thread.
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(2).clamp(1, MAX_WORKERS);
    // This thread reads pictures out of the board (the database
    // connection cannot be shared) and hands them to the workers; the
    // bounded queue keeps only a few files in memory at once.
    let (tx, rx) = std::sync::mpsc::sync_channel::<PictureJob>(workers);
    let rx = Mutex::new(rx);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let Ok((bytes, shown, placements)) = lock(&rx).recv() else { break };
                if !progress.cancelled() {
                    match place_picture(&bytes, shown, &placements, store) {
                        Ok((placed, missed)) => {
                            lock(&items).extend(placed);
                            skipped.fetch_add(missed, Ordering::Relaxed);
                        }
                        // Storing failed (disk full, folder gone): stop.
                        Err(e) => {
                            lock(&failed).get_or_insert(e);
                            progress.cancel();
                        }
                    }
                }
                progress.advance(1);
            });
        }
        for (picture, placements) in jobs {
            if progress.cancelled() {
                break;
            }
            match fetch.bytes(&picture.at) {
                Ok(bytes) if !bytes.is_empty() => {
                    if tx.send((bytes, picture.shown, placements)).is_err() {
                        break;
                    }
                }
                other => {
                    if let Err(e) = other {
                        log::warn!("PureRef picture skipped: {e:#}");
                    }
                    skipped.fetch_add(placements.len(), Ordering::Relaxed);
                    progress.advance(1);
                }
            }
        }
        drop(tx);
    });
    if let Some(e) = failed.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    if progress.cancelled() {
        bail!("cancelled");
    }

    let mut items = items.into_inner().unwrap_or_else(|e| e.into_inner());
    for n in raw.notes {
        if let Some(item) = place_note(&n) {
            items.push((n.order, item));
        }
    }
    items.sort_by(|a, b| {
        for (x, y) in a.0.iter().zip(&b.0) {
            match x.partial_cmp(y) {
                Some(std::cmp::Ordering::Equal) | None => {}
                Some(o) => return o,
            }
        }
        // A group paints under its own children.
        a.0.len().cmp(&b.0.len())
    });
    Ok(Board { items: items.into_iter().map(|(_, item)| item).collect(), skipped: skipped.into_inner() })
}

struct Decoded {
    img: image::RgbaImage,
    format: Option<image::ImageFormat>,
    /// The pixels are the file's exactly as any decoder reads them (no
    /// EXIF turn applied), so the file can be stored as it is.
    as_stored: bool,
}

fn decode(data: &[u8], shown: Option<[u32; 2]>) -> Result<Decoded> {
    use image::ImageDecoder;
    let reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format().context("reading a picture")?;
    let format = reader.format();
    let mut decoder = reader.into_decoder().context("reading a picture")?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).context("decoding a picture")?;
    // A phone photo stored sideways shows upright only if PureRef turned
    // it, which the size PureRef recorded gives away; its crops and
    // transforms are relative to the picture as PureRef shows it.
    let mut as_stored = true;
    if let Some([w, h]) = shown {
        if w != h && [w, h] == [img.height(), img.width()] {
            img.apply_orientation(orientation);
            as_stored = false;
        }
    }
    Ok(Decoded { img: img.to_rgba8(), format, as_stored })
}

/// Which part of a picture one placement shows, and where.
struct Cut {
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    masked: bool,
    mirrored: bool,
    center: [f64; 2],
    half: [f64; 2],
    rotation: f64,
}

impl Cut {
    /// The placement shows the whole picture as it is.
    fn whole(&self, img: &image::RgbaImage) -> bool {
        !self.masked && !self.mirrored && [self.x0, self.y0, self.w, self.h] == [0, 0, img.width(), img.height()]
    }
}

fn cut_of(img: &image::RgbaImage, p: &RawPlacement) -> Option<Cut> {
    let (w, h) = (f64::from(img.width()), f64::from(img.height()));
    // Values within float noise of a whole pixel count as that pixel.
    let snap = |v: f64| if (v - v.round()).abs() < 1e-6 { v.round() } else { v };
    let (mut lo, mut hi) = ([0.0, 0.0], [w, h]);
    let mut masked = false;
    if !p.crop.is_empty() {
        lo = [f64::INFINITY; 2];
        hi = [f64::NEG_INFINITY; 2];
        for q in &p.crop {
            for k in 0..2 {
                lo[k] = lo[k].min(q[k]);
                hi[k] = hi[k].max(q[k]);
            }
        }
        // Anything but the plain rectangle needs the outline cut out: it
        // is that rectangle when every point is one of its corners and
        // it covers the rectangle's whole area (three corners make a
        // triangle).
        let corner = |q: &[f64; 2]| {
            let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
            (near(q[0], lo[0]) || near(q[0], hi[0])) && (near(q[1], lo[1]) || near(q[1], hi[1]))
        };
        let box_area = (hi[0] - lo[0]) * (hi[1] - lo[1]);
        masked = !(p.crop.iter().all(corner) && (polygon_area(&p.crop) - box_area).abs() <= 1e-6 * box_area.max(1.0));
    }
    let x0 = snap(lo[0]).floor().clamp(0.0, w) as u32;
    let y0 = snap(lo[1]).floor().clamp(0.0, h) as u32;
    let x1 = snap(hi[0]).ceil().clamp(0.0, w) as u32;
    let y1 = snap(hi[1]).ceil().clamp(0.0, h) as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mirrored = p.to_scene.det() < 0.0;

    // Pixel (u, v) of the crop is source pixel (x0 + u, y0 + v); a
    // mirrored crop reads its columns right to left.
    let (fw, fh) = (f64::from(cw), f64::from(ch));
    let mut a = Xf::translate(f64::from(x0), f64::from(y0)).then(p.to_scene);
    if mirrored {
        a = Xf { a: -1.0, b: 0.0, c: 0.0, d: 1.0, tx: fw, ty: 0.0 }.then(a);
    }
    let u = a.apply_vec([1.0, 0.0]);
    let u_len = u[0].hypot(u[1]);
    if u_len.is_nan() || u_len <= 1e-9 {
        return None;
    }
    // Height across the width axis, so a sheared picture keeps its area.
    let v_len = a.det().abs() / u_len;
    let center = a.apply([fw * 0.5, fh * 0.5]);
    let half = [fw * 0.5 * u_len, fh * 0.5 * v_len];
    if !(center.iter().chain(&half).all(|v| v.is_finite()) && half[1] > 0.0) {
        return None;
    }
    Some(Cut { x0, y0, w: cw, h: ch, masked, mirrored, center, half, rotation: u[1].atan2(u[0]) })
}

/// What one stored cut of a picture became: its file, and the preview
/// the slide keeps.
#[derive(Clone)]
struct Stored {
    picture: PictureRef,
    preview: Arc<Vec<u8>>,
    preview_size: [u32; 2],
}

/// Turn one picture's placements into slide items, storing what they
/// show in the media folder. Returns the items and how many placements
/// could not be shown. An error means the media folder could not be
/// written, which stops the whole open.
fn place_picture(bytes: &[u8], shown: Option<[u32; 2]>, placements: &[RawPlacement], store: &Store) -> Result<(Vec<Placed>, usize)> {
    let decoded = match decode(bytes, shown) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("PureRef picture skipped: {e:#}");
            return Ok((Vec::new(), placements.len()));
        }
    };
    let img = &decoded.img;
    // An animated GIF plays where it is shown whole; a crop or mirror of
    // one is a still of its first frame, as the player cannot show those.
    let animation = (decoded.format == Some(image::ImageFormat::Gif))
        .then(|| super::gif_player::scan_reader(std::io::Cursor::new(bytes)).ok())
        .flatten()
        .filter(|s| s.frames > 1);
    let mut gif: Option<(MediaRef, Arc<Vec<u8>>, [u32; 2])> = None;
    let mut original: Option<String> = None;
    let mut made: HashMap<(u32, u32, u32, u32, bool), Stored> = HashMap::new();
    let mut out = Vec::new();
    let mut missed = 0;
    for p in placements {
        let Some(cut) = cut_of(img, p) else {
            missed += 1;
            continue;
        };
        let item = match animation {
            Some(scan) if cut.whole(img) && decoded.as_stored => {
                if gif.is_none() {
                    let file = store.put(bytes, "gif")?;
                    // The poster only shows until the GIF starts playing,
                    // so it can be small; a board may hold many GIFs.
                    let poster = shrink(img.clone(), GIF_POSTER_SIDE);
                    let meta = MediaMeta { frame_rate: [0, 1], frames: scan.frames, duration: scan.duration, size: scan.size };
                    let media = MediaRef { file, kind: MediaKind::Gif, source_name: String::new(), meta: Some(meta), id: 0 };
                    gif = Some((media, Arc::new(encode_picture(&poster, 85)?.0), [poster.width(), poster.height()]));
                }
                let Some((media, poster, size)) = &gif else { unreachable!("set just above") };
                let mut media = media.clone();
                media.id = fresh_media_id();
                BoardItem::Picture {
                    png: Arc::clone(poster),
                    px_size: *size,
                    picture: None,
                    media: Some(media),
                    center: cut.center,
                    half: cut.half,
                    rotation: cut.rotation,
                }
            }
            _ => {
                let key = (cut.x0, cut.y0, cut.w, cut.h, cut.mirrored);
                let stored = match made.get(&key) {
                    Some(s) if !cut.masked => s.clone(),
                    _ => {
                        let s = store_cut(bytes, &decoded, &cut, &p.crop, store, &mut original)?;
                        if !cut.masked {
                            made.insert(key, s.clone());
                        }
                        s
                    }
                };
                BoardItem::Picture {
                    png: stored.preview,
                    px_size: stored.preview_size,
                    picture: Some(stored.picture),
                    media: None,
                    center: cut.center,
                    half: cut.half,
                    rotation: cut.rotation,
                }
            }
        };
        out.push((p.order.clone(), item));
    }
    Ok((out, missed))
}

/// Store what `cut` shows of a picture. Usually that is the original
/// file, stored once (`original` remembers its name) and shared by every
/// crop of it, with the crop and mirror recorded for loading. A cut-out
/// outline, a photo turned upright, or a format the app cannot read
/// again is re-encoded instead (JPEG when opaque, else PNG) at most
/// `IMAGE_SIDE` pixels.
fn store_cut(bytes: &[u8], decoded: &Decoded, cut: &Cut, outline: &[[f64; 2]], store: &Store, original: &mut Option<String>) -> Result<Stored> {
    use image::ImageFormat as F;
    let img = &decoded.img;
    let original_ext = match decoded.format {
        Some(F::Png) => Some("png"),
        Some(F::Jpeg) => Some("jpg"),
        Some(F::WebP) => Some("webp"),
        Some(F::Bmp) => Some("bmp"),
        Some(F::Gif) => Some("gif"),
        _ => None,
    };
    let (picture, preview) = match original_ext.filter(|_| !cut.masked && decoded.as_stored) {
        Some(ext) => {
            let file = match original {
                Some(f) => f.clone(),
                None => original.insert(store.put(bytes, ext)?).clone(),
            };
            let all = [cut.x0, cut.y0, cut.w, cut.h] == [0, 0, img.width(), img.height()];
            let picture = PictureRef { file, size: [cut.w, cut.h], crop: (!all).then_some([cut.x0, cut.y0, cut.w, cut.h]), mirror: cut.mirrored };
            let preview = picture.view(img, PREVIEW_SIDE);
            (picture, preview)
        }
        None => {
            let mut sub = image::imageops::crop_imm(img, cut.x0, cut.y0, cut.w, cut.h).to_image();
            if cut.masked {
                for (x, y, px) in sub.enumerate_pixels_mut() {
                    if !inside(outline, f64::from(cut.x0 + x) + 0.5, f64::from(cut.y0 + y) + 0.5) {
                        px[3] = 0;
                    }
                }
            }
            // Pictures on a slide cannot be mirrored, so the pixels are.
            if cut.mirrored {
                image::imageops::flip_horizontal_in_place(&mut sub);
            }
            let sub = shrink(sub, IMAGE_SIDE);
            let (encoded, ext) = encode_picture(&sub, 92)?;
            let picture = PictureRef { file: store.put(&encoded, ext)?, size: [sub.width(), sub.height()], crop: None, mirror: false };
            (picture, preview_of(&sub))
        }
    };
    let preview_size = [preview.width(), preview.height()];
    let (preview, _) = encode_picture(&preview, 80)?;
    Ok(Stored { picture, preview: Arc::new(preview), preview_size })
}

fn preview_of(img: &image::RgbaImage) -> image::RgbaImage {
    let [w, h] = fit_side([img.width(), img.height()], PREVIEW_SIDE);
    image::imageops::thumbnail(img, w, h)
}

/// JPEG at `quality` for an opaque picture (a fraction of the PNG size
/// for photos), PNG when any pixel is see-through. Everything that reads
/// a raster's bytes sniffs the format, so a preview may be either.
fn encode_picture(img: &image::RgbaImage, quality: u8) -> Result<(Vec<u8>, &'static str)> {
    if img.pixels().all(|p| p[3] == 255) {
        let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).context("encoding a picture")?;
        Ok((out, "jpg"))
    } else {
        Ok((encode_png(img)?, "png"))
    }
}

/// Unsigned area (shoelace); the outline may or may not repeat its
/// first point at the end.
fn polygon_area(poly: &[[f64; 2]]) -> f64 {
    let mut twice = 0.0;
    for (i, p) in poly.iter().enumerate() {
        let q = poly[(i + 1) % poly.len()];
        twice += p[0] * q[1] - q[0] * p[1];
    }
    (twice * 0.5).abs()
}

/// Even-odd point-in-polygon.
fn inside(poly: &[[f64; 2]], x: f64, y: f64) -> bool {
    let mut odd = false;
    let mut j = poly.len().wrapping_sub(1);
    for (i, pi) in poly.iter().enumerate() {
        let pj = poly[j];
        if (pi[1] > y) != (pj[1] > y) && x < (pj[0] - pi[0]) * (y - pi[1]) / (pj[1] - pi[1]) + pi[0] {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn place_note(n: &RawNote) -> Option<BoardItem> {
    if n.text.trim().is_empty() {
        return None;
    }
    let text = match n.width {
        Some(width) => wrap_note(&n.text, width, n.font_px),
        None => n.text.clone(),
    };
    let [w, h] = text_extent(&text, n.font_px as f32).map(f64::from);
    let w = n.width.unwrap_or(w);
    let local = if n.centred { [-w * 0.5, -h * 0.5] } else { [0.0, 0.0] };
    let pos = n.to_scene.apply(local);
    let u = n.to_scene.apply_vec([1.0, 0.0]);
    let font_size = n.font_px * n.to_scene.det().abs().sqrt();
    if !(pos.iter().all(|v| v.is_finite()) && font_size > 0.0 && font_size.is_finite()) {
        return None;
    }
    Some(BoardItem::Note { text, pos, font_size, color: n.color, rotation: u[1].atan2(u[0]) })
}

/// Word-wrap `text` to lines about `width` note units long at
/// `font_px`, as PureRef wraps a note given a fixed width. Text shapes
/// do not wrap themselves, so the breaks go into the text. Character
/// widths are estimated (a little over half the font size), like
/// `text_extent` does; a word longer than a line is broken.
fn wrap_note(text: &str, width: f64, font_px: f64) -> String {
    let per_line = ((width / (font_px * 0.55)).floor() as usize).max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for mut word in para.split(' ') {
            loop {
                let (wl, ll) = (word.chars().count(), line.chars().count());
                if (if ll == 0 { wl } else { ll + 1 + wl }) <= per_line {
                    if ll > 0 {
                        line.push(' ');
                    }
                    line.push_str(word);
                    break;
                }
                if ll > 0 {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                let head: String = word.chars().take(per_line).collect();
                word = &word[head.len()..];
                lines.push(head);
                if word.is_empty() {
                    break;
                }
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    /// A scratch folder, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("osn-pureref-test-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn files(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&self.0).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A `w`×`h` PNG, red on the left half and blue on the right.
    fn two_tone_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, _| image::Rgba(if x < w / 2 { RED } else { BLUE }));
        encode_png(&img).unwrap()
    }

    /// A two-frame animated GIF: red, then blue.
    fn animated_gif(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            let frames = [RED, BLUE].map(|c| {
                image::Frame::from_parts(image::RgbaImage::from_pixel(w, h, image::Rgba(c)), 0, 0, image::Delay::from_numer_denom_ms(100, 1))
            });
            enc.encode_frames(frames).unwrap();
        }
        out
    }

    fn open_bytes_with(bytes: Vec<u8>, db_in_memory_max: u64) -> (Board, TempDir) {
        let media = TempDir::new("media");
        let len = bytes.len() as u64;
        let board = open_from(std::io::Cursor::new(bytes), len, &media.0, &Progress::default(), db_in_memory_max).unwrap();
        (board, media)
    }

    fn open_bytes(bytes: Vec<u8>) -> (Board, TempDir) {
        open_bytes_with(bytes, DB_IN_MEMORY_MAX)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn assert_close(got: [f64; 2], want: [f64; 2]) {
        assert!(close(got[0], want[0]) && close(got[1], want[1]), "got {got:?}, want {want:?}");
    }

    /// Within JPEG's error of `want`.
    fn near(got: [u8; 4], want: [u8; 4]) -> bool {
        got.iter().zip(want).all(|(g, w)| (i16::from(*g) - i16::from(w)).abs() <= 12)
    }

    /// QTransform: `scale` then turn by `angle` (clockwise on screen),
    /// then move to (x, y). Qt's 9-double order.
    fn qt(sx: f64, sy: f64, angle: f64, x: f64, y: f64) -> [f64; 9] {
        let (s, c) = angle.sin_cos();
        [c * sx, s * sx, 0.0, -s * sy, c * sy, 0.0, x, y, 1.0]
    }

    fn move_by(x: f64, y: f64) -> [f64; 9] {
        qt(1.0, 1.0, 0.0, x, y)
    }

    /// Closed outline of the rectangle (x0, y0)–(x1, y1).
    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<[f64; 2]> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]
    }

    struct Pic<'a> {
        png: &'a Arc<Vec<u8>>,
        picture: Option<&'a PictureRef>,
        media: Option<&'a MediaRef>,
        center: [f64; 2],
        half: [f64; 2],
        rotation: f64,
    }

    fn picture(item: &BoardItem) -> Pic<'_> {
        match item {
            BoardItem::Picture { png, picture, media, center, half, rotation, .. } => {
                Pic { png, picture: picture.as_ref(), media: media.as_ref(), center: *center, half: *half, rotation: *rotation }
            }
            BoardItem::Note { .. } => panic!("expected a picture"),
        }
    }

    /// What a picture item shows, loaded from its stored file the way
    /// the app loads it.
    fn stored(media: &TempDir, item: &BoardItem) -> (PictureRef, image::RgbaImage) {
        let p = picture(item).picture.expect("a stored picture").clone();
        let img = image::open(media.0.join(&p.file)).unwrap().to_rgba8();
        let shown = p.view(&img, u32::MAX);
        (p, shown)
    }

    fn note(item: &BoardItem) -> (&str, [f64; 2], f64, [u8; 4], f64) {
        match item {
            BoardItem::Note { text, pos, font_size, color, rotation } => (text, *pos, *font_size, *color, *rotation),
            BoardItem::Picture { .. } => panic!("expected a note"),
        }
    }

    // -----------------------------------------------------------------
    // A small 1.x writer, following the layout the reader expects
    // -----------------------------------------------------------------

    enum Slot {
        Png(Vec<u8>),
        /// Shares the PNG of the item with this id.
        Same(u32),
        /// Linked from disk, not stored.
        Linked,
    }

    enum V1 {
        Image { id: u32, xf: [f64; 9], z: f64, pre: [f64; 9], outline: Vec<[f64; 2]>, children: Vec<V1>, extra: usize },
        Text { id: u32, text: String, xf: [f64; 9], z: f64, fg: [u16; 4], children: Vec<V1> },
    }

    /// An image item showing a `w`×`h` picture centred on its origin,
    /// uncropped, as PureRef places a fresh picture.
    fn image(id: u32, w: f64, h: f64, xf: [f64; 9], z: f64) -> V1 {
        V1::Image { id, xf, z, pre: move_by(-w / 2.0, -h / 2.0), outline: rect(-w / 2.0, -h / 2.0, w / 2.0, h / 2.0), children: Vec::new(), extra: 0 }
    }

    struct W(Vec<u8>);

    impl W {
        fn u8(&mut self, v: u8) { self.0.push(v); }
        fn u16(&mut self, v: u16) { self.0.extend(v.to_be_bytes()); }
        fn u32(&mut self, v: u32) { self.0.extend(v.to_be_bytes()); }
        fn u64(&mut self, v: u64) { self.0.extend(v.to_be_bytes()); }
        fn f64(&mut self, v: f64) { self.0.extend(v.to_be_bytes()); }
        fn qstring(&mut self, s: &str) {
            let units: Vec<u16> = s.encode_utf16().collect();
            self.u32(units.len() as u32 * 2);
            for u in units { self.u16(u); }
        }
        fn doubles(&mut self, v: &[f64]) { for &d in v { self.f64(d); } }

        fn item(&mut self, item: &V1) {
            let at = self.0.len();
            self.u64(0); // end, patched below
            let children = match item {
                V1::Image { id, xf, z, pre, outline, children, extra } => {
                    self.qstring(V1_IMAGE);
                    self.qstring("C:/refs/pic.png");
                    self.qstring("pic");
                    self.f64(1.0);
                    self.doubles(xf);
                    self.u32(*id);
                    self.f64(*z);
                    self.doubles(pre);
                    self.u32(outline.len() as u32);
                    for (i, p) in outline.iter().enumerate() {
                        self.u32(u32::from(i > 0));
                        self.doubles(p);
                    }
                    self.f64(0.0);
                    self.u32(1);
                    self.u8(0);
                    self.u64(u64::MAX);
                    self.u32(children.len() as u32);
                    // Fields a newer PureRef appends: skipped by `end`.
                    self.0.extend(std::iter::repeat(0xAB).take(*extra));
                    children
                }
                V1::Text { id, text, xf, z, fg, children } => {
                    self.qstring(V1_TEXT);
                    self.qstring(text);
                    self.doubles(xf);
                    self.u32(*id);
                    self.f64(*z);
                    self.u8(1);
                    for c in fg { self.u16(*c); }
                    self.u16(0);
                    self.u8(1);
                    for _ in 0..5 { self.u16(0); }
                    self.u32(children.len() as u32);
                    children
                }
            };
            let end = self.0.len() as u64;
            self.0[at..at + 8].copy_from_slice(&end.to_be_bytes());
            for c in children { self.item(c); }
        }
    }

    fn v1_board(slots: &[(u32, Slot)], items: &[V1]) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.u32(8);
        for u in "1.10".encode_utf16() { w.u16(u); }
        w.u16(items.len() as u16);
        w.u16(slots.len() as u16);
        w.u64(0); // table offset, patched below
        w.qstring("1.11.1");
        w.u32(64);
        w.0.extend([0u8; 64]);
        w.0.extend([0u8; 116]);
        assert_eq!(w.0.len(), 224);
        let mut ranges = Vec::new();
        for (id, slot) in slots {
            let start = w.0.len() as u64;
            match slot {
                Slot::Png(png) => w.0.extend(png),
                Slot::Same(owner) => w.u32(*owner),
                Slot::Linked => w.u32(u32::MAX),
            }
            ranges.push((*id, start, w.0.len() as u64));
        }
        for item in items { w.item(item); }
        w.qstring(""); // folder
        let table = w.0.len() as u64;
        w.0[16..24].copy_from_slice(&table.to_be_bytes());
        for (id, start, end) in ranges {
            w.u32(id);
            w.u64(start);
            w.u64(end);
        }
        w.0
    }

    #[test]
    fn v1_places_turned_scaled_pictures_and_shares_duplicates() {
        let png = two_tone_png(40, 20);
        let (board, media) = open_bytes(v1_board(
            &[(1, Slot::Png(png.clone())), (2, Slot::Same(1)), (3, Slot::Linked)],
            &[
                image(1, 40.0, 20.0, qt(2.0, 2.0, FRAC_PI_2, 100.0, 50.0), 2.0),
                image(2, 40.0, 20.0, move_by(-300.0, 0.0), 1.0),
                image(3, 40.0, 20.0, move_by(0.0, 0.0), 3.0),
            ],
        ));
        assert_eq!(board.skipped, 1, "the linked picture");
        assert_eq!(board.items.len(), 2);
        // z 1 paints under z 2.
        let a = picture(&board.items[0]);
        assert_close(a.center, [-300.0, 0.0]);
        assert_close(a.half, [20.0, 10.0]);
        assert!(close(a.rotation, 0.0));
        let b = picture(&board.items[1]);
        assert_close(b.center, [100.0, 50.0]);
        assert_close(b.half, [40.0, 20.0]);
        assert!(close(b.rotation, FRAC_PI_2), "{}", b.rotation);
        let (fa, fb) = (a.picture.unwrap(), b.picture.unwrap());
        assert_eq!(fa, fb, "a duplicate shares one stored file");
        assert_eq!(fa.size, [40, 20]);
        assert_eq!(std::fs::read(media.0.join(&fa.file)).unwrap(), png, "a picture shown whole keeps its original file");
        assert_eq!(media.files(), vec![fa.file.clone()]);
        let preview = image::load_from_memory(a.png).unwrap();
        assert!(preview.width().max(preview.height()) <= PREVIEW_SIDE);
    }

    #[test]
    fn v1_crops_mirrors_and_cuts_outlines() {
        let cropped = V1::Image {
            id: 1,
            xf: move_by(0.0, 0.0),
            z: 1.0,
            pre: move_by(-20.0, -10.0),
            // The left half, in item space.
            outline: rect(-20.0, -10.0, 0.0, 10.0),
            children: Vec::new(),
            extra: 6,
        };
        let mirrored = image(2, 40.0, 20.0, qt(-1.0, 1.0, 0.0, 500.0, 0.0), 2.0);
        let triangle = V1::Image {
            id: 3,
            xf: move_by(0.0, 300.0),
            z: 3.0,
            pre: move_by(-20.0, -10.0),
            outline: vec![[-20.0, -10.0], [20.0, -10.0], [-20.0, 10.0], [-20.0, -10.0]],
            children: Vec::new(),
            extra: 0,
        };
        let png = two_tone_png(40, 20);
        let (board, media) = open_bytes(v1_board(&[(1, Slot::Png(png.clone())), (2, Slot::Same(1)), (3, Slot::Same(1))], &[cropped, mirrored, triangle]));
        assert_eq!((board.items.len(), board.skipped), (3, 0));

        let (crop, img) = stored(&media, &board.items[0]);
        assert_eq!((crop.crop, crop.mirror, crop.size), (Some([0, 0, 20, 20]), false, [20, 20]));
        assert_eq!(std::fs::read(media.0.join(&crop.file)).unwrap(), png, "a crop keeps the original file");
        assert_eq!(img.dimensions(), (20, 20));
        assert!(img.pixels().all(|p| p.0 == RED), "only the red half is shown");
        let p = picture(&board.items[0]);
        assert_close(p.center, [-10.0, 0.0]);
        assert_close(p.half, [10.0, 10.0]);

        let (mirror, img) = stored(&media, &board.items[1]);
        assert_eq!((mirror.file.as_str(), mirror.crop, mirror.mirror), (crop.file.as_str(), None, true), "the same file, shown mirrored");
        assert_eq!((img.get_pixel(0, 0).0, img.get_pixel(39, 0).0), (BLUE, RED), "pixels mirrored");
        let p = picture(&board.items[1]);
        assert!(close(p.rotation, 0.0), "the mirror is in the pixels, not a turn: {}", p.rotation);
        assert_close(p.center, [500.0, 0.0]);
        assert_close(p.half, [20.0, 10.0]);
        let preview = image::load_from_memory(p.png).unwrap().to_rgba8();
        assert!(near(preview.get_pixel(0, 0).0, BLUE), "the preview is mirrored too");

        let (cutout, img) = stored(&media, &board.items[2]);
        assert!(cutout.file.ends_with(".png") && cutout.crop.is_none(), "a cut-out is its own PNG: {cutout:?}");
        assert_eq!(media.files().len(), 2, "the original and the cut-out");
        assert_eq!(img.dimensions(), (40, 20));
        assert_eq!(img.get_pixel(1, 1).0[3], 255, "inside the triangle");
        assert_eq!(img.get_pixel(38, 18).0[3], 0, "outside the triangle");
    }

    #[test]
    fn v1_notes_and_children_follow_their_parent() {
        let mut parent = image(1, 40.0, 20.0, move_by(100.0, 50.0), 1.0);
        if let V1::Image { children, .. } = &mut parent {
            children.push(V1::Text {
                id: 2,
                text: "Hello\nWorld".into(),
                xf: qt(2.0, 2.0, 0.0, 10.0, 0.0),
                z: 0.5,
                fg: [0xFFFF, 0xFFFF, 0, 0],
                children: Vec::new(),
            });
        }
        let (board, _media) = open_bytes(v1_board(&[(1, Slot::Png(two_tone_png(40, 20)))], &[parent]));
        assert_eq!(board.counts(), (1, 1));
        // The child note paints above its parent picture.
        let (text, pos, font_size, color, rotation) = note(&board.items[1]);
        assert_eq!(text, "Hello\nWorld");
        assert_close(pos, [110.0, 50.0]);
        assert!(close(font_size, NOTE_FONT_PX * 2.0));
        assert_eq!(color, [255, 0, 0, 255]);
        assert!(close(rotation, 0.0));
    }

    // -----------------------------------------------------------------
    // 2.x: a real SQLite database in PureRef's container
    // -----------------------------------------------------------------

    fn qt_blob(m: [f64; 9]) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 0x50, 0];
        for v in m { b.extend(v.to_be_bytes()); }
        b
    }

    fn path_blob(points: &[[f64; 2]]) -> Vec<u8> {
        let mut b = vec![0, 0, 4, 0, 0, 0, 0, 0, 13];
        b.extend(b"QPainterPath\0");
        b.extend((points.len() as u32).to_be_bytes());
        for (i, p) in points.iter().enumerate() {
            b.extend(u32::from(i > 0).to_be_bytes());
            b.extend(p[0].to_be_bytes());
            b.extend(p[1].to_be_bytes());
        }
        b.extend([0u8; 8]);
        b
    }

    /// The bytes as TEXT, one character per byte, as older PureRef 2
    /// files store them.
    fn wrapped(b: &[u8]) -> String {
        b.iter().map(|&c| char::from(c)).collect()
    }

    fn v2_container(db: &[u8]) -> Vec<u8> {
        let thumb = [0xFF, 0xD8, 1, 2, 3, 4, 5, 6, 0xFF, 0xD9];
        let front = 108 + thumb.len();
        assert!(db.len() > front);
        let mut out = vec![0u8; front + db.len()];
        out[0..4].copy_from_slice(&6u32.to_be_bytes());
        for (i, u) in "2.1".encode_utf16().enumerate() {
            out[4 + i * 2..6 + i * 2].copy_from_slice(&u.to_be_bytes());
        }
        out[14..22].copy_from_slice(&(db.len() as u64).to_be_bytes());
        out[22..26].copy_from_slice(&10u32.to_be_bytes());
        for (i, u) in "2.0.3".encode_utf16().enumerate() {
            out[26 + i * 2..28 + i * 2].copy_from_slice(&u.to_be_bytes());
        }
        out[36..40].copy_from_slice(&64u32.to_be_bytes());
        out[104..108].copy_from_slice(&(thumb.len() as u32).to_be_bytes());
        out[108..front].copy_from_slice(&thumb);
        out[front..db.len()].copy_from_slice(&db[front..]);
        out[db.len()..].copy_from_slice(&db[..front]);
        out
    }

    fn v2_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE TABLE images (id INTEGER PRIMARY KEY, source_type INTEGER, origin TEXT, source TEXT, format TEXT, checksum TEXT, data BLOB, width INTEGER, height INTEGER);
             CREATE TABLE items (parent INTEGER, id INTEGER PRIMARY KEY, name TEXT, transform BLOB, sort_order BLOB, z REAL, opacity REAL, locked INTEGER, comment INTEGER);
             CREATE TABLE items_images (image INTEGER, playback_speed REAL, id INTEGER PRIMARY KEY, playback_state INTEGER, image_transform BLOB, image_bounds BLOB, playback_frame INTEGER, flags INTEGER);
             CREATE TABLE items_notes (text_color TEXT, id INTEGER PRIMARY KEY, fixed_size TEXT, background_color TEXT, text TEXT, style INTEGER);
             CREATE TABLE items_groups (id INTEGER PRIMARY KEY, background_color TEXT, lock_mode INTEGER);",
        )
        .unwrap();
        conn
    }

    /// Place picture `image` (w×h) as item `id`, cropped to `outline`
    /// (item space) or shown whole.
    #[allow(clippy::too_many_arguments)]
    fn v2_place(conn: &rusqlite::Connection, id: i64, image: i64, w: f64, h: f64, xf: [f64; 9], z: f64, outline: Option<Vec<[f64; 2]>>) {
        use rusqlite::params;
        conn.execute("INSERT INTO items (parent, id, transform, z) VALUES (-1, ?1, ?2, ?3)", params![id, qt_blob(xf), z]).unwrap();
        let outline = outline.unwrap_or_else(|| rect(-w / 2.0, -h / 2.0, w / 2.0, h / 2.0));
        conn.execute(
            "INSERT INTO items_images (image, id, image_transform, image_bounds) VALUES (?1, ?2, ?3, ?4)",
            params![image, id, qt_blob(move_by(-w / 2.0, -h / 2.0)), path_blob(&outline)],
        )
        .unwrap();
    }

    fn v2_board() -> Vec<u8> {
        use rusqlite::params;
        let conn = v2_db();
        conn.execute("INSERT INTO images (id, format, data, width, height) VALUES (1, 'PNG', ?1, 40, 20)", params![two_tone_png(40, 20)]).unwrap();
        // A picture linked from disk: no data.
        conn.execute("INSERT INTO images (id, format, data, width, height) VALUES (2, 'PNG', NULL, 40, 20)", []).unwrap();
        // A group moved right, holding a turned picture; its transform is
        // stored the older, TEXT-wrapped way.
        conn.execute("INSERT INTO items (parent, id, transform, z) VALUES (-1, 10, ?1, 0)", params![qt_blob(move_by(1000.0, 0.0))]).unwrap();
        conn.execute("INSERT INTO items_groups (id) VALUES (10)", []).unwrap();
        conn.execute(
            "INSERT INTO items (parent, id, transform, z) VALUES (10, 1, CAST(?1 AS TEXT), 1)",
            params![wrapped(&qt_blob(qt(1.0, 1.0, FRAC_PI_2, 0.0, 0.0)))],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items_images (image, id, image_transform, image_bounds) VALUES (1, 1, ?1, CAST(?2 AS TEXT))",
            params![qt_blob(move_by(-20.0, -10.0)), wrapped(&path_blob(&rect(-20.0, -10.0, 20.0, 10.0)))],
        )
        .unwrap();
        v2_place(&conn, 3, 2, 40.0, 20.0, move_by(0.0, 0.0), 2.0, None);
        let html = "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.0//EN\">\n<html><head><meta name=\"qrichtext\" content=\"1\" /></head>\
                    <body style=\"font-family:'Open Sans'; font-size:22px;\">\n<p style=\"margin:0px;\"><span style=\"font-size:30px; color:#00ff00;\">Hi &amp; bye</span></p>\n\
                    <p style=\"-qt-paragraph-type:empty;\"><br /></p>\n<p><span style=\"font-size:30px; color:#00ff00;\">line 3</span></p></body></html>";
        conn.execute("INSERT INTO items (parent, id, transform, z) VALUES (-1, 2, ?1, 5)", params![qt_blob(move_by(0.0, 0.0))]).unwrap();
        conn.execute("INSERT INTO items_notes (id, background_color, text) VALUES (2, '#d9ffffff', ?1)", params![html]).unwrap();
        v2_container(&conn.serialize(rusqlite::MAIN_DB).unwrap())
    }

    fn check_v2_board(board: &Board, media: &TempDir) {
        assert_eq!(board.skipped, 1, "the linked picture");
        assert_eq!(board.counts(), (1, 1));
        let p = picture(&board.items[0]);
        assert_eq!(p.picture.unwrap().size, [40, 20]);
        assert!(media.0.join(&p.picture.unwrap().file).is_file());
        assert_close(p.center, [1000.0, 0.0]);
        assert_close(p.half, [20.0, 10.0]);
        assert!(close(p.rotation, FRAC_PI_2));

        let (text, pos, font_size, color, _) = note(&board.items[1]);
        assert_eq!(text, "Hi & bye\n\nline 3");
        assert!(close(font_size, 30.0));
        assert_eq!(color, [0, 255, 0, 255]);
        // A 2.x note is centred on its origin.
        let [w, h] = text_extent(text, 30.0).map(f64::from);
        assert_close(pos, [-w / 2.0, -h / 2.0]);
    }

    #[test]
    fn v2_reads_groups_wrapped_values_and_rich_text() {
        let (board, media) = open_bytes(v2_board());
        check_v2_board(&board, &media);
    }

    /// A big board's database goes through a temporary file instead of
    /// memory, with the same result.
    #[test]
    fn v2_big_boards_read_through_a_temporary_file() {
        let (board, media) = open_bytes_with(v2_board(), 0);
        check_v2_board(&board, &media);
    }

    #[test]
    fn v2_animated_gifs_play_and_crops_of_them_are_stills() {
        use rusqlite::params;
        let conn = v2_db();
        let gif = animated_gif(16, 8);
        conn.execute("INSERT INTO images (id, format, data, width, height) VALUES (1, 'gif', ?1, 16, 8)", params![gif]).unwrap();
        v2_place(&conn, 1, 1, 16.0, 8.0, move_by(0.0, 0.0), 1.0, None);
        v2_place(&conn, 2, 1, 16.0, 8.0, move_by(100.0, 0.0), 2.0, Some(rect(-8.0, -4.0, 0.0, 4.0)));
        let (board, media) = open_bytes(v2_container(&conn.serialize(rusqlite::MAIN_DB).unwrap()));
        assert_eq!((board.items.len(), board.skipped), (2, 0));

        let whole = picture(&board.items[0]);
        let m = whole.media.expect("shown whole, the GIF plays");
        assert!(whole.picture.is_none());
        assert_eq!((m.kind, m.meta.map(|x| x.frames)), (MediaKind::Gif, Some(2)));
        assert_eq!(std::fs::read(media.0.join(&m.file)).unwrap(), gif, "the GIF is stored as it is");
        assert_close(whole.half, [8.0, 4.0]);

        let crop = picture(&board.items[1]);
        assert!(crop.media.is_none());
        let (_, img) = stored(&media, &board.items[1]);
        assert_eq!(img.dimensions(), (8, 8));
        assert!(img.pixels().all(|p| near(p.0, RED)), "a still of the first frame");
    }

    #[test]
    fn cancelling_stops_and_removes_what_was_stored() {
        let media = TempDir::new("cancel");
        let bytes = v1_board(&[(1, Slot::Png(two_tone_png(40, 20)))], &[image(1, 40.0, 20.0, move_by(0.0, 0.0), 1.0)]);
        let progress = Progress::default();
        progress.cancel();
        let len = bytes.len() as u64;
        let err = open_from(std::io::Cursor::new(bytes), len, &media.0, &progress, DB_IN_MEMORY_MAX).err().expect("cancelled");
        assert_eq!(format!("{err}"), "cancelled");
        assert!(media.files().is_empty());
    }

    #[test]
    fn progress_counts_pictures() {
        let media = TempDir::new("progress");
        let bytes = v1_board(
            &[(1, Slot::Png(two_tone_png(40, 20))), (2, Slot::Png(two_tone_png(10, 10)))],
            &[image(1, 40.0, 20.0, move_by(0.0, 0.0), 1.0), image(2, 10.0, 10.0, move_by(50.0, 0.0), 2.0)],
        );
        let progress = Progress::default();
        let len = bytes.len() as u64;
        open_from(std::io::Cursor::new(bytes), len, &media.0, &progress, DB_IN_MEMORY_MAX).unwrap();
        assert_eq!(progress.get(), (Stage::Pictures, 2, 2));
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        let media = TempDir::new("damaged");
        let try_open = |b: Vec<u8>| {
            let len = b.len() as u64;
            let _ = open_from(std::io::Cursor::new(b), len, &media.0, &Progress::default(), DB_IN_MEMORY_MAX);
        };
        for good in [
            v1_board(&[(1, Slot::Png(two_tone_png(8, 8)))], &[image(1, 8.0, 8.0, move_by(0.0, 0.0), 1.0)]),
            v2_board(),
        ] {
            for cut in (0..good.len()).step_by(7).chain([good.len() - 1]) {
                try_open(good[..cut].to_vec());
            }
            let mut garbled = good.clone();
            for (i, b) in garbled.iter_mut().enumerate().skip(12) {
                *b ^= (i * 31 % 251) as u8;
            }
            try_open(garbled);
        }
        let len = 24;
        assert!(open_from(std::io::Cursor::new(b"PNG\x89 not a board at all".to_vec()), len, &media.0, &Progress::default(), 0).is_err());
        assert!(open_from(std::io::Cursor::new(Vec::new()), 0, &media.0, &Progress::default(), 0).is_err());
    }

    #[test]
    fn fixed_width_notes_wrap() {
        // 10 px font: about 5.5 px a character, so 40 units hold 7 and
        // 30 hold 5.
        assert_eq!(wrap_note("rich elegant house on top", 40.0, 10.0), "rich\nelegant\nhouse\non top");
        assert_eq!(wrap_note("a b c\n\nlongerword", 30.0, 10.0), "a b c\n\nlonge\nrword");
        assert_eq!(qsizef(&[0, 0, 0, 0x16, 0, 0x40, 0x60, 0x60, 0, 0, 0, 0, 0, 0xBF, 0xF0, 0, 0, 0, 0, 0, 0]), Some([131.0, -1.0]));
    }

    #[test]
    fn html_notes_become_plain_text() {
        assert_eq!(note_text("plain <b> text\r\nline"), "plain <b> text\nline");
        assert_eq!(note_text("<html><body><p>a&lt;b &#65;&#x42;</p><p>c<br />d</p></body></html>"), "a<b AB\nc\nd");
        assert_eq!(html_font_px("<html><body style=\"font-size:9pt\">x</body></html>"), Some(12.0));
        assert_eq!(html_color("<html><body><span style=\"background-color:#ff0000\">x</span></body></html>"), None);
        assert_eq!(hex_color("#80102030"), Some([0x10, 0x20, 0x30, 0x80]));
        assert_eq!(hsv_to_rgba(12000, 65535, 65535, 65535), [0, 255, 0, 255]);
    }

    fn lone_picture(center: [f64; 2], half: [f64; 2]) -> Board {
        let picture = Some(PictureRef { file: "x.png".into(), size: [1, 1], crop: None, mirror: false });
        Board {
            items: vec![BoardItem::Picture { png: Arc::new(Vec::new()), px_size: [1, 1], picture, media: None, center, half, rotation: 0.0 }],
            skipped: 0,
        }
    }

    #[test]
    fn a_board_fits_and_centres_in_the_view() {
        let c = into_canvas(lone_picture([5000.0, -40.0], [1000.0, 500.0]), [800.0, 600.0], 1.0, "refs");
        assert_eq!(c.layers[0].name, "refs");
        let Shape::Raster { pos, size, kind, media, picture, .. } = &c.layers[0].shapes[0] else { panic!("expected a picture") };
        // Limited by the width: 90 % of 800 over 2000.
        assert!((size[0] - 720.0).abs() < 1e-3 && (size[1] - 360.0).abs() < 1e-3, "{size:?}");
        assert!((pos[0] - 40.0).abs() < 1e-3 && (pos[1] - 120.0).abs() < 1e-3, "{pos:?}");
        assert!(*kind == RasterKind::Image && media.is_none());
        assert_eq!(picture.as_ref().map(|p| p.file.as_str()), Some("x.png"));

        // A small board stays at natural size: one picture pixel per
        // physical pixel, so half as many points at 200 % display scale.
        let c = into_canvas(lone_picture([0.0, 0.0], [10.0, 10.0]), [800.0, 600.0], 2.0, "");
        let Shape::Raster { pos, size, .. } = &c.layers[0].shapes[0] else { panic!("expected a picture") };
        assert_eq!((*pos, *size), ([395.0, 295.0], [10.0, 10.0]));
    }

    /// Opening a board leaves the board file and the files next to it
    /// exactly as they were; the slide refers only to the app's own
    /// copies in the media folder.
    #[test]
    fn opening_never_touches_the_original_files() {
        let dir = TempDir::new("originals");
        let media = TempDir::new("originals-media");
        let board_path = dir.0.join("Board.PUR");
        std::fs::write(dir.0.join("pic.png"), two_tone_png(40, 20)).unwrap();
        std::fs::write(&board_path, v1_board(&[(1, Slot::Png(two_tone_png(40, 20)))], &[image(1, 40.0, 20.0, qt(-1.0, 1.0, 0.0, 0.0, 0.0), 1.0)])).unwrap();
        let snapshot = || {
            let mut files: Vec<_> = std::fs::read_dir(&dir.0)
                .unwrap()
                .flatten()
                .map(|e| {
                    let meta = e.metadata().unwrap();
                    (e.file_name(), std::fs::read(e.path()).unwrap(), meta.modified().unwrap(), meta.permissions().readonly())
                })
                .collect();
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        };
        let before = snapshot();
        assert!(is_pureref(&board_path));

        let board = open(&board_path, &media.0, &Progress::default()).unwrap();
        let canvas = into_canvas(board, [800.0, 600.0], 1.0, "Board");
        let mut used = HashSet::new();
        canvas.media_files(&mut used);
        let mut used: Vec<String> = used.into_iter().collect();
        used.sort();
        assert_eq!(used, media.files(), "the slide uses exactly the copies it stored, which cleanup tracks");
        let saved = crate::persistence::serialize_canvas(&canvas).unwrap();
        assert!(!saved.contains("Board.PUR") && !saved.contains("pic.png") && !saved.contains("refs/pic"), "no path back to the originals");
        drop(canvas);

        assert!(before == snapshot(), "the board folder is unchanged");
    }

    /// Opens real boards: `OSN_TEST_PUR` lists `.pur` files separated by
    /// `;`. Prints what each holds and checks the slide survives a save
    /// round trip. Pictures go to `OSN_DATA_DIR`'s media folder when that
    /// is set (and the slide is written there too, for a look in the
    /// app); otherwise to a scratch folder removed afterwards.
    #[test]
    #[ignore]
    fn opens_real_boards() {
        let list = std::env::var("OSN_TEST_PUR").expect("set OSN_TEST_PUR to one or more .pur files, separated by ;");
        let scratch = TempDir::new("real");
        let keep = std::env::var_os("OSN_DATA_DIR").is_some();
        let media_dir = if keep { crate::persistence::paths::media_dir().unwrap() } else { scratch.0.clone() };
        for path in list.split(';').filter(|p| !p.trim().is_empty()) {
            let path = Path::new(path.trim());
            let before = std::fs::metadata(path).unwrap();
            let t = std::time::Instant::now();
            let board = open(path, &media_dir, &Progress::default()).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            let (pictures, notes) = board.counts();
            let gifs = board.items.iter().filter(|i| matches!(i, BoardItem::Picture { media: Some(_), .. })).count();
            println!("{}: {pictures} pictures ({gifs} playing GIFs), {notes} notes, {} skipped, {:?}", path.display(), board.skipped, t.elapsed());
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let canvas = into_canvas(board, [1920.0, 1080.0], 1.0, &name);
            let mut used = HashSet::new();
            canvas.media_files(&mut used);
            let stored: u64 = used.iter().map(|f| std::fs::metadata(media_dir.join(f)).map_or(0, |m| m.len())).sum();
            let text = crate::persistence::serialize_canvas(&canvas).unwrap();
            let back = crate::persistence::parse_canvas(&text, 3).unwrap();
            assert_eq!(back.layers[0].shapes.len(), canvas.layers[0].shapes.len());
            println!("  slide file: {:.1} MB, media files: {} totalling {:.1} MB", text.len() as f64 / 1e6, used.len(), stored as f64 / 1e6);
            if keep {
                let idx = crate::persistence::max_canvas_index().map_or(0, |m| m + 1);
                crate::persistence::save_canvas(idx, &canvas).unwrap();
                println!("  written as slide {}", idx + 1);
            }
            let after = std::fs::metadata(path).unwrap();
            assert!(before.len() == after.len() && before.modified().unwrap() == after.modified().unwrap(), "the board file is unchanged");
        }
    }
}
