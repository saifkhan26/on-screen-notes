//! Picture vectors: what each picture on the slides shows, as numbers
//! from a vision model (`ml`), so find can list pictures of the same
//! subject — the same character, pose or kind of study — and not only
//! ones that look alike pixel for pixel (`tools::similar`).
//!
//! Off until turned on in the settings: the first time, it fetches the
//! runtime and the model (`ml::models`, about 215 MB). Then every picture
//! is read once, in the background: a stored picture from its file in the
//! media folder (the part its crop shows), any other from the bytes its
//! slide keeps (a video's or GIF's poster, a small picture whole). Its
//! vector is kept in the data folder (`persistence::vectors`), so a
//! picture is read once ever, not once a run.
//!
//! Threads: a few read and prepare pictures (`ml::prep`), pausing while
//! the pen draws; one runs the model (`ml::runtime`), on the GPU when it
//! can, and lets go of it (and its memory) after a minute with nothing to
//! read. Turning it off stops them all; vectors already read stay.

use crate::canvas::board_link::Fnv;
use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{PictureRef, RasterKind, Shape};
use crate::ml::models::{self, Download};
use crate::ml::prep;
use crate::ml::runtime::{self, Device, Engine};
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

/// Where a picture is read from.
enum Source {
    /// Its file in the media folder, the part shown.
    Stored(PictureRef),
    /// The bytes its slide keeps.
    Bytes(Arc<Vec<u8>>),
}

struct Job {
    key: Arc<str>,
    source: Source,
}

struct Prepped {
    key: Arc<str>,
    data: Vec<f32>,
}

