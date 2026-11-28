//! Picture vectors: what each picture on the slides shows, as numbers
//! from a vision model (`ml`), so find can list pictures of the same
//! subject — the same character, pose or kind of study — and not only
//! ones that look alike pixel for pixel (`tools::similar`).
//!
//! Off until turned on in the settings: the first time, it fetches the
//! runtime and the model (`ml::models`, about 215 MB). Then every picture
//! is read once, in the background: a stored picture from its file in the
//! media folder (the part its crop shows), a video or GIF from `FRAMES`
//! frames spread through it and its poster, their vectors averaged — so
//! it is found by what it mostly shows, not only by how it starts — and
//! any other from the bytes its slide keeps (a small picture whole). Its
//! vector is kept in the data folder (`persistence::vectors`), so a
//! picture is read once ever, not once a run.
//!
//! Typed words get a vector too, from the model's text half
//! (`ml::tokenizer`), fetched the first time words are looked for, so
//! find can list the pictures that show what was typed.
//!
//! Threads: a few read and prepare pictures (`ml::prep`), pausing while
//! the pen draws; one runs the model (`ml::runtime`), on the GPU when it
//! can, and lets go of it (and its memory) after a minute with nothing to
//! read. Turning it off stops them all; vectors already read stay.

use crate::canvas::board_link::Fnv;
use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{MediaKind, MediaRef, PictureRef, RasterKind, Shape};
use crate::ml::models::{self, Download};
use crate::ml::prep;
use crate::ml::runtime::{self, Device, Engine};
use crate::ml::tokenizer::Tokenizer;
use crate::persistence::vectors::VectorCache;
use image::RgbaImage;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// How often the slides are looked through for new pictures.
const LOOK_EVERY: Duration = Duration::from_millis(500);
/// Pictures run through the model at once.
const BATCH_GPU: usize = 16;
const BATCH_CPU: usize = 8;
/// The model is let go after this long with nothing to read.
const IDLE: Duration = Duration::from_secs(60);
/// Prepared pictures waiting for the model, at most (0.8 MB each).
const QUEUE: usize = 32;
/// The vectors file in the data folder.
const FILE: &str = "picture-vectors.bin";
/// Frames read from each video and GIF, spread through it, besides its
/// poster. Part of its key: another count reads them again.
const FRAMES: usize = 6;

/// Where a picture is read from.
enum Source {
    /// Its file in the media folder, the part shown.
    Stored(PictureRef),
    /// A video or GIF in the media folder, and its poster's bytes (read
    /// alone if the frames cannot be: no ffmpeg, say).
    Media { file: String, kind: MediaKind, poster: Arc<Vec<u8>> },
    /// The bytes its slide keeps.
    Bytes(Arc<Vec<u8>>),
}

struct Job {
    key: Arc<str>,
    source: Source,
}

struct Prepped {
    key: Arc<str>,
    /// One prepared picture, or a video's or GIF's frames, whose vectors
    /// are averaged.
    frames: Vec<Vec<f32>>,
}

/// What find asks for, read before anything waiting.
enum Ask {
    /// A picture's vector, to find others like it.
    Picture { id: u64, img: RgbaImage },
    /// Typed words' vector, to find pictures that show them.
    Words { id: u64, text: String },
}

/// Words are read as each of these, the vectors averaged: a word alone
/// matches pictures less well than a phrase about a picture.
const WORD_TEMPLATES: [&str; 3] = ["{}", "a drawing of {}", "an illustration of {}"];

enum Out {
    /// A picture's vector; `None` when it could not be read.
    Vector(Arc<str>, Option<Vec<f32>>),
    Answer(u64, Option<Vec<f32>>),
}

/// How far getting the model going is.
#[derive(Debug, Clone)]
enum Stage {
    Fetching(Download),
    Starting,
    Ready(Device),
    Failed(String),
}

/// How far getting the text half going is.
#[derive(Debug, Clone)]
enum Words {
    Idle,
    Fetching(Download),
    Ready,
    Failed(String),
}

/// The text half, in the model thread.
struct TextSide {
    tok: Tokenizer,
    model: PathBuf,
    engine: Option<Engine>,
}

enum Slot {
    Waiting,
    Got(Arc<[f32]>),
    Unreadable,
}

/// The key of a shape's bytes, by their address.
struct Seen {
    /// Keeps the address from going to other bytes while it is a key.
    png: Weak<Vec<u8>>,
    key: Arc<str>,
}

