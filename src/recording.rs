//! Screen-recording pipeline.
//!
//! The user toggles recording via `Ctrl+Shift+R` (or a toolbar button
//! that fires the same event). A dedicated worker thread spawned at
//! `start()` does the heavy lifting: each tick it captures the primary
//! monitor with `xcap`, crops to the OSN window region, downsamples
//! to half resolution, and **streams the encoded frame directly into
//! the destination GIF on disk**. The main UI thread is untouched —
//! the only synchronisation is a `Sender<()>` stop signal and an
//! `Arc<Mutex<...>>` slot for the final result.
//!
//! ## Why a background thread
//!
//! `xcap::Monitor::capture_image()` plus NeuQuant palette quantisation
//! costs 30-100 ms per frame at 1080p. Doing that on the egui main
//! thread froze the overlay between captures (visible as choppy pen
//! ink while recording). Offloading to a worker makes the UI thread
//! pay only the cost of the atomic state read + a small mutex check
//! per frame.
//!
//! ## Why streaming instead of accumulating
//!
//! Earlier revisions kept every frame in a `Vec<CapturedFrame>` and
//! encoded at stop time. A 30-second 15 fps recording of a 1080p
//! display is 450 frames × ~8 MB raw RGBA ≈ 3.5 GB of resident
//! memory — that OOM'd the app before encode even started. Streaming
//! keeps memory flat (one frame plus the encoder's internal state).
//!
//! ## What the recording does NOT contain
//!
//! While recording, the app hides its own floating UI
//! (`ui_visible = false`) so the toolbar and status-badge do not
//! appear in the saved video. The overlay window itself stays visible
//! — that is what carries the ink strokes the user is drawing.
//! Everything else on screen (browser, IDE, etc.) ends up in the
//! recording exactly as it appears.

use crate::error::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Target capture frame-rate. 15 fps balances "smooth enough to read"
/// against GIF file size; browsers cap GIF at ~50 fps but quality
/// degrades fast above 20 anyway.
#[allow(dead_code)]
pub const TARGET_FPS: f32 = 15.0;
/// Minimum wall-clock interval between captures.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(66);
/// Auto-stop ceiling so a forgotten recording does not produce a
/// gigabyte GIF.
pub const MAX_DURATION_SECS: u64 = 30;
/// Downscale factor (integer) applied to each captured frame before
/// encoding. 2 = half resolution = 1/4 the pixels = ~4× faster
/// palette quantisation. Quality is still fine for screen-share GIFs.
pub const DOWNSCALE: u32 = 2;

/// Public state of the recorder, surfaced to the toolbar. The worker
/// thread updates an `AtomicU8` mirroring this enum; the main thread
/// reads it cheaply each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecState {
    Idle,
    Capturing,
    /// The worker is alive but skipping captures. Time spent here
    /// does not count toward `MAX_DURATION_SECS`.
    Paused,
    Encoding,
}

const STATE_IDLE: u8 = 0;
const STATE_CAPTURING: u8 = 1;
const STATE_PAUSED: u8 = 2;
const STATE_ENCODING: u8 = 3;

fn state_from_u8(v: u8) -> RecState {
    match v {
        STATE_CAPTURING => RecState::Capturing,
        STATE_PAUSED   => RecState::Paused,
        STATE_ENCODING => RecState::Encoding,
        _ => RecState::Idle,
    }
}

/// Commands the main thread sends to the worker.
enum Cmd {
    Stop,
    Pause,
    Resume,
}

/// Handle owned by `OnScreenNotesApp`. All heavy work runs on the
/// worker thread; this struct is just the control surface.
pub struct Recorder {
    /// Worker-shared state. Updated by the worker, read by the UI.
    state: Arc<AtomicU8>,
    /// Command channel into the worker. Dropped on stop so a second
    /// stop is a no-op and the worker exits if the sender is gone.
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    /// Worker thread handle. We never block-join the main thread on
    /// it; the worker writes its result into the shared slot and
    /// exits. Kept around so the OS reclaims the thread cleanly when
    /// `Recorder` drops.
    worker: Option<JoinHandle<()>>,
    /// Output slot for the worker's final `RecordResult`. Main thread
    /// polls via `take_result()`.
    result: Arc<Mutex<Option<RecordResult>>>,
    /// Worker-shared frame counter. Bumped by the worker after each
    /// successful `write_frame`; read by the UI for status display.
    frame_count: Arc<AtomicU8>, // wraps but only used for "any frames yet" UX hint
    /// Start time, owned by the main thread for the toolbar timer.
    started_at: Option<Instant>,
}