/// A picture find looks for others like, read before anything waiting.
struct Ask {
    id: u64,
    img: RgbaImage,
}

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
    got: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
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
/// picture by its file and crop, any other by its bytes.
fn key_of(png: &[u8], picture: Option<&PictureRef>) -> Arc<str> {
    match picture {
        Some(p) => format!("file:{}:{:?}", p.file, p.crop).into(),
        None => format!("bytes:{:016x}:{}", Fnv::of(png), png.len()).into(),
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
                let Shape::Raster { png, kind: RasterKind::Image, picture, .. } = s else { continue };
                let addr = Arc::as_ptr(png) as usize;
                let key = match self.seen.get(&addr) {
                    Some(seen) => Arc::clone(&seen.key),
                    None => {
                        let key = key_of(png, picture.as_deref());
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
                        let source = match picture.as_deref() {
                            Some(p) => Source::Stored(p.clone()),
                            None => Source::Bytes(Arc::clone(png)),
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

    /// Ask for `img`'s vector, ahead of everything waiting; see `answer`.
    pub fn ask(&mut self, img: &RgbaImage) -> Option<u64> {
        let pipe = self.pipe.as_ref()?;
        let img = prep::shrink(img);
        self.next_ask += 1;
        let id = self.next_ask;
        pipe.asks.send(Ask { id, img }).ok()?;
        self.asked.insert(id);
        Some(id)
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
    let got = Arc::new(AtomicU64::new(0));
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let jobs_rx = Arc::new(Mutex::new(jobs_rx));
    let media = crate::persistence::paths::media_dir().ok();
    for i in 0..readers() {
        let (rx, to_model, out, paused, stop, media) = (Arc::clone(&jobs_rx), to_model.clone(), out_tx.clone(), Arc::clone(&paused), Arc::clone(&stop), media.clone());
        if let Err(e) = std::thread::Builder::new().name(format!("osn-vectors-read-{i}")).spawn(move || reader(&rx, &to_model, &out, &paused, &stop, media.as_deref())) {
            log::warn!("could not start a picture reading thread: {e}");
        }
    }
    drop(to_model);
    let (st, g, sp) = (Arc::clone(&stage), Arc::clone(&got), Arc::clone(&stop));
    if let Err(e) = std::thread::Builder::new().name("osn-vectors-model".into()).spawn(move || model_thread(&prepped, &asks_rx, &out_tx, &st, &g, &sp)) {
        *lock(&stage) = Stage::Failed(format!("could not start: {e}"));
    }
    Pipe { jobs, asks, out, stage, got, paused, stop }
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
        let img = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&job.source, media))).ok().flatten();
        let sent = match img {
            Some(img) => to_model.send(Prepped { key: job.key, data: prep::prep(&img) }).is_ok(),
            None => out.send(Out::Vector(job.key, None)).is_ok(),
        };
        if !sent {
            return;
        }
    }
}

/// The picture `source` holds; a stored one's crop shrunk for reading
/// (`prep::read_side`).
fn read(source: &Source, media: Option<&Path>) -> Option<RgbaImage> {
    match source {
        Source::Stored(p) => {
            let path = media?.join(&p.file);
            let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgba8();
            let [_, _, w, h] = p.crop.unwrap_or([0, 0, img.width(), img.height()]);
            Some(p.view(&img, prep::read_side(w, h)))
        }
        Source::Bytes(b) => Some(image::load_from_memory(b).ok()?.to_rgba8()),
    }
}

fn model_thread(prepped: &Receiver<Prepped>, asks: &Receiver<Ask>, out: &Sender<Out>, stage: &Mutex<Stage>, got: &AtomicU64, stop: &AtomicBool) {
    let set = |s: Stage| *lock(stage) = s;
    let (model, device, first) = match setup(&set, got, stop) {
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
    let mut last_work = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(a) = asks.try_recv() {
            let v = run(&mut engine, &model, device, &[prep::prep(&a.img)]).ok().and_then(|mut v| v.pop());
            last_work = Instant::now();
            if out.send(Out::Answer(a.id, v)).is_err() {
                return;
            }
            continue;
        }
        match prepped.recv_timeout(Duration::from_millis(50)) {
            Ok(first) => {
                let mut items = vec![first];
                items.extend(prepped.try_iter().take(batch - 1));
                let data: Vec<Vec<f32>> = items.iter_mut().map(|p| std::mem::take(&mut p.data)).collect();
                let vectors = run(&mut engine, &model, device, &data);
                let n = items.len();
                let sent = match vectors {
                    Ok(vs) => items.into_iter().zip(vs).all(|(p, v)| out.send(Out::Vector(p.key, Some(v))).is_ok()),
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
                if engine.is_some() && last_work.elapsed() > IDLE {
                    engine = None;
                    log::info!("picture search let go of the model after a minute idle");
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Run `pictures` through the model, opening it again if it was let go.
fn run(engine: &mut Option<Engine>, model: &Path, device: Device, pictures: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, String> {
    if engine.is_none() {
        *engine = Some(Engine::open(model, device, readers())?);
    }
    engine.as_mut().expect("opened above").embed(pictures)
}

/// Fetch what is missing, load the runtime and open the model: on the GPU
/// when it can, else on the CPU.
fn setup(set: &dyn Fn(Stage), got: &AtomicU64, stop: &AtomicBool) -> Result<(PathBuf, Device, Engine), String> {
    let dir = models::dir()?;
    let fetch = |d: &Download| {
        if !dir.join(d.file).is_file() {
            set(Stage::Fetching(*d));
        }
        let path = models::fetch(d, &dir, got, stop);
        set(Stage::Starting);
        path
    };
    if !models::runtime_present(&dir) {
        set(Stage::Fetching(models::RUNTIME));
        models::fetch_runtime(&dir, got, stop)?;
        set(Stage::Starting);
    }
    runtime::load(&dir)?;
    if runtime::gpu_available() {
        let model = fetch(&models::GPU_MODEL)?;
        match Engine::open(&model, Device::Gpu, 1) {
            Ok(e) => return Ok((model, Device::Gpu, e)),
            Err(e) => log::warn!("the GPU could not run the model, using the CPU: {e}"),
        }
    }
    let model = fetch(&models::CPU_MODEL)?;
    let e = Engine::open(&model, Device::Cpu, readers())?;
    Ok((model, Device::Cpu, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_picture_is_known_by_its_file_and_crop_others_by_their_bytes() {
        let p = PictureRef { file: "00ab.png".into(), size: [10, 10], crop: Some([1, 2, 3, 4]), mirror: false };
        assert_eq!(&*key_of(b"preview", Some(&p)), "file:00ab.png:Some([1, 2, 3, 4])");
        let whole = PictureRef { crop: None, ..p.clone() };
        assert_ne!(key_of(b"x", Some(&whole)), key_of(b"x", Some(&p)), "another crop is another picture");
        assert_eq!(key_of(b"same", None), key_of(b"same", None));
        assert_ne!(key_of(b"same", None), key_of(b"other", None));
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