/// The threads, while it is on.
struct Pipe {
    jobs: Sender<Job>,
    asks: Sender<Ask>,
    out: Receiver<Out>,
    stage: Arc<Mutex<Stage>>,
    words: Arc<Mutex<Words>>,
    got: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Longest the threads are waited for when it stops.
const STOP_WAIT: Duration = Duration::from_secs(3);

impl Drop for Pipe {
    /// Stop the threads and wait for them, a while: ONNX Runtime unloaded
    /// at exit while a thread still holds a session can hang the exit.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + STOP_WAIT;
        while self.threads.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        for t in self.threads.drain(..) {
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

#[derive(Default)]
pub struct PictureVectors {
    known: HashMap<Arc<str>, Slot>,
    seen: HashMap<usize, Seen>,
    cache: Option<VectorCache>,
    cache_read: bool,
    pipe: Option<Pipe>,
    looked: Option<Instant>,
    /// Pictures on the slides last looked through, and how many are read.
    total: usize,
    read: usize,
    /// Goes up whenever a vector comes in, so results know to update.
    generation: u64,
    answers: HashMap<u64, Option<Arc<[f32]>>>,
    asked: HashSet<u64>,
    next_ask: u64,
}

/// Threads reading pictures: half the cores, at most four.
fn readers() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4))
}

/// What a picture is known by, here and in the vectors file: a stored
/// picture by its file and crop, a video or GIF by its file (and how many
/// frames are read of it), any other by its bytes.
fn key_of(png: &[u8], picture: Option<&PictureRef>, media: Option<&MediaRef>) -> Arc<str> {
    match (picture, media) {
        (Some(p), _) => format!("file:{}:{:?}", p.file, p.crop).into(),
        (None, Some(m)) => format!("media:{}:{FRAMES}", m.file).into(),
        (None, None) => format!("bytes:{:016x}:{}", Fnv::of(png), png.len()).into(),
    }
}

