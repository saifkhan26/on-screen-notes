//! Video playback through an `ffmpeg` child process.
//!
//! `ffmpeg` decodes, converts and scales; it writes raw RGBA frames to
//! its stdout, and a reader thread turns each into an egui image. The
//! channel between them holds only a few frames, so when the player
//! stops reading — paused — the pipe fills and ffmpeg simply blocks.
//!
//! Frames are numbered: frame `i` is the one shown at `i / fps`. The
//! decoder is started at a frame index (it seeks a quarter of a frame
//! early so rounding cannot skip the frame asked for), and the `k`-th
//! frame it produces is frame `start + k`. Decoded frames go into a
//! cache around the current position, which is what makes stepping
//! backwards cheap: a step lands in the cache, or the decoder is
//! restarted a little before the wanted frame and refills it.

use super::ffmpeg::Tools;
use super::{Cmd, PlayerStatus};
use crate::error::{anyhow, Context, Result};
use egui::{Color32, ColorImage};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Largest decode size, long side. 4K sources are decoded at this so
/// the pipe, the cache and the GPU upload stay affordable.
const MAX_DECODE_SIDE: u32 = 1920;
/// Memory for cached frames, per video.
const CACHE_BUDGET: usize = 192 * 1024 * 1024;
/// Frames restarting the decoder rewinds past the wanted frame when
/// stepping backwards, so the next few Previous presses are cache hits.
const BACK_WINDOW: u64 = 15;
/// A paused video keeps its decoder this long (for quick stepping),
/// then lets it go. The frame on screen stays.
const IDLE_DECODER: Duration = Duration::from_secs(5);
/// Frames are decoded this much larger than they are shown, so zooming
/// in a little stays sharp and the GPU only ever scales them down.
const HEADROOM: f32 = 1.5;
/// How long a zoom or resize must hold still before the video is
/// decoded again at the new size, so a zoom gesture does not restart
/// ffmpeg on every frame.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);

/// What `ffprobe` says about the video stream.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    /// Index of the stream to decode (cover-art streams are skipped).
    pub stream: usize,
    /// Display size: rotation applied and non-square pixels squared.
    pub size: [u32; 2],
    pub fps: [u32; 2],
    /// Frame count, when the file records it.
    pub frames: Option<u64>,
    pub duration: Option<f64>,
}

impl VideoInfo {
    pub fn fps(&self) -> f64 {
        f64::from(self.fps[0]) / f64::from(self.fps[1].max(1))
    }

    /// Frame count from the header, else estimated from the length.
    pub fn frame_estimate(&self) -> Option<u64> {
        self.frames.or_else(|| self.duration.map(|d| (d * self.fps()).round().max(1.0) as u64))
    }
}

