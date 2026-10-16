//! Animated GIF playback, decoded in-process (no FFmpeg needed).
//!
//! A background thread decodes every frame once — `image`'s GIF
//! decoder composites each frame onto the previous ones, so what comes
//! out is exactly what a browser shows — and appends it to a shared
//! `GifData`. Players read from that, so playback starts on the first
//! frame while the rest are still decoding, and stepping backwards is
//! just an index change.

use super::{Cmd, PlayerStatus};
use crate::error::{Context, Result};
use egui::ColorImage;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Most memory one GIF's decoded frames may take. Larger GIFs are
/// scaled down as they decode so they fit.
const FRAME_BUDGET: usize = 192 * 1024 * 1024;

/// One decoded frame.
#[derive(Clone)]
pub struct GifFrame {
    pub image: Arc<ColorImage>,
    /// How long it stays up, in seconds.
    pub delay: f64,
}

/// A GIF's frames, filled in by the decoder thread.
pub struct GifData {
    frames: Mutex<Vec<GifFrame>>,
    done: AtomicBool,
    error: Mutex<Option<String>>,
    /// Set when nothing needs this GIF any more; the decoder stops.
    stop: AtomicBool,
}

impl GifData {
    /// Start decoding `path` on a background thread.
    pub fn load(path: PathBuf) -> Arc<Self> {
        let data = Arc::new(Self {
            frames: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
            error: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&data);
        let spawned = std::thread::Builder::new().name("osn-gif-decode".into()).spawn(move || {
            if let Err(e) = decode_into(&path, &worker) {
                log::warn!("GIF decode of {} failed: {e:#}", path.display());
                *worker.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{e:#}"));
            }
            worker.done.store(true, Ordering::Release);
        });
        if let Err(e) = spawned {
            *data.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            data.done.store(true, Ordering::Release);
        }
        data
    }

    /// Ask the decoder to stop early.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Size and timing of a GIF, read without decoding any pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GifScan {
    pub size: [u32; 2],
    pub frames: u32,
    /// Total length in seconds, with `frame_delay`'s rules applied.
    pub duration: f64,
}

/// Count frames and add up delays. Skips LZW decoding, so it is fast
/// even for large GIFs.
pub fn scan(path: &Path) -> Result<GifScan> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    scan_reader(BufReader::new(file))
}

/// `scan` for a GIF already in memory or behind any reader.
pub fn scan_reader<R: std::io::Read>(reader: R) -> Result<GifScan> {
    let mut opts = gif::DecodeOptions::new();
    opts.skip_frame_decoding(true);
    let mut dec = opts.read_info(reader).context("reading GIF header")?;
    let size = [u32::from(dec.width()), u32::from(dec.height())];
    let (mut frames, mut duration) = (0u32, 0.0);
    while let Some(frame) = dec.read_next_frame().context("reading GIF frame")? {
        frames += 1;
        duration += frame_delay(f64::from(frame.delay) * 10.0);
    }
    Ok(GifScan { size, frames, duration })
}

/// A frame's display time in seconds from its stored delay in
/// milliseconds. Browsers show 0–10 ms delays (common in old GIFs that
/// meant "as fast as possible") as 100 ms; so do we, so GIFs play at
/// the speed people are used to.
pub fn frame_delay(ms: f64) -> f64 {
    if ms <= 10.0 { 0.1 } else { ms / 1000.0 }
}

/// Factor that brings `frames` frames of `size` under `budget` bytes.
fn fit_scale(size: [u32; 2], frames: u32, budget: usize) -> f64 {
    let bytes = size[0] as f64 * size[1] as f64 * 4.0 * frames.max(1) as f64;
    if bytes <= budget as f64 { 1.0 } else { (budget as f64 / bytes).sqrt() }
}

/// The first frame, composited, at full size — the poster for an
/// import.
pub fn first_frame(path: &Path) -> Result<image::RgbaImage> {
    use image::AnimationDecoder;
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let dec = image::codecs::gif::GifDecoder::new(BufReader::new(file)).context("reading GIF")?;
    let frame = dec.into_frames().next().context("GIF has no frames")?.context("decoding GIF frame")?;
    Ok(frame.into_buffer())
}

fn decode_into(path: &Path, data: &GifData) -> Result<()> {
    use image::AnimationDecoder;
    let scan = scan(path)?;
    let scale = fit_scale(scan.size, scan.frames, FRAME_BUDGET);
    let target = [
        ((scan.size[0] as f64 * scale).round() as u32).max(1),
        ((scan.size[1] as f64 * scale).round() as u32).max(1),
    ];
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let dec = image::codecs::gif::GifDecoder::new(BufReader::new(file)).context("reading GIF")?;
    for frame in dec.into_frames() {
        if data.stop.load(Ordering::Acquire) { return Ok(()); }
        let frame = frame.context("decoding GIF frame")?;
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = frame_delay(f64::from(num) / f64::from(den.max(1)));
        let mut buf = frame.into_buffer();
        if scale < 1.0 {
            buf = image::imageops::resize(&buf, target[0], target[1], image::imageops::FilterType::Triangle);
        }
        let size = [buf.width() as usize, buf.height() as usize];
        let image = Arc::new(ColorImage::from_rgba_unmultiplied(size, buf.as_raw()));
        data.frames.lock().unwrap_or_else(|e| e.into_inner()).push(GifFrame { image, delay });
    }
    Ok(())
}

/// One GIF object's playback state.
pub struct GifPlayer {
    data: Arc<GifData>,
    /// Local copy of the decoded frames so far (cheap `Arc` clones).
    frames: Vec<GifFrame>,
    /// `starts[i]` = time at which frame `i` begins.
    starts: Vec<f64>,
    index: usize,
    /// Seconds already spent showing `index`.
    into_frame: f64,
    playing: bool,
    looping: bool,
    last_tick: Option<Instant>,
}

impl GifPlayer {
    /// GIFs autoplay and loop, like they do on the web. `resume` is a
    /// position in seconds to start from.
    pub fn new(data: Arc<GifData>, resume: Option<f64>) -> Self {
        let mut p = Self {
            data,
            frames: Vec::new(),
            starts: Vec::new(),
            index: 0,
            into_frame: 0.0,
            playing: true,
            looping: true,
            last_tick: None,
        };
        p.pull_frames();
        if let Some(t) = resume { p.seek(t); }
        p
    }