/// `CREATE_NO_WINDOW` Win32 flag — suppresses the brief console
/// pop-up Windows shows when a GUI app spawns a console child
/// process. No-op on non-Windows.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Apply `CREATE_NO_WINDOW` on Windows so child-process spawns from
/// the GUI binary do not flash a console window.
fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// One-shot detection of an `ffmpeg` binary on PATH. Cached at app
/// startup so toggling recording doesn't pay the spawn cost. Returns
/// `true` when `ffmpeg -version` exits 0.
pub fn detect_ffmpeg() -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut cmd)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Active encoder for a recording session. Worker uses the same
/// write/finalise API regardless of container; the path extension is
/// chosen up-front in `start()`.
enum Encoder {
    Gif(gif::Encoder<BufWriter<File>>),
    Mp4(Mp4Pipe),
}

/// ffmpeg child process accepting raw RGBA frames on stdin. Drop the
/// `stdin` field to close the pipe — ffmpeg then flushes and exits.
struct Mp4Pipe {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Mp4Pipe {
    fn spawn(path: &Path, w: u16, h: u16, fps: u32) -> Result<Self> {
        let size_arg = format!("{w}x{h}");
        let fps_arg = fps.to_string();
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
                "-y",
                "-f", "rawvideo",
                "-pix_fmt", "rgba",
                "-s", &size_arg,
                "-r", &fps_arg,
                "-i", "pipe:0",
                "-c:v", "libx264",
                "-pix_fmt", "yuv420p",
                "-crf", "23",
                "-preset", "veryfast",
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = hide_console(&mut cmd)
            .spawn()
            .context("spawning ffmpeg")?;
        let stdin = child.stdin.take().context("ffmpeg stdin missing")?;
        Ok(Self { child, stdin: Some(stdin) })
    }

    fn write_frame(&mut self, rgba: &[u8]) -> std::io::Result<()> {
        if let Some(s) = self.stdin.as_mut() {
            s.write_all(rgba)
        } else {
            Ok(())
        }
    }

    fn finish(mut self) {
        // Closing stdin signals end-of-input → ffmpeg writes the MP4
        // trailer and exits. Wait so the file is fully on disk by
        // the time we report success.
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// One downscaled RGBA frame.
struct DownsampledFrame {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
}

/// Outcome of a completed recording, used to drive the toast UI.
#[derive(Debug, Clone)]
pub struct RecordResult {
    pub path: PathBuf,
    pub copied_to_clipboard: bool,
    pub frame_count: usize,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(STATE_IDLE)),
            cmd_tx: None,
            worker: None,
            result: Arc::new(Mutex::new(None)),
            frame_count: Arc::new(AtomicU8::new(0)),
            started_at: None,
        }
    }
}

impl Recorder {
    #[allow(dead_code)]
    pub fn state(&self) -> RecState { state_from_u8(self.state.load(Ordering::Acquire)) }
    pub fn is_recording(&self) -> bool {
        let s = self.state.load(Ordering::Acquire);
        s == STATE_CAPTURING || s == STATE_PAUSED
    }
    #[allow(dead_code)]
    pub fn is_paused(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_PAUSED
    }
    #[allow(dead_code)]
    pub fn frame_count(&self) -> usize {
        self.frame_count.load(Ordering::Acquire) as usize
    }
    pub fn elapsed(&self) -> Duration {
        self.started_at.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// Start a new recording. Spawns the worker and returns
    /// immediately. No-op if already capturing or finalising.
    /// `use_mp4` decides the container: MP4 (via ffmpeg) when true,
    /// GIF otherwise. Caller is responsible for detecting ffmpeg
    /// availability up front.
    pub fn start(&mut self, dir: PathBuf, hwnd: isize, use_mp4: bool) {
        let cur = self.state.load(Ordering::Acquire);
        if cur != STATE_IDLE { return; }

        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("recording dir create failed: {e}");
            return;
        }
        let ext = if use_mp4 { "mp4" } else { "gif" };
        let path = dir.join(format!("on-screen-notes_{}.{ext}", timestamp()));
        log::info!("recording started ({ext}): {path:?}");

        // Reset shared output so a previous result does not leak in.
        *self.result.lock().unwrap() = None;
        self.frame_count.store(0, Ordering::Release);
        self.state.store(STATE_CAPTURING, Ordering::Release);
        self.started_at = Some(Instant::now());

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let state = Arc::clone(&self.state);
        let result = Arc::clone(&self.result);
        let frame_count = Arc::clone(&self.frame_count);
        let worker = std::thread::Builder::new()
            .name("osn-recorder".into())
            .spawn(move || worker_loop(state, result, frame_count, cmd_rx, path, hwnd, use_mp4))
            .ok();

        if worker.is_none() {
            log::warn!("recording worker thread spawn failed");
            self.state.store(STATE_IDLE, Ordering::Release);
            self.started_at = None;
            return;
        }
        self.cmd_tx = Some(cmd_tx);
        self.worker = worker;
    }