impl PictureVectors {
    /// Keep up with `slides` (the shown one first) while `on`: take in
    /// what the threads read and, every `LOOK_EVERY`, send new pictures
    /// off. Reading pauses while `drawing`. Off stops the threads.
    pub fn tick<'a>(&mut self, slides: impl IntoIterator<Item = &'a Canvas>, on: bool, drawing: bool) {
        if !on {
            if self.pipe.take().is_some() {
                // What was on its way will not come: ask again next time.
                self.known.retain(|_, s| !matches!(s, Slot::Waiting));
                self.asked.clear();
            }
            return;
        }
        if !self.cache_read {
            self.cache_read = true;
            self.read_cache();
        }
        let pipe = self.pipe.get_or_insert_with(start);
        pipe.paused.store(drawing, Ordering::Relaxed);
        let outs: Vec<Out> = pipe.out.try_iter().collect();
        let jobs = pipe.jobs.clone();
        for o in outs {
            match o {
                Out::Vector(key, Some(v)) => {
                    if let Some(c) = &mut self.cache {
                        if let Err(e) = c.add(&key, &v) {
                            log::warn!("could not keep a picture vector: {e}");
                            self.cache = None;
                        }
                    }
                    self.known.insert(key, Slot::Got(v.into()));
                    self.generation += 1;
                }
                Out::Vector(key, None) => {
                    self.known.insert(key, Slot::Unreadable);
                }
                Out::Answer(id, v) => {
                    if self.asked.remove(&id) {
                        self.answers.insert(id, v.map(Into::into));
                    }
                }
            }
        }
        if self.looked.is_some_and(|t| t.elapsed() < LOOK_EVERY) {
            return;
        }
        self.looked = Some(Instant::now());
        self.seen.retain(|_, s| s.png.strong_count() > 0);
        let mut here = HashSet::new();
        let (mut total, mut read) = (0, 0);
        for c in slides {
            for s in c.layers.iter().flat_map(|l| &l.shapes) {
                let Shape::Raster { png, kind: RasterKind::Image, picture, media, .. } = s else { continue };
                let addr = Arc::as_ptr(png) as usize;
                let key = match self.seen.get(&addr) {
                    Some(seen) => Arc::clone(&seen.key),
                    None => {
                        let key = key_of(png, picture.as_deref(), media.as_deref());
                        self.seen.insert(addr, Seen { png: Arc::downgrade(png), key: Arc::clone(&key) });
                        key
                    }
                };
                if !here.insert(Arc::clone(&key)) {
                    continue;
                }
                total += 1;
                match self.known.get(&key) {
                    Some(Slot::Got(_) | Slot::Unreadable) => read += 1,
                    Some(Slot::Waiting) => {}
                    None => {
                        let source = match (picture.as_deref(), media.as_deref()) {
                            (Some(p), _) => Source::Stored(p.clone()),
                            (None, Some(m)) => Source::Media { file: m.file.clone(), kind: m.kind, poster: Arc::clone(png) },
                            (None, None) => Source::Bytes(Arc::clone(png)),
                        };
                        if jobs.send(Job { key: Arc::clone(&key), source }).is_ok() {
                            self.known.insert(key, Slot::Waiting);
                        }
                    }
                }
            }
        }
        (self.total, self.read) = (total, read);
    }

    /// Read what earlier runs kept.
    fn read_cache(&mut self) {
        let path = match crate::persistence::paths::data_dir() {
            Ok(d) => d.join(FILE),
            Err(e) => {
                log::warn!("no data folder for picture vectors: {e}");
                return;
            }
        };
        let (cache, kept) = VectorCache::open(&path, models::MODEL_ID, prep::DIM);
        for (key, v) in kept {
            self.known.insert(key.into(), Slot::Got(v.into()));
        }
        self.cache = Some(cache);
        self.generation += 1;
    }

    /// Stop the threads, waiting for them a while: before the app exits.
    pub fn stop(&mut self) {
        self.pipe = None;
    }

    /// It is on.
    pub fn is_on(&self) -> bool {
        self.pipe.is_some()
    }

    /// The vector of the picture these bytes are, once read.
    pub fn vector(&self, png: &Arc<Vec<u8>>) -> Option<Arc<[f32]>> {
        let seen = self.seen.get(&(Arc::as_ptr(png) as usize))?;
        match self.known.get(&seen.key)? {
            Slot::Got(v) => Some(Arc::clone(v)),
            _ => None,
        }
    }

    /// What the picture these bytes are is known by (`key_of`), once seen.
    pub fn key(&self, png: &Arc<Vec<u8>>) -> Option<Arc<str>> {
        self.seen.get(&(Arc::as_ptr(png) as usize)).map(|s| Arc::clone(&s.key))
    }

    /// Ask for `img`'s vector, ahead of everything waiting; see `answer`.
    pub fn ask(&mut self, img: &RgbaImage) -> Option<u64> {
        let img = prep::shrink(img);
        self.send_ask(|id| Ask::Picture { id, img })
    }

    /// Ask for the vector of typed `words`; see `answer`. The first time,
    /// the text half is fetched (`words_status`).
    pub fn ask_words(&mut self, words: &str) -> Option<u64> {
        let text = words.trim().to_string();
        self.send_ask(|id| Ask::Words { id, text })
    }

    fn send_ask(&mut self, ask: impl FnOnce(u64) -> Ask) -> Option<u64> {
        let pipe = self.pipe.as_ref()?;
        self.next_ask += 1;
        let id = self.next_ask;
        pipe.asks.send(ask(id)).ok()?;
        self.asked.insert(id);
        Some(id)
    }

    /// While the text half is fetched, or when it could not start: where
    /// it is, in a few words, for find.
    pub fn words_status(&self) -> Option<String> {
        let p = self.pipe.as_ref()?;
        let words = lock(&p.words).clone();
        match words {
            Words::Fetching(d) => {
                let got = p.got.load(Ordering::Relaxed).min(d.size);
                Some(format!("Downloading {} ({} MB) — {}%", d.what, d.size / 1_000_000, got * 100 / d.size.max(1)))
            }
            Words::Failed(e) => Some(format!("Couldn't search pictures by words: {e}")),
            Words::Idle | Words::Ready => None,
        }
    }

    /// The vector asked for as `id`: `None` while it is being read,
    /// `Some(None)` when it could not be, or was not asked.
    pub fn answer(&self, id: u64) -> Option<Option<Arc<[f32]>>> {
        if self.asked.contains(&id) {
            return None;
        }
        Some(self.answers.get(&id).cloned().flatten())
    }

    /// `(read, total)` pictures on the slides last looked through.
    pub fn progress(&self) -> (usize, usize) {
        (self.read, self.total)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The model runs and every picture is read: results are final.
    pub fn settled(&self) -> bool {
        self.ready() && self.read >= self.total && self.asked.is_empty()
    }

    fn ready(&self) -> bool {
        self.pipe.as_ref().is_some_and(|p| matches!(*lock(&p.stage), Stage::Ready(_)))
    }

    /// There is work under way: the app should keep ticking.
    pub fn busy(&self) -> bool {
        let Some(p) = &self.pipe else { return false };
        if matches!(*lock(&p.words), Words::Fetching(_)) {
            return true;
        }
        match &*lock(&p.stage) {
            Stage::Failed(_) => false,
            Stage::Ready(_) => self.read < self.total || !self.asked.is_empty(),
            _ => true,
        }
    }

    /// Where it is, in a few words, for the settings.
    pub fn status(&self) -> String {
        let Some(p) = &self.pipe else { return "Off".into() };
        let stage = lock(&p.stage).clone();
        match stage {
            Stage::Fetching(d) => {
                let got = p.got.load(Ordering::Relaxed).min(d.size);
                format!("Downloading {} ({} MB) — {}%", d.what, d.size / 1_000_000, got * 100 / d.size.max(1))
            }
            Stage::Starting => "Starting…".into(),
            Stage::Ready(dev) if self.read < self.total => format!("Reading pictures, {} of {} · {}", self.read, self.total, dev.label()),
            Stage::Ready(dev) => format!("Ready · {} pictures · {}", self.total, dev.label()),
            Stage::Failed(e) => format!("Couldn't start: {e}"),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start the threads.
fn start() -> Pipe {
    let (jobs, jobs_rx) = mpsc::channel::<Job>();
    let (to_model, prepped) = mpsc::sync_channel::<Prepped>(QUEUE);
    let (asks, asks_rx) = mpsc::channel::<Ask>();
    let (out_tx, out) = mpsc::channel::<Out>();
    let stage = Arc::new(Mutex::new(Stage::Starting));
    let words = Arc::new(Mutex::new(Words::Idle));
    let got = Arc::new(AtomicU64::new(0));
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let jobs_rx = Arc::new(Mutex::new(jobs_rx));
    let media = crate::persistence::paths::media_dir().ok();
    let mut threads = Vec::new();
    for i in 0..readers() {
        let (rx, to_model, out, paused, stop, media) = (Arc::clone(&jobs_rx), to_model.clone(), out_tx.clone(), Arc::clone(&paused), Arc::clone(&stop), media.clone());
        match std::thread::Builder::new().name(format!("osn-vectors-read-{i}")).spawn(move || reader(&rx, &to_model, &out, &paused, &stop, media.as_deref())) {
            Ok(t) => threads.push(t),
            Err(e) => log::warn!("could not start a picture reading thread: {e}"),
        }
    }
    drop(to_model);
    let state = State { stage: Arc::clone(&stage), words: Arc::clone(&words), got: Arc::clone(&got), stop: Arc::clone(&stop) };
    match std::thread::Builder::new().name("osn-vectors-model".into()).spawn(move || model_thread(&prepped, &asks_rx, &out_tx, &state)) {
        Ok(t) => threads.push(t),
        Err(e) => *lock(&stage) = Stage::Failed(format!("could not start: {e}")),
    }
    Pipe { jobs, asks, out, stage, words, got, paused, stop, threads }
}

fn reader(jobs: &Mutex<Receiver<Job>>, to_model: &SyncSender<Prepped>, out: &Sender<Out>, paused: &AtomicBool, stop: &AtomicBool, media: Option<&Path>) {
    loop {
        let job = match lock(jobs).recv_timeout(Duration::from_millis(500)) {
            Ok(j) => j,
            Err(RecvTimeoutError::Timeout) if !stop.load(Ordering::Relaxed) => continue,
            Err(_) => return,
        };
        while paused.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let frames = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&job.source, media))).ok().unwrap_or_default();
        let sent = if frames.is_empty() {
            out.send(Out::Vector(job.key, None)).is_ok()
        } else {
            to_model.send(Prepped { key: job.key, frames: frames.iter().map(prep::prep).collect() }).is_ok()
        };
        if !sent {
            return;
        }
    }
}