    fn pull_frames(&mut self) {
        let shared = self.data.frames.lock().unwrap_or_else(|e| e.into_inner());
        if shared.len() > self.frames.len() {
            for f in &shared[self.frames.len()..] {
                let start = self.starts.last().copied().unwrap_or(0.0)
                    + self.frames.last().map(|l| l.delay).unwrap_or(0.0);
                self.starts.push(start);
                self.frames.push(f.clone());
            }
        }
    }

    fn done(&self) -> bool {
        self.data.done.load(Ordering::Acquire)
    }

    /// Advance the clock to `now`.
    pub fn tick(&mut self, now: Instant) {
        self.pull_frames();
        // A long stall (window hidden, debugger) should not fast-forward
        // through dozens of frames on return.
        let dt = self.last_tick.map(|t| now.duration_since(t).as_secs_f64().min(0.5)).unwrap_or(0.0);
        self.last_tick = Some(now);
        if !self.playing || self.frames.len() < 2 { return; }
        self.into_frame += dt;
        while self.into_frame >= self.frames[self.index].delay {
            if self.index + 1 < self.frames.len() {
                self.into_frame -= self.frames[self.index].delay;
                self.index += 1;
            } else if !self.done() {
                // Caught up with the decoder; hold here until it adds more.
                self.into_frame = 0.0;
                break;
            } else if self.looping {
                self.into_frame -= self.frames[self.index].delay;
                self.index = 0;
            } else {
                self.into_frame = 0.0;
                self.playing = false;
                break;
            }
        }
    }

    pub fn frame(&self) -> Option<&Arc<ColorImage>> {
        self.frames.get(self.index).map(|f| &f.image)
    }