    /// Pause / resume an in-flight recording. No-op if not currently
    /// capturing or paused.
    pub fn toggle_pause(&mut self) {
        let cur = self.state.load(Ordering::Acquire);
        let cmd = match cur {
            STATE_CAPTURING => Cmd::Pause,
            STATE_PAUSED    => Cmd::Resume,
            _ => return,
        };
        if let Some(tx) = self.cmd_tx.as_ref() {
            let _ = tx.send(cmd);
        }
    }

    /// Legacy no-op kept so callers do not have to be deleted in lock
    /// step. The worker self-paces; the main thread does not drive
    /// per-frame capture any more.
    #[allow(dead_code)]
    pub fn tick(&mut self) {}

    /// Ask the worker to wrap up. Returns immediately; the worker
    /// finalises the GIF trailer + clipboard copy and writes its
    /// `RecordResult` into the shared slot, which the UI picks up
    /// via `take_result()` next frame.
    pub fn stop_and_encode(&mut self) {
        let cur = self.state.load(Ordering::Acquire);
        if cur != STATE_CAPTURING && cur != STATE_PAUSED { return; }
        // Send the stop signal; ignore errors (worker may have
        // already exited on its own e.g. via the 30-s auto-stop).
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Cmd::Stop);
        }
        // Detach the worker handle — it owns its own resources and
        // will exit shortly. Keeping a handle around just to
        // eventually drop it is fine.
        self.started_at = None;
    }

    /// Drain the most-recent encode result, if any.
    pub fn take_result(&mut self) -> Option<RecordResult> {
        let mut slot = self.result.lock().ok()?;
        let out = slot.take();
        if out.is_some() {
            // Worker has finished; clear the handle so we are clean
            // for the next recording.
            self.worker = None;
        }
        out
    }
}