/// The picture `source` holds — a stored one's crop shrunk for reading
/// (`prep::read_side`) — or a video's or GIF's poster and frames. Empty
/// when nothing could be read.
fn read(source: &Source, media: Option<&Path>) -> Vec<RgbaImage> {
    let picture = |b: &[u8]| image::load_from_memory(b).ok().map(|i| i.to_rgba8());
    match source {
        Source::Stored(p) => {
            let Some(path) = media.map(|m| m.join(&p.file)) else { return Vec::new() };
            let img = image::ImageReader::open(path).ok().and_then(|r| r.with_guessed_format().ok()).and_then(|r| r.decode().ok());
            img.map(|img| {
                let img = img.to_rgba8();
                let [_, _, w, h] = p.crop.unwrap_or([0, 0, img.width(), img.height()]);
                p.view(&img, prep::read_side(w, h))
            })
            .into_iter()
            .collect()
        }
        Source::Media { file, kind, poster } => {
            let mut frames: Vec<RgbaImage> = picture(poster).into_iter().collect();
            if let Some(path) = media.map(|m| m.join(file)) {
                match spread(&path, *kind) {
                    Ok(more) => frames.extend(more.iter().map(prep::shrink)),
                    Err(e) => log::info!("picture search reads only the poster of {file}: {e}"),
                }
            }
            frames
        }
        Source::Bytes(b) => picture(b).into_iter().collect(),
    }
}