    fn seek(&mut self, secs: f64) {
        if self.frames.is_empty() { return; }
        let secs = secs.max(0.0);
        self.index = self.starts.partition_point(|s| *s <= secs).saturating_sub(1).min(self.frames.len() - 1);
        self.into_frame = (secs - self.starts[self.index]).clamp(0.0, self.frames[self.index].delay);
    }

    pub fn position(&self) -> f64 {
        self.starts.get(self.index).copied().unwrap_or(0.0) + self.into_frame
    }

    pub fn command(&mut self, cmd: Cmd) {
        let n = self.frames.len();
        match cmd {
            Cmd::TogglePlay => {
                self.playing = !self.playing;
                // Play from the end restarts.
                if self.playing && !self.looping && self.done() && self.index + 1 == n {
                    self.index = 0;
                    self.into_frame = 0.0;
                }
            }
            Cmd::Step(d) => {
                self.playing = false;
                self.into_frame = 0.0;
                if n > 0 {
                    // Wrap around, as the GIF itself does.
                    self.index = (self.index as i64 + d).rem_euclid(n as i64) as usize;
                }
            }
            Cmd::Seek { secs, .. } => self.seek(secs),
            Cmd::ToggleLoop => self.looping = !self.looping,
        }
    }

    pub fn status(&self, meta: Option<&crate::canvas::shape::MediaMeta>) -> PlayerStatus {
        let done = self.done();
        let error = self.data.error.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let decoded_len = self.starts.last().copied().unwrap_or(0.0) + self.frames.last().map(|l| l.delay).unwrap_or(0.0);
        PlayerStatus {
            playing: self.playing,
            looping: self.looping,
            pos: self.position(),
            duration: if done { Some(decoded_len) } else { meta.map(|m| m.duration) },
            frame: self.index as u64,
            frames: if done { Some(self.frames.len() as u64) } else { meta.map(|m| u64::from(m.frames)) },
            loading: self.frames.is_empty() && error.is_none(),
            error: if self.frames.is_empty() { error } else { None },
            needs_ffmpeg: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_zero_delays_play_at_browser_speed() {
        assert_eq!(frame_delay(0.0), 0.1);
        assert_eq!(frame_delay(10.0), 0.1);
        assert!((frame_delay(20.0) - 0.02).abs() < 1e-9);
        assert!((frame_delay(70.0) - 0.07).abs() < 1e-9);
    }

    #[test]
    fn big_gifs_are_scaled_to_the_budget() {
        assert_eq!(fit_scale([100, 100], 10, 1 << 20), 1.0);
        let s = fit_scale([1000, 1000], 100, 100_000_000);
        let bytes = (1000.0 * s) * (1000.0 * s) * 4.0 * 100.0;
        assert!(bytes <= 100_000_000.0 * 1.0001, "{bytes}");
    }

    fn player(delays: &[f64]) -> GifPlayer {
        let data = Arc::new(GifData {
            frames: Mutex::new(
                delays
                    .iter()
                    .map(|&delay| GifFrame { image: Arc::new(ColorImage::new([1, 1], egui::Color32::BLACK)), delay })
                    .collect(),
            ),
            done: AtomicBool::new(true),
            error: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        GifPlayer::new(data, None)
    }

    #[test]
    fn plays_loops_and_steps() {
        let mut p = player(&[0.1, 0.2, 0.1]);
        let t0 = Instant::now();
        p.tick(t0);
        p.tick(t0 + std::time::Duration::from_millis(150));
        assert_eq!(p.index, 1);
        p.tick(t0 + std::time::Duration::from_millis(450));
        assert_eq!(p.index, 0, "looped back to the start");
        p.command(Cmd::Step(-1));
        assert_eq!((p.index, p.playing), (2, false));
        p.command(Cmd::Step(1));
        assert_eq!(p.index, 0);
        p.command(Cmd::Seek { secs: 0.25, preview: false });
        assert_eq!(p.index, 1);
    }
}