/// Worker thread entry point. Owns the encoder, the file handle, and
/// the first-frame clipboard buffer. Exits on stop signal, max
/// duration, or fatal capture / encode error.
fn worker_loop(
    state: Arc<AtomicU8>,
    result: Arc<Mutex<Option<RecordResult>>>,
    frame_count_shared: Arc<AtomicU8>,
    cmd_rx: mpsc::Receiver<Cmd>,
    path: PathBuf,
    hwnd: isize,
    use_mp4: bool,
) {
    let started = Instant::now();
    let mut encoder: Option<Encoder> = None;
    let mut first_frame: Option<DownsampledFrame> = None;
    let mut frame_count: usize = 0;
    let mut next_capture = Instant::now();
    // Wall-clock instant of the previous successfully-encoded frame.
    // Used to set each new frame's `delay` to the real elapsed time
    // since the previous frame, so GIF playback runs at wall-clock
    // speed even when xcap capture is slow (it routinely takes
    // 80-150 ms per frame on Windows, well above FRAME_INTERVAL).
    let mut last_encoded_at: Option<Instant> = None;

    // Pause accounting. `paused` is the live flag; `pause_started` is
    // when the current pause began; `total_paused` accumulates so the
    // 30-second auto-stop timer can subtract paused time.
    let mut paused = false;
    let mut pause_started: Option<Instant> = None;
    let mut total_paused = Duration::ZERO;

    let mut stop = false;
    'main: loop {
        // Drain command channel. The channel itself being dropped
        // also signals shutdown (main thread gone).
        loop {
            match cmd_rx.try_recv() {
                Ok(Cmd::Stop) => { stop = true; break; }
                Ok(Cmd::Pause) => {
                    if !paused {
                        paused = true;
                        pause_started = Some(Instant::now());
                        state.store(STATE_PAUSED, Ordering::Release);
                        log::info!("recording paused");
                    }
                }
                Ok(Cmd::Resume) => {
                    if paused {
                        paused = false;
                        if let Some(t) = pause_started.take() {
                            total_paused += t.elapsed();
                        }
                        // Push next_capture forward so we don't burst
                        // a frame the instant we unpause.
                        next_capture = Instant::now() + FRAME_INTERVAL;
                        state.store(STATE_CAPTURING, Ordering::Release);
                        log::info!("recording resumed");
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => { stop = true; break; }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if stop { break 'main; }

        // Auto-stop only counts active-recording time, not paused.
        let active_elapsed = started.elapsed().saturating_sub(total_paused);
        if active_elapsed.as_secs() >= MAX_DURATION_SECS {
            log::warn!(
                "recording auto-stopped at {}s (MAX_DURATION_SECS)",
                MAX_DURATION_SECS
            );
            break;
        }

        // While paused, idle in a tight sleep so we still react
        // promptly to Resume / Stop commands.
        if paused {
            std::thread::sleep(Duration::from_millis(16));
            continue;
        }

        // Pace ourselves to FRAME_INTERVAL. Sleep in small slices so
        // we still notice the stop signal promptly.
        let now = Instant::now();
        if now < next_capture {
            let remaining = next_capture - now;
            // 16 ms slice = ~60 Hz responsiveness for the stop signal.
            let slice = remaining.min(Duration::from_millis(16));
            std::thread::sleep(slice);
            continue;
        }
        next_capture = now + FRAME_INTERVAL;

        match capture_primary(hwnd) {
            Ok(frame) => {
                // Lazy encoder init — needs the first frame's size.
                if encoder.is_none() {
                    let init = if use_mp4 {
                        let fps = (1000 / FRAME_INTERVAL.as_millis().max(1) as u32).max(1);
                        Mp4Pipe::spawn(&path, frame.width, frame.height, fps)
                            .map(Encoder::Mp4)
                    } else {
                        init_gif_encoder(&path, &frame).map(Encoder::Gif)
                    };
                    match init {
                        Ok(enc) => {
                            encoder = Some(enc);
                            first_frame = Some(DownsampledFrame {
                                width: frame.width,
                                height: frame.height,
                                rgba: frame.rgba.clone(),
                            });
                        }
                        Err(e) => {
                            log::warn!("encoder init failed: {e}; aborting recording");
                            break;
                        }
                    }
                }
                let enc = encoder.as_mut().expect("encoder just initialised");
                let now = Instant::now();
                let write_ok = match enc {
                    Encoder::Gif(g) => {
                        let mut buf = frame.rgba.clone();
                        // Speed 10: NeuQuant runs in ~80 ms at half-1080p; on
                        // a worker thread that does not block the UI, so we
                        // can comfortably hit the 66-ms frame interval as
                        // long as the underlying capture itself is fast
                        // enough.
                        let mut gif_frame = gif::Frame::from_rgba_speed(
                            frame.width, frame.height, &mut buf, 10,
                        );
                        // Real-time delay: measure ms since the
                        // previous frame, convert to centiseconds.
                        let delay_cs = match last_encoded_at {
                            Some(prev) => {
                                let ms = now.duration_since(prev).as_millis() as u32;
                                ((ms + 5) / 10).clamp(2, 100) as u16
                            }
                            None => (FRAME_INTERVAL.as_millis() as u16 / 10).max(2),
                        };
                        gif_frame.delay = delay_cs;
                        g.write_frame(&gif_frame)
                            .map_err(|e| crate::error::anyhow!("gif write_frame: {e}"))
                    }
                    Encoder::Mp4(m) => m
                        .write_frame(&frame.rgba)
                        .map_err(|e| crate::error::anyhow!("ffmpeg pipe write: {e}")),
                };
                if let Err(e) = write_ok {
                    log::warn!("write_frame failed: {e}; aborting recording");
                    break;
                }
                last_encoded_at = Some(now);
                frame_count += 1;
                // u8 wraps; this counter is only an "anything written
                // yet" UX hint, not a precise total.
                frame_count_shared.store(frame_count.min(255) as u8, Ordering::Release);
            }
            Err(e) => {
                log::warn!("capture frame failed: {e}");
                // Don't break — skip this frame and try again on the
                // next tick. Transient xcap failures recover.
            }
        }
    }

    // ---- Finalise ---------------------------------------------------------
    state.store(STATE_ENCODING, Ordering::Release);
    // GIF: drop the encoder so the BufWriter flushes and the trailer
    // is written. MP4: close stdin → ffmpeg writes moov + exits.
    match encoder.take() {
        Some(Encoder::Gif(g)) => drop(g),
        Some(Encoder::Mp4(m)) => m.finish(),
        None => {}
    }

    let mut copied = false;
    if let Some(f) = &first_frame {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let img = arboard::ImageData {
                width: f.width as usize,
                height: f.height as usize,
                bytes: std::borrow::Cow::Borrowed(&f.rgba),
            };
            match cb.set_image(img) {
                Ok(()) => {
                    log::info!("recording first frame copied to clipboard");
                    copied = true;
                }
                Err(e) => log::warn!("clipboard copy failed: {e}"),
            }
        }
    }

    if let Ok(mut slot) = result.lock() {
        *slot = Some(RecordResult {
            path,
            copied_to_clipboard: copied,
            frame_count,
        });
    }
    log::info!("recording stopped: {frame_count} frame(s)");
    state.store(STATE_IDLE, Ordering::Release);
}

/// Create the on-disk GIF encoder once we know the frame size.
fn init_gif_encoder(
    path: &PathBuf,
    frame: &DownsampledFrame,
) -> Result<gif::Encoder<BufWriter<File>>> {
    let file = File::create(path).with_context(|| format!("creating {path:?}"))?;
    let writer = BufWriter::new(file);
    let mut encoder =
        gif::Encoder::new(writer, frame.width, frame.height, &[]).context("init gif encoder")?;
    encoder.set_repeat(gif::Repeat::Infinite).ok();
    Ok(encoder)
}

/// Capture the primary monitor, optionally crop to the OSN window's
/// screen rect, then downsample to 1/`DOWNSCALE`.
///
/// When `hwnd != 0` on Windows, we crop the captured RGBA to the
/// region returned by `GetWindowRect(hwnd)`. That makes the saved
/// GIF contain only the user's target window (which OSN has been
/// resized to cover) instead of the entire desktop. Cropping
/// happens before downsample, so the destination dimensions still
/// match the GIF encoder's first-frame size as long as the user
/// doesn't resize OSN mid-recording — and even then `gif` will
/// happily clip subsequent frames at the encoder's recorded size.
fn capture_primary(hwnd: isize) -> Result<DownsampledFrame> {
    let monitors = xcap::Monitor::all().context("listing monitors")?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary())
        .unwrap_or(&monitors[0]);
    let img = monitor.capture_image().context("capturing screen")?;
    let src_w = img.width() as i32;
    let src_h = img.height() as i32;
    let src = img.into_raw();

    // Resolve crop region in monitor-physical pixels. Fall back to
    // the whole monitor if we cannot read OSN's rect.
    let (cx, cy, cw, ch) = if hwnd != 0 {
        match crate::platform::window_rect(hwnd) {
            Some((l, t, r, b)) => {
                let mut l = l.max(0);
                let mut t = t.max(0);
                let mut r = r.min(src_w);
                let mut b = b.min(src_h);
                if r <= l || b <= t {
                    l = 0; t = 0; r = src_w; b = src_h;
                }
                (l, t, r - l, b - t)
            }
            None => (0, 0, src_w, src_h),
        }
    } else {
        (0, 0, src_w, src_h)
    };

    let dst_w = ((cw as u32) / DOWNSCALE).max(2) as u16;
    let dst_h = ((ch as u32) / DOWNSCALE).max(2) as u16;
    let mut dst = vec![0u8; dst_w as usize * dst_h as usize * 4];
    let scale = DOWNSCALE as i32;
    let src_stride = src_w as usize * 4;
    for y in 0..dst_h as i32 {
        let sy = cy + y * scale;
        if sy < 0 || sy >= src_h { continue; }
        for x in 0..dst_w as i32 {
            let sx = cx + x * scale;
            if sx < 0 || sx >= src_w { continue; }
            let s = sy as usize * src_stride + sx as usize * 4;
            let d = (y as usize * dst_w as usize + x as usize) * 4;
            if s + 4 > src.len() { continue; }
            dst[d]     = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
            dst[d + 3] = src[s + 3];
        }
    }
    // Composite a synthetic cursor marker into the downsampled frame.
    // xcap (Win32 BitBlt) does NOT include the OS cursor; without
    // this, the recorded GIF shows no pointer and viewers can't tell
    // where the presenter is pointing. We draw a small filled disc
    // with a contrasting ring, switching to red while the mouse
    // button is down so clicks are visible too.
    if let Some((csx, csy)) = crate::platform::cursor_pos() {
        // Cursor in monitor-physical px → downsampled-frame px.
        let fx = (csx - cx) / DOWNSCALE as i32;
        let fy = (csy - cy) / DOWNSCALE as i32;
        if fx >= 0 && fy >= 0 && fx < dst_w as i32 && fy < dst_h as i32 {
            let down = crate::platform::lmb_down();
            let core = if down { [240, 60, 60, 255] } else { [255, 255, 255, 255] };
            let ring = [0u8, 0, 0, 180];
            draw_cursor_marker(
                &mut dst,
                dst_w as i32,
                dst_h as i32,
                fx,
                fy,
                /* core_radius */ 4,
                /* ring_radius */ 6,
                core,
                ring,
            );
        }
    }

    Ok(DownsampledFrame {
        width: dst_w,
        height: dst_h,
        rgba: dst,
    })
}

/// Paint a filled disc + outer ring into `buf` (RGBA, row-major,
/// width `w`, height `h`). Clipped at the buffer bounds — no panic
/// if the disc would partially overflow. Used by the recording
/// pipeline to make the cursor visible in saved GIFs.
fn draw_cursor_marker(
    buf: &mut [u8],
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    core_r: i32,
    ring_r: i32,
    core: [u8; 4],
    ring: [u8; 4],
) {
    let r2_core = core_r * core_r;
    let r2_ring = ring_r * ring_r;
    let y0 = (cy - ring_r).max(0);
    let y1 = (cy + ring_r).min(h - 1);
    let x0 = (cx - ring_r).max(0);
    let x1 = (cx + ring_r).min(w - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x - cx;
            let dy = y - cy;
            let d2 = dx * dx + dy * dy;
            let i = ((y as usize) * w as usize + x as usize) * 4;
            if i + 4 > buf.len() { continue; }
            if d2 <= r2_core {
                // Solid core — overwrite alpha-aware (premultiply
                // input alpha against background).
                blend_pixel(&mut buf[i..i + 4], core);
            } else if d2 <= r2_ring {
                blend_pixel(&mut buf[i..i + 4], ring);
            }
        }
    }
}

/// Alpha-blend `src` (straight RGBA) onto `dst` in place.
fn blend_pixel(dst: &mut [u8], src: [u8; 4]) {
    let sa = src[3] as u32;
    if sa == 0 { return; }
    if sa == 255 {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
        dst[3] = 255;
        return;
    }
    let inv = 255 - sa;
    dst[0] = ((src[0] as u32 * sa + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((src[1] as u32 * sa + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((src[2] as u32 * sa + dst[2] as u32 * inv) / 255) as u8;
    dst[3] = dst[3].max(src[3]);
}

/// `YYYYMMDD_HHMMSS` timestamp without pulling chrono — same format
/// the screenshot pipeline already uses.
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}{mo:02}{d:02}_{h:02}{m:02}{s:02}")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year: u64 = 1970;
    loop {
        let leap = is_leap(year);
        let dy = if leap { 366 } else { 365 };
        if days < dy { break; }
        days -= dy;
        year += 1;
    }
    let m_lengths = [31, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u64 = 1;
    for &len in &m_lengths {
        if days < len { break; }
        days -= len;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