/// `FRAMES` frames spread through the video or GIF at `path`.
fn spread(path: &Path, kind: MediaKind) -> anyhow::Result<Vec<RgbaImage>> {
    use crate::media::video_player;
    match kind {
        MediaKind::Gif => crate::media::gif_player::spread_frames(path, FRAMES),
        MediaKind::Video => {
            let tools = crate::media::ffmpeg::tools().ok_or_else(|| anyhow::anyhow!("no ffmpeg"))?;
            let info = video_player::probe(&tools, path)?;
            let total = info.frame_estimate().unwrap_or(1).max(1);
            // Short side at twice what the model reads, as for pictures.
            let [w, h] = info.size;
            let s = ((2 * prep::SIDE) as f64 / f64::from(w.min(h).max(1))).min(1.0);
            let size = [((f64::from(w) * s).round() as u32).max(2) & !1, ((f64::from(h) * s).round() as u32).max(2) & !1];
            (0..FRAMES as u64).map(|i| video_player::grab_frame(&tools, path, &info, ((2 * i + 1) * total) / (2 * FRAMES as u64), size)).collect()
        }
    }
}

/// What the model thread shares with the app.
struct State {
    stage: Arc<Mutex<Stage>>,
    words: Arc<Mutex<Words>>,
    got: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

fn model_thread(prepped: &Receiver<Prepped>, asks: &Receiver<Ask>, out: &Sender<Out>, st: &State) {
    let (got, stop) = (&*st.got, &*st.stop);
    let set = |s: Stage| *lock(&st.stage) = s;
    let (dir, model, device, first) = match setup(&set, got, stop) {
        Ok(x) => x,
        Err(e) => {
            if !stop.load(Ordering::Relaxed) {
                log::warn!("picture search could not start: {e}");
                set(Stage::Failed(e));
            }
            return;
        }
    };
    log::info!("picture search runs on the {}", device.label());
    set(Stage::Ready(device));
    let batch = if device == Device::Gpu { BATCH_GPU } else { BATCH_CPU };
    let mut engine = Some(first);
    let mut text: Option<TextSide> = None;
    let mut last_work = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(a) = asks.try_recv() {
            let (id, v) = match a {
                Ask::Picture { id, img } => (id, run(&mut engine, &model, device, &[prep::prep(&img)]).ok().and_then(|mut v| v.pop())),
                Ask::Words { id, text: words } => {
                    let v = words_vector(&mut text, &dir, device, &words, st);
                    if let Err(e) = &v {
                        if !stop.load(Ordering::Relaxed) {
                            log::warn!("picture search could not read words: {e}");
                            *lock(&st.words) = Words::Failed(e.clone());
                        }
                    }
                    (id, v.ok())
                }
            };
            last_work = Instant::now();
            if out.send(Out::Answer(id, v)).is_err() {
                return;
            }
            continue;
        }
        match prepped.recv_timeout(Duration::from_millis(50)) {
            Ok(first) => {
                // Up to `batch` frames, whole items only.
                let mut items = vec![first];
                let mut frames = items[0].frames.len();
                while frames < batch {
                    match prepped.try_recv() {
                        Ok(p) => {
                            frames += p.frames.len();
                            items.push(p);
                        }
                        Err(_) => break,
                    }
                }
                let counts: Vec<usize> = items.iter().map(|p| p.frames.len()).collect();
                let data: Vec<Vec<f32>> = items.iter_mut().flat_map(|p| std::mem::take(&mut p.frames)).collect();
                let vectors = run(&mut engine, &model, device, &data);
                let n = items.len();
                let sent = match vectors {
                    Ok(vs) => {
                        let mut vs = vs.into_iter();
                        items.into_iter().zip(counts).all(|(p, k)| out.send(Out::Vector(p.key, Some(mean(vs.by_ref().take(k))))).is_ok())
                    }
                    Err(e) => {
                        log::warn!("picture search could not read {n} pictures: {e}");
                        items.into_iter().all(|p| out.send(Out::Vector(p.key, None)).is_ok())
                    }
                };
                if !sent {
                    return;
                }
                last_work = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                let text_open = text.as_ref().is_some_and(|t| t.engine.is_some());
                if (engine.is_some() || text_open) && last_work.elapsed() > IDLE {
                    engine = None;
                    if let Some(t) = &mut text {
                        t.engine = None;
                    }
                    log::info!("picture search let go of the model after a minute idle");
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The average of vectors, length 1: a video's from its frames'.
fn mean(vs: impl Iterator<Item = Vec<f32>>) -> Vec<f32> {
    let mut sum = vec![0.0; prep::DIM];
    for v in vs {
        sum.iter_mut().zip(v).for_each(|(s, x)| *s += x);
    }
    prep::unit(&sum)
}

/// Run `pictures` through the model, opening it again if it was let go.
fn run(engine: &mut Option<Engine>, model: &Path, device: Device, pictures: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, String> {
    if engine.is_none() {
        *engine = Some(Engine::open(model, device, readers())?);
    }
    engine.as_mut().expect("opened above").embed(pictures)
}

/// The vector of typed `words`: each of `WORD_TEMPLATES`, averaged. The
/// first time, the text half is fetched and opened.
fn words_vector(text: &mut Option<TextSide>, dir: &Path, device: Device, words: &str, st: &State) -> Result<Vec<f32>, String> {
    if text.is_none() {
        let mut fetched = Vec::new();
        for d in [models::TOKENIZER, models::TEXT_MODEL] {
            if !dir.join(d.file).is_file() {
                *lock(&st.words) = Words::Fetching(d);
            }
            fetched.push(models::fetch(&d, dir, &st.got, &st.stop)?);
        }
        let tok = std::fs::read(&fetched[0]).map_err(|e| e.to_string()).and_then(|b| Tokenizer::parse(&b))?;
        *text = Some(TextSide { tok, model: fetched.pop().expect("two fetched"), engine: None });
        *lock(&st.words) = Words::Ready;
    }
    let t = text.as_mut().expect("set above");
    if t.engine.is_none() {
        t.engine = Some(match Engine::open(&t.model, device, readers()) {
            Ok(e) => e,
            Err(e) if device == Device::Gpu => {
                log::warn!("the GPU could not run the text model, using the CPU: {e}");
                Engine::open(&t.model, Device::Cpu, readers())?
            }
            Err(e) => return Err(e),
        });
    }
    let words = words.to_lowercase();
    let ids: Vec<Vec<i64>> = WORD_TEMPLATES.iter().map(|w| t.tok.model_ids(&w.replace("{}", &words))).collect();
    let vs = t.engine.as_mut().expect("opened above").embed_ids(&ids)?;
    let mut sum = vec![0.0; prep::DIM];
    for v in &vs {
        sum.iter_mut().zip(v).for_each(|(s, x)| *s += x);
    }
    Ok(prep::unit(&sum))
}

/// Fetch what is missing, load the runtime and open the model: on the GPU
/// when it can, else on the CPU. The same half-size model runs on both.
fn setup(set: &dyn Fn(Stage), got: &AtomicU64, stop: &AtomicBool) -> Result<(PathBuf, PathBuf, Device, Engine), String> {
    let dir = models::dir()?;
    if !models::runtime_present(&dir) {
        set(Stage::Fetching(models::RUNTIME));
        models::fetch_runtime(&dir, got, stop)?;
        set(Stage::Starting);
    }
    runtime::load(&dir)?;
    if !dir.join(models::VISION_MODEL.file).is_file() {
        set(Stage::Fetching(models::VISION_MODEL));
    }
    let model = models::fetch(&models::VISION_MODEL, &dir, got, stop)?;
    set(Stage::Starting);
    if runtime::gpu_available() {
        match Engine::open(&model, Device::Gpu, 1) {
            Ok(e) => return Ok((dir, model, Device::Gpu, e)),
            Err(e) => log::warn!("the GPU could not run the model, using the CPU: {e}"),
        }
    }
    let e = Engine::open(&model, Device::Cpu, readers())?;
    Ok((dir, model, Device::Cpu, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_picture_is_known_by_its_file_and_crop_others_by_their_bytes() {
        let p = PictureRef { file: "00ab.png".into(), size: [10, 10], crop: Some([1, 2, 3, 4]), mirror: false };
        assert_eq!(&*key_of(b"preview", Some(&p), None), "file:00ab.png:Some([1, 2, 3, 4])");
        let whole = PictureRef { crop: None, ..p.clone() };
        assert_ne!(key_of(b"x", Some(&whole), None), key_of(b"x", Some(&p), None), "another crop is another picture");
        assert_eq!(key_of(b"same", None, None), key_of(b"same", None, None));
        assert_ne!(key_of(b"same", None, None), key_of(b"other", None, None));
    }

    /// Runs the real model: `OSN_MODEL_DIR` must hold it (or fetch it) and
    /// `OSN_DATA_DIR` must be a scratch folder, as the vectors file goes there.
    #[test]
    #[ignore = "runs the model; needs OSN_MODEL_DIR and a scratch OSN_DATA_DIR"]
    fn a_picture_read_in_the_background_and_one_asked_for_agree() {
        assert!(std::env::var_os("OSN_DATA_DIR").is_some() && std::env::var_os("OSN_MODEL_DIR").is_some());
        let img = RgbaImage::from_fn(300, 200, |x, y| image::Rgba([(x % 256) as u8, (y * 3 % 256) as u8, 120, 255]));
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let png = Arc::new(bytes);
        let mut c = Canvas::default();
        c.layers[0].shapes.push(Shape::Raster {
            pos: [0.0, 0.0],
            size: [300.0, 200.0],
            png: Arc::clone(&png),
            px_size: [300, 200],
            color: [0; 4],
            rotation: 0.0,
            flip: false,
            kind: RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: None,
            grain: None,
        });
        let mut v = PictureVectors::default();
        let start = Instant::now();
        let mut asked = None;
        let (mut read, mut answer) = (None, None);
        while read.is_none() || answer.is_none() {
            assert!(start.elapsed() < Duration::from_secs(600), "never read: {}", v.status());
            std::thread::sleep(Duration::from_millis(50));
            v.looked = None;
            v.tick([&c], true, false);
            if asked.is_none() {
                asked = v.ask(&img);
            }
            read = v.vector(&png);
            answer = asked.and_then(|id| v.answer(id)).map(|a| a.expect("the asked picture was read"));
        }
        let (a, b) = (read.unwrap(), answer.unwrap());
        let like: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        assert!(like > 0.999, "{like}");
        assert!(v.status().starts_with("Ready"), "{}", v.status());
        v.tick([&c], false, false);
        assert!(!v.is_on());
    }

    /// Runs the text half on the vectors kept in a scratch `OSN_DATA_DIR`
    /// and prints what typed words find: real words should list pictures,
    /// nonsense few or none.
    #[test]
    #[ignore = "runs the model; needs OSN_MODEL_DIR and a scratch OSN_DATA_DIR with picture-vectors.bin"]
    fn words_find_pictures() {
        assert!(std::env::var_os("OSN_DATA_DIR").is_some() && std::env::var_os("OSN_MODEL_DIR").is_some());
        let c = Canvas::default();
        let mut v = PictureVectors::default();
        v.tick([&c], true, false);
        let kept: Vec<Arc<[f32]>> = v.known.values().filter_map(|s| if let Slot::Got(x) = s { Some(Arc::clone(x)) } else { None }).collect();
        assert!(!kept.is_empty(), "no vectors kept in OSN_DATA_DIR");
        let mut nonsense_most = 0;
        for (words, real) in [
            ("rough pencil sketch", true),
            ("legs", true),
            ("hands", true),
            ("seen from behind", true),
            ("red hair", true),
            ("girl with a sword", true),
            ("anatomy study", true),
            ("full body", true),
            ("lying down", true),
            ("black and white", true),
            ("feet", true),
            ("zzz", false),
            ("the", false),
            ("asdf", false),
            ("purple elephant spaceship", false),
            ("qwzx vbnm", false),
            ("spreadsheet invoice total", false),
        ] {
            let start = Instant::now();
            let id = v.ask_words(words).expect("asked");
            let vector = loop {
                assert!(start.elapsed() < Duration::from_secs(900), "{}", v.words_status().unwrap_or_default());
                std::thread::sleep(Duration::from_millis(50));
                v.tick([&c], true, false);
                if let Some(a) = v.answer(id) {
                    break a.expect("words read");
                }
            };
            let likes: Vec<f32> = kept.iter().map(|k| k.iter().zip(vector.iter()).map(|(a, b)| a * b).sum()).collect();
            let top = likes.iter().cloned().fold(f32::MIN, f32::max);
            let found = crate::tools::similar::by_words(&vector, kept.iter().enumerate().map(|(i, k)| (i, &k[..])));
            let odds: Vec<String> = found.iter().take(5).map(|f| format!("{:.3}", f.1)).collect();
            let mut sorted = likes.clone();
            sorted.sort_by(|a, b| b.total_cmp(a));
            let at = |f: f32| sorted[((sorted.len() - 1) as f32 * f) as usize];
            println!("{words:28} {:5.0} ms  top {top:.3} 10th {:.3} 1% {:.3} 10% {:.3} median {:.3}  listed {:2}  odds {}", start.elapsed().as_millis(), sorted[9.min(sorted.len() - 1)], at(0.01), at(0.1), at(0.5), found.len(), odds.join(" "));
            if real {
                assert!(!found.is_empty(), "{words}: nothing listed");
            } else {
                nonsense_most = nonsense_most.max(found.len());
            }
        }
        println!("nonsense listed at most {nonsense_most}");
    }

    /// Reads the videos and GIFs on a scratch slide by their frames, and
    /// says how far each one's vector is from its poster's alone.
    #[test]
    #[ignore = "runs the model and ffmpeg; needs OSN_MODEL_DIR and a scratch OSN_DATA_DIR with videos"]
    fn videos_are_read_by_their_frames() {
        use crate::persistence::{load_canvas, LoadOutcome, LoadProgress};
        assert!(std::env::var_os("OSN_DATA_DIR").is_some() && std::env::var_os("OSN_MODEL_DIR").is_some());
        let LoadOutcome::Loaded(c) = load_canvas(0, 0, &LoadProgress::default()) else { panic!("no slide 0") };
        let mut v = PictureVectors::default();
        let start = Instant::now();
        loop {
            v.tick([&c], true, false);
            let (read, total) = v.progress();
            if total > 0 && read >= total {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(600), "{}", v.status());
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("{} read in {:.1} s", v.progress().1, start.elapsed().as_secs_f32());
        let mut media = 0;
        for s in c.layers.iter().flat_map(|l| &l.shapes) {
            let Shape::Raster { png, media: Some(m), .. } = s else { continue };
            media += 1;
            let whole = v.vector(png).expect("read");
            let poster = image::load_from_memory(png).unwrap().to_rgba8();
            let id = v.ask(&poster).unwrap();
            let alone = loop {
                v.tick([&c], true, false);
                if let Some(a) = v.answer(id) {
                    break a.expect("poster read");
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let like: f32 = whole.iter().zip(alone.iter()).map(|(a, b)| a * b).sum();
            println!("{:?} {}: likeness of frames to poster {like:.3}", m.kind, m.file);
            assert!(like > 0.3 && like < 0.9999, "frames should add to the poster, not replace or equal it: {like}");
        }
        assert!(media > 0, "no videos or GIFs on slide 0");
    }

    #[test]
    fn off_it_does_nothing() {
        let mut v = PictureVectors::default();
        let c = Canvas::default();
        v.tick([&c], false, false);
        assert!(!v.is_on() && !v.busy());
        assert_eq!(v.status(), "Off");
        assert!(v.ask(&RgbaImage::new(4, 4)).is_none());
    }
}