/// Run `ffprobe` on `path`.
pub fn probe(tools: &Tools, path: &Path) -> Result<VideoInfo> {
    let out = tools
        .ffprobe_cmd()
        .args([
            "-v", "error",
            "-select_streams", "v",
            "-show_entries",
            "stream=index,width,height,avg_frame_rate,r_frame_rate,nb_frames,duration,sample_aspect_ratio\
             :stream_disposition=attached_pic:stream_tags=rotate:stream_side_data=rotation:format=duration",
            "-of", "default",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("running ffprobe")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffprobe: {}", err.lines().next().unwrap_or("failed")));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

/// `"30000/1001"` → `[30000, 1001]`; `None` for `0/0` and junk.
fn parse_ratio(s: &str, sep: char) -> Option<[u32; 2]> {
    let (n, d) = s.split_once(sep)?;
    let (n, d) = (n.trim().parse().ok()?, d.trim().parse().ok()?);
    (n > 0 && d > 0).then_some([n, d])
}

/// Parse `ffprobe -of default` output: `[STREAM]…[/STREAM]` blocks of
/// `key=value` lines (side data nested inside), then `[FORMAT]`.
fn parse_probe(text: &str) -> Result<VideoInfo> {
    use std::collections::HashMap;
    let mut streams: Vec<HashMap<String, String>> = Vec::new();
    let mut format: HashMap<String, String> = HashMap::new();
    let mut section = "";
    for line in text.lines().map(str::trim) {
        match line {
            "[STREAM]" => { section = "stream"; streams.push(HashMap::new()); }
            "[FORMAT]" => section = "format",
            "[/STREAM]" | "[/FORMAT]" => section = "",
            _ => {
                let Some((k, v)) = line.split_once('=') else { continue };
                let map = match section {
                    "stream" => streams.last_mut(),
                    "format" => Some(&mut format),
                    _ => None,
                };
                if let Some(map) = map {
                    // Side data's `rotation` wins over the older tag.
                    map.entry(k.to_string()).or_insert_with(|| v.to_string());
                }
            }
        }
    }
    let s = streams
        .iter()
        .find(|s| s.get("DISPOSITION:attached_pic").map(String::as_str) != Some("1")
            && s.get("width").and_then(|w| w.parse::<u32>().ok()).unwrap_or(0) > 0)
        .ok_or_else(|| anyhow!("no video stream"))?;
    let get = |k: &str| s.get(k).map(String::as_str).unwrap_or("");
    let stream = get("index").parse().unwrap_or(0);
    let (mut w, mut h): (u32, u32) = (get("width").parse()?, get("height").parse()?);
    if let Some([n, d]) = parse_ratio(get("sample_aspect_ratio"), ':') {
        if n != d {
            w = ((f64::from(w) * f64::from(n) / f64::from(d)).round() as u32).max(1);
        }
    }
    let rotation: i64 = s
        .get("rotation")
        .or_else(|| s.get("TAG:rotate"))
        .and_then(|r| r.parse::<f64>().ok())
        .map(|r| r.round() as i64)
        .unwrap_or(0);
    if rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    let sane = |r: Option<[u32; 2]>| r.filter(|[n, d]| (1.0..=120.0).contains(&(f64::from(*n) / f64::from(*d))));
    let fps = sane(parse_ratio(get("avg_frame_rate"), '/'))
        .or_else(|| sane(parse_ratio(get("r_frame_rate"), '/')))
        .unwrap_or([30, 1]);
    let frames = get("nb_frames").parse().ok().filter(|n| *n > 0);
    let duration = get("duration")
        .parse::<f64>()
        .ok()
        .or_else(|| format.get("duration").and_then(|d| d.parse().ok()))
        .filter(|d| *d > 0.0);
    Ok(VideoInfo { stream, size: [w, h], fps, frames, duration })
}

/// Decode size for a video of display size `native`: never larger than
/// the source or `MAX_DECODE_SIDE`, and `HEADROOM` times the size it is
/// shown at (`on_screen`, physical pixels) — but at least 480 px on its
/// long side.
pub fn decode_size(native: [u32; 2], on_screen: Option<[f32; 2]>) -> [u32; 2] {
    let long = native[0].max(native[1]).max(1) as f32;
    let mut scale = (MAX_DECODE_SIDE as f32 / long).min(1.0);
    if let Some([sw, sh]) = on_screen {
        let fit = (sw / native[0].max(1) as f32).max(sh / native[1].max(1) as f32) * HEADROOM;
        scale = scale.min(fit.max(480.0 / long));
    }
    [
        ((native[0] as f32 * scale).round() as u32).max(2),
        ((native[1] as f32 * scale).round() as u32).max(2),
    ]
}

/// Whether frames decoded at `current` still suit a view that asks for
/// `wanted` (both from `decode_size`). Up to 1.4× larger is fine: the
/// headroom means they are still at least the size shown. More than 2×
/// smaller wastes decoding and makes the GPU skip pixels.
fn size_fits(current: [u32; 2], wanted: [u32; 2]) -> bool {
    let r = wanted[0] as f32 / current[0].max(1) as f32;
    (0.5..=1.4).contains(&r)
}

/// Arguments that make ffmpeg write frames `start, start+1, …` of
/// `info`'s stream as raw RGBA at `size`.
fn decode_args(tools: &Tools, path: &Path, info: &VideoInfo, start: u64, size: [u32; 2]) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = Vec::new();
    let mut push = |s: &str| a.push(s.into());
    push("-nostdin");
    push("-hide_banner");
    push("-loglevel");
    push("error");
    if start > 0 {
        // A quarter frame early: the decoder trims everything before
        // the seek time, so seeking to exactly `start / fps` could round
        // past frame `start`, and half a frame would sit on the `fps`
        // filter's rounding tie.
        let t = (start as f64 - 0.25) / info.fps();
        push("-ss");
        push(&format!("{t:.6}"));
    }
    a.push("-i".into());
    a.push(path.as_os_str().to_owned());
    let mut push = |s: &str| a.push(s.into());
    push("-map");
    push(&format!("0:{}", info.stream));
    push("-an");
    push("-sn");
    push("-dn");
    push("-vf");
    push(&format!(
        "fps={}/{},scale={}:{}:flags=bicubic,format=rgba",
        info.fps[0], info.fps[1], size[0], size[1]
    ));
    // The `fps` filter already made the stream constant-rate; stop the
    // muxer from duplicating or dropping frames again.
    if tools.has_fps_mode() {
        push("-fps_mode");
    } else {
        push("-vsync");
    }
    push("passthrough");
    push("-f");
    push("rawvideo");
    push("pipe:1");
    a
}

/// One frame of the poster for an import: frame 0 at `size`.
pub fn grab_first_frame(tools: &Tools, path: &Path, info: &VideoInfo, size: [u32; 2]) -> Result<image::RgbaImage> {
    let mut args = decode_args(tools, path, info, 0, size);
    // `-frames:v 1` must come before the output name.
    let out_name = args.pop().unwrap_or_default();
    args.extend(["-frames:v".into(), "1".into(), out_name]);
    let out = tools
        .ffmpeg_cmd()
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("running ffmpeg")?;
    let want = size[0] as usize * size[1] as usize * 4;
    if out.stdout.len() < want {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffmpeg could not decode a frame: {}", err.lines().next().unwrap_or("no output")));
    }
    let mut bytes = out.stdout;
    bytes.truncate(want);
    image::RgbaImage::from_raw(size[0], size[1], bytes).context("frame size mismatch")
}

enum Msg {
    /// `(generation, frame index, image)`.
    Frame(u64, u64, Arc<ColorImage>),
    /// The decoder of this generation ran out; the value is one past
    /// the last frame index it produced.
    End(u64, u64),
}

/// A running ffmpeg plus the thread reading its frames.
struct Decoder {
    child: Child,
    rx: Receiver<Msg>,
    generation: u64,
    start: u64,
    /// Index of the next frame expected from it.
    next: u64,
    ended: bool,
    size: [u32; 2],
    last_activity: Instant,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // The reader thread then sees EOF (or a closed channel) and ends.
        let _ = self.child.kill();
    }
}

fn spawn_decoder(tools: &Tools, path: &Path, info: &VideoInfo, start: u64, size: [u32; 2], generation: u64) -> Result<Decoder> {
    let mut child = tools
        .ffmpeg_cmd()
        .args(decode_args(tools, path, info, start, size))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Not piped: an unread stderr pipe fills up and ffmpeg stalls.
        .stderr(Stdio::null())
        .spawn()
        .context("starting ffmpeg")?;
    super::ffmpeg::kill_with_app(&child);
    let stdout = child.stdout.take().context("ffmpeg stdout")?;
    let (tx, rx) = mpsc::sync_channel(4);
    std::thread::Builder::new()
        .name("osn-video-read".into())
        .spawn(move || read_frames(stdout, tx, generation, start, size))
        .context("starting video reader")?;
    Ok(Decoder { child, rx, generation, start, next: start, ended: false, size, last_activity: Instant::now() })
}

fn read_frames(mut out: impl Read, tx: SyncSender<Msg>, generation: u64, start: u64, size: [u32; 2]) {
    let len = size[0] as usize * size[1] as usize * 4;
    let mut index = start;
    loop {
        let mut buf = vec![0u8; len];
        if out.read_exact(&mut buf).is_err() {
            let _ = tx.send(Msg::End(generation, index));
            return;
        }
        // Video is opaque, so straight and premultiplied alpha are the
        // same bytes and the buffer can become `Color32`s as it is.
        let pixels: Vec<Color32> = bytemuck::cast_vec(buf);
        let image = Arc::new(ColorImage { size: [size[0] as usize, size[1] as usize], pixels });
        if tx.send(Msg::Frame(generation, index, image)).is_err() {
            return;
        }
        index += 1;
    }
}

enum Probe {
    Pending(Receiver<Result<VideoInfo>>),
    Ready(VideoInfo),
    Failed,
}

/// Playback clock: frame `start` was due at `t0`.
#[derive(Clone, Copy)]
struct Clock {
    t0: Instant,
    start: u64,
}

/// One video object's playback state.
pub struct VideoPlayer {
    path: PathBuf,
    tools: Option<Tools>,
    probe: Probe,
    decoder: Option<Decoder>,
    generation: u64,
    cache: BTreeMap<u64, Arc<ColorImage>>,
    shown: Option<(u64, Arc<ColorImage>)>,
    /// Frame wanted on screen while paused.
    want: u64,
    clock: Option<Clock>,
    looping: bool,
    /// Frame count, learned when a decoder reaches the end.
    end: Option<u64>,
    last_spawn: Option<Instant>,
    /// Shown size in physical pixels, for picking the decode size.
    on_screen: Option<[f32; 2]>,
    error: Option<String>,
    /// Position (seconds) to open at, applied once the probe is in.
    pending_resume: Option<f64>,
    /// When the view first stopped suiting the decoded size; see
    /// `follow_view`.
    resize_since: Option<Instant>,
}

impl VideoPlayer {
    /// Start probing `path`. Videos open paused; `resume` is a position
    /// in seconds to open at.
    pub fn new(path: PathBuf, tools: Option<Tools>, resume: Option<f64>) -> Self {
        let mut error = None;
        let probe = match (&tools, path.is_file()) {
            (_, false) => {
                error = Some("Video file is missing".to_string());
                Probe::Failed
            }
            (None, true) => Probe::Failed,
            (Some(t), true) => {
                let (tx, rx) = mpsc::channel();
                let (t, p) = (t.clone(), path.clone());
                let _ = std::thread::Builder::new()
                    .name("osn-video-probe".into())
                    .spawn(move || { let _ = tx.send(probe(&t, &p)); });
                Probe::Pending(rx)
            }
        };
        Self {
            path,
            tools,
            probe,
            decoder: None,
            generation: 0,
            cache: BTreeMap::new(),
            shown: None,
            want: 0,
            clock: None,
            looping: false,
            end: None,
            last_spawn: None,
            on_screen: None,
            error,
            pending_resume: resume,
            resize_since: None,
        }
    }

    pub fn set_on_screen(&mut self, px: [f32; 2]) {
        self.on_screen = Some(px);
    }

    fn info(&self) -> Option<&VideoInfo> {
        match &self.probe {
            Probe::Ready(i) => Some(i),
            _ => None,
        }
    }

    /// Frames in the video: the real count once the end was reached,
    /// else the header's count or an estimate.
    fn frame_count(&self) -> Option<u64> {
        self.end.or_else(|| self.info().and_then(VideoInfo::frame_estimate))
    }

    fn last_frame(&self) -> u64 {
        self.frame_count().map(|n| n.saturating_sub(1)).unwrap_or(u64::MAX)
    }

    fn poll_probe(&mut self) {
        let Probe::Pending(rx) = &self.probe else { return };
        match rx.try_recv() {
            Ok(Ok(info)) => {
                let fps = info.fps();
                self.probe = Probe::Ready(info);
                if let Some(t) = self.pending_resume.take() {
                    self.want = (t * fps).round() as u64;
                }
            }
            Ok(Err(e)) => {
                log::warn!("probing {} failed: {e:#}", self.path.display());
                self.error = Some(format!("Can't read this video ({e})"));
                self.probe = Probe::Failed;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.error = Some("Can't read this video".into());
                self.probe = Probe::Failed;
            }
        }
    }

    /// The frame index that should be on screen at `now`.
    fn target(&self, now: Instant, fps: f64) -> u64 {
        match self.clock {
            Some(c) => c.start + (now.saturating_duration_since(c.t0).as_secs_f64() * fps) as u64,
            None => self.want,
        }
    }

    /// Advance playback to `now`: read decoded frames, pick the frame
    /// to show, and (re)start the decoder when the wanted frame is not
    /// on its way.
    pub fn tick(&mut self, now: Instant) {
        self.poll_probe();
        let Some(fps) = self.info().map(VideoInfo::fps) else { return };
        let mut target = self.target(now, fps);

        // Past the end: loop, or stop on the last frame.
        if let Some(end) = self.end {
            if target >= end {
                if self.clock.is_some() && self.looping {
                    self.clock = Some(Clock { t0: now, start: 0 });
                    target = 0;
                } else {
                    self.clock = None;
                    target = end.saturating_sub(1);
                }
            }
        }
        if self.clock.is_none() {
            self.want = target;
        }

        let playing = self.clock.is_some();
        if let Some(native) = self.info().map(|i| i.size) {
            self.follow_view(now, native);
        }
        self.pump(target + if playing { 8 } else { 2 }, target);

        if let Some(img) = self.cache.get(&target) {
            // Compared by image too: after a resize the same frame comes
            // back sharper and must replace the old one.
            if self.shown.as_ref().map_or(true, |(i, f)| *i != target || !Arc::ptr_eq(f, img)) {
                self.shown = Some((target, Arc::clone(img)));
            }
        } else if playing {
            // Running late: show the newest frame that is not ahead.
            if let Some((&i, img)) = self.cache.range(..=target).next_back() {
                if self.shown.as_ref().map_or(true, |(s, _)| *s < i || *s > target) {
                    self.shown = Some((i, Arc::clone(img)));
                }
            }
        }

        if !self.cache.contains_key(&target) {
            self.ensure_decoder(now, target, playing);
        } else if !playing {
            // Paused with the frame on screen: let an idle decoder go.
            if self.decoder.as_ref().is_some_and(|d| now.duration_since(d.last_activity) > IDLE_DECODER) {
                self.decoder = None;
            }
        }
    }

    /// Zooming or resizing changes the size the video is shown at, but
    /// the decoder keeps making frames at the size it was started with —
    /// stretched, they look blurry. Once the view has settled at a size
    /// the decoded frames no longer suit, drop them and the decoder;
    /// `ensure_decoder` then decodes from the current frame at the new
    /// size, playing or paused. The old frame stays up until then.
    fn follow_view(&mut self, now: Instant, native: [u32; 2]) {
        let current = self
            .decoder
            .as_ref()
            .map(|d| d.size)
            .or_else(|| self.shown.as_ref().map(|(_, f)| [f.size[0] as u32, f.size[1] as u32]));
        let Some(current) = current else {
            self.resize_since = None;
            return;
        };
        if size_fits(current, decode_size(native, self.on_screen)) {
            self.resize_since = None;
            return;
        }
        let since = *self.resize_since.get_or_insert(now);
        if now.duration_since(since) >= RESIZE_SETTLE {
            self.resize_since = None;
            self.cache.clear();
            self.decoder = None;
            self.last_spawn = None;
        }
    }

    /// Move frames from the decoder into the cache, up to `horizon`.
    /// Stopping there is what pauses ffmpeg: the channel fills and its
    /// next write blocks.
    fn pump(&mut self, horizon: u64, target: u64) {
        let Some(d) = self.decoder.as_mut() else { return };
        while !d.ended && d.next <= horizon {
            match d.rx.try_recv() {
                Ok(Msg::Frame(g, i, img)) if g == d.generation => {
                    self.cache.insert(i, img);
                    d.next = i + 1;
                    d.last_activity = Instant::now();
                }
                Ok(Msg::End(g, n)) if g == d.generation => {
                    d.ended = true;
                    if n > d.start || d.start == 0 {
                        self.end = Some(n.max(1));
                        if n == 0 {
                            self.error = Some("FFmpeg could not decode this video".into());
                        }
                    } else {
                        // Started past the real end: it lies at or
                        // before `start`.
                        self.end = Some(self.end.map_or(n, |e| e.min(n)).max(1));
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    d.ended = true;
                    break;
                }
            }
        }
        let frame_bytes = self.cache.values().next().map_or(1, |f| f.pixels.len() * 4).max(1);
        let cap = (CACHE_BUDGET / frame_bytes).clamp(8, 600);
        while self.cache.len() > cap {
            let (first, last) = match (self.cache.keys().next(), self.cache.keys().next_back()) {
                (Some(&f), Some(&l)) => (f, l),
                _ => break,
            };
            let drop = if target.abs_diff(first) >= target.abs_diff(last) { first } else { last };
            self.cache.remove(&drop);
        }
    }

    /// Keep or (re)start the decoder so that `target` gets decoded.
    fn ensure_decoder(&mut self, now: Instant, target: u64, playing: bool) {
        let (Some(tools), Some(info)) = (self.tools.clone(), self.info().cloned()) else { return };
        // Frames the decoder may still have to get through first.
        let slack = if playing { 8 } else { 45 };
        if let Some(d) = &self.decoder {
            let coming = !d.ended && d.start <= target && d.next <= target && target - d.next <= slack;
            if coming { return; }
            // Playback falling behind a slow decoder: restarting costs a
            // seek each time, so do it at most once a second.
            let min_gap = if playing && !d.ended && d.next <= target { 1000 } else { 100 };
            if self.last_spawn.is_some_and(|t| now.duration_since(t) < Duration::from_millis(min_gap)) {
                return;
            }
        } else if self.last_spawn.is_some_and(|t| now.duration_since(t) < Duration::from_millis(100)) {
            return;
        }
        // Going backwards: start a bit earlier so the next Previous
        // presses find their frames cached.
        let going_back = !playing && self.shown.as_ref().is_some_and(|(s, _)| target < *s);
        let start = if going_back { target.saturating_sub(BACK_WINDOW) } else { target };

        let want_size = decode_size(info.size, self.on_screen);
        let size = match self.decoder.as_ref().map(|d| d.size).or_else(|| self.cache.values().next().map(|f| [f.size[0] as u32, f.size[1] as u32])) {
            // Keep the current size while it suits the view, so the
            // cache stays usable.
            Some(cur) if size_fits(cur, want_size) => cur,
            Some(_) => {
                self.cache.clear();
                want_size
            }
            None => want_size,
        };
        self.generation += 1;
        self.last_spawn = Some(now);
        self.decoder = None;
        match spawn_decoder(&tools, &self.path, &info, start, size, self.generation) {
            Ok(d) => self.decoder = Some(d),
            Err(e) => {
                log::warn!("starting ffmpeg for {} failed: {e:#}", self.path.display());
                self.error = Some(format!("Can't start FFmpeg ({e})"));
            }
        }
    }

    pub fn frame(&self) -> Option<&Arc<ColorImage>> {
        self.shown.as_ref().map(|(_, f)| f)
    }

    fn current(&self) -> u64 {
        self.shown.as_ref().map(|(i, _)| *i).unwrap_or(self.want)
    }

    pub fn position(&self) -> f64 {
        self.info().map_or(0.0, |i| self.current() as f64 / i.fps())
    }

    pub fn command(&mut self, cmd: Cmd, now: Instant) {
        let Some(fps) = self.info().map(VideoInfo::fps) else { return };
        match cmd {
            Cmd::TogglePlay => {
                if self.clock.is_some() {
                    self.want = self.current();
                    self.clock = None;
                } else {
                    let mut from = self.want;
                    if self.end.is_some_and(|e| from + 1 >= e) { from = 0; }
                    self.want = from;
                    self.clock = Some(Clock { t0: now, start: from });
                }
            }
            Cmd::Step(d) => {
                let base = if self.clock.is_some() { self.current() } else { self.want };
                self.clock = None;
                self.want = base.saturating_add_signed(d).min(self.last_frame());
            }
            Cmd::Seek { secs, .. } => {
                let i = ((secs.max(0.0) * fps).round() as u64).min(self.last_frame());
                self.want = i;
                if self.clock.is_some() {
                    self.clock = Some(Clock { t0: now, start: i });
                }
            }
            Cmd::ToggleLoop => self.looping = !self.looping,
        }
    }

    pub fn status(&self, meta: Option<&crate::canvas::shape::MediaMeta>) -> PlayerStatus {
        let fps = self.info().map(VideoInfo::fps)
            .or_else(|| meta.filter(|m| m.frame_rate[0] > 0).map(|m| f64::from(m.frame_rate[0]) / f64::from(m.frame_rate[1].max(1))));
        let frames = self.frame_count().or_else(|| meta.map(|m| u64::from(m.frames)));
        let loading = self.error.is_none()
            && self.tools.is_some()
            && (matches!(self.probe, Probe::Pending(_))
                || (self.clock.is_none() && self.shown.as_ref().map(|(i, _)| *i) != Some(self.want)));
        PlayerStatus {
            playing: self.clock.is_some(),
            looping: self.looping,
            pos: fps.map_or(0.0, |f| self.current() as f64 / f),
            duration: match (frames, fps) {
                (Some(n), Some(f)) => Some(n as f64 / f),
                _ => meta.map(|m| m.duration),
            },
            frame: self.current(),
            frames,
            loading,
            error: self.error.clone(),
            needs_ffmpeg: self.tools.is_none() && self.error.is_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = "\
[STREAM]
index=0
width=1920
height=1080
sample_aspect_ratio=1:1
r_frame_rate=30/1
avg_frame_rate=30000/1001
duration=10.010000
nb_frames=300
DISPOSITION:attached_pic=0
[SIDE_DATA]
rotation=-90
[/SIDE_DATA]
[/STREAM]
[FORMAT]
duration=10.050000
[/FORMAT]
";

    #[test]
    fn probe_output_with_rotation() {
        let i = parse_probe(PHONE).unwrap();
        assert_eq!(i.size, [1080, 1920]);
        assert_eq!(i.fps, [30000, 1001]);
        assert_eq!(i.frames, Some(300));
        assert!((i.duration.unwrap() - 10.01).abs() < 1e-9);
    }

    #[test]
    fn probe_output_skips_cover_art_and_estimates_frames() {
        let text = "\
[STREAM]
index=0
width=600
height=600
avg_frame_rate=0/0
r_frame_rate=90000/1
DISPOSITION:attached_pic=1
[/STREAM]
[STREAM]
index=1
width=1440
height=1080
sample_aspect_ratio=4:3
avg_frame_rate=0/0
r_frame_rate=25/1
nb_frames=N/A
duration=N/A
DISPOSITION:attached_pic=0
TAG:rotate=180
[/STREAM]
[FORMAT]
duration=4.000000
[/FORMAT]
";
        let i = parse_probe(text).unwrap();
        assert_eq!(i.stream, 1);
        assert_eq!(i.size, [1920, 1080], "anamorphic pixels squared, 180° keeps the shape");
        assert_eq!(i.fps, [25, 1]);
        assert_eq!(i.frames, None);
        assert_eq!(i.frame_estimate(), Some(100));
    }

    #[test]
    fn decode_size_caps_and_follows_the_view() {
        assert_eq!(decode_size([3840, 2160], None), [1920, 1080]);
        assert_eq!(decode_size([1280, 720], None), [1280, 720]);
        // Shown small: decode at 1.5x the on-screen size, not below 480.
        assert_eq!(decode_size([1920, 1080], Some([640.0, 360.0])), [960, 540]);
        assert_eq!(decode_size([1920, 1080], Some([100.0, 56.0])), [480, 270]);
        // Shown larger than the source: the source size.
        assert_eq!(decode_size([1280, 720], Some([2560.0, 1440.0])), [1280, 720]);
    }

    #[test]
    fn zooming_in_far_enough_asks_for_a_new_decode() {
        let native = [1920, 1080];
        let small = decode_size(native, Some([300.0, 169.0]));
        // A little zoom: the headroom covers it.
        assert!(size_fits(small, decode_size(native, Some([390.0, 219.0]))));
        // 3x zoom: frames would be stretched, so decode again.
        assert!(!size_fits(small, decode_size(native, Some([900.0, 506.0]))));
        // Zoomed far out: too big, decode smaller.
        let big = decode_size(native, Some([1280.0, 720.0]));
        assert!(!size_fits(big, decode_size(native, Some([200.0, 112.0]))));
    }

    #[test]
    fn seeks_a_quarter_frame_early() {
        let tools = Tools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into(), version: Some((7, 1)) };
        let info = VideoInfo { stream: 0, size: [4, 4], fps: [30, 1], frames: None, duration: None };
        let args: Vec<String> = decode_args(&tools, Path::new("v.mp4"), &info, 30, [4, 4])
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 1], format!("{:.6}", 29.75 / 30.0));
        assert!(args.contains(&"-fps_mode".to_string()));
        let none: Vec<String> = decode_args(&tools, Path::new("v.mp4"), &info, 0, [4, 4])
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!none.contains(&"-ss".to_string()));
    }

    /// Needs ffmpeg: decodes a generated clip whose frame number is
    /// written into its brightness and checks seeking lands exactly.
    #[test]
    #[ignore]
    fn seeking_lands_on_the_requested_frame() {
        let tools = super::super::ffmpeg::tools().expect("ffmpeg");
        let dir = std::env::temp_dir().join(format!("osn-seek-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("count.mp4");
        // Luma = 16 + 2 × frame number (limited range, so it maps to
        // RGB ≈ 2.33 × frame number); lossless, B-frames, a keyframe
        // every 50 frames so most seeks land mid-GOP.
        let status = tools
            .ffmpeg_cmd()
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=64x64:r=30:d=3.5",
                "-vf", "geq=lum='16+2*N':cb=128:cr=128", "-c:v", "libx264", "-qp", "0",
                "-g", "50", "-bf", "2", "-pix_fmt", "yuv444p"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(status.success());
        let info = probe(&tools, &clip).unwrap();
        for s in [0u64, 1, 7, 49, 50, 51, 77, 100, 104] {
            let mut d = spawn_decoder(&tools, &clip, &info, s, [64, 64], 1).unwrap();
            let Msg::Frame(_, i, img) = d.rx.recv().unwrap() else { panic!("no frame at {s}") };
            assert_eq!(i, s);
            let lum = f64::from(img.pixels[32 * 64 + 32].r());
            let expect = 2.0 * s as f64 * 255.0 / 219.0;
            assert!((lum - expect).abs() <= 1.0, "seek {s}: got brightness {lum}, expected {expect:.1}");
            d.child.kill().ok();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
