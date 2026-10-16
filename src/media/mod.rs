//! Videos and animated GIFs on the canvas.
//!
//! A media object is a `Shape::Raster` whose `media` field names a file
//! in `persistence::paths::media_dir()`. The slide stores only that name
//! and a small poster frame. Everything that moves — decoders, clocks,
//! GPU textures — lives in `MediaHub`, outside the canvas, so playing a
//! video never marks a slide dirty or lands in the undo history.

pub mod ffmpeg;
pub mod gif_player;
pub mod import;
pub mod pureref;
pub mod video_player;

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{MediaKind, MediaMeta};
use egui::{ColorImage, ImageData, TextureHandle, TextureId, TextureOptions};
use gif_player::{GifData, GifPlayer};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use video_player::VideoPlayer;

/// A playback command from the control bar or a hotkey.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmd {
    TogglePlay,
    /// Pause and move this many frames (negative = back).
    Step(i64),
    /// Jump to a time. `preview` while the scrubber is being dragged.
    Seek { secs: f64, preview: bool },
    ToggleLoop,
}

/// What the control bar shows for one object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerStatus {
    pub playing: bool,
    pub looping: bool,
    /// Current position, seconds.
    pub pos: f64,
    pub duration: Option<f64>,
    /// Index of the frame on screen.
    pub frame: u64,
    pub frames: Option<u64>,
    /// Waiting for the first frame or a seek.
    pub loading: bool,
    pub error: Option<String>,
    /// A video that cannot play until FFmpeg is installed.
    pub needs_ffmpeg: bool,
}

enum Player {
    Gif(GifPlayer),
    Video(Box<VideoPlayer>),
}

impl Player {
    fn tick(&mut self, now: Instant) {
        match self {
            Player::Gif(p) => p.tick(now),
            Player::Video(p) => p.tick(now),
        }
    }

    fn frame(&self) -> Option<&Arc<ColorImage>> {
        match self {
            Player::Gif(p) => p.frame(),
            Player::Video(p) => p.frame(),
        }
    }

    fn position(&self) -> f64 {
        match self {
            Player::Gif(p) => p.position(),
            Player::Video(p) => p.position(),
        }
    }
}

struct Entry {
    player: Player,
    meta: Option<MediaMeta>,
    texture: Option<TextureHandle>,
    /// The frame now in `texture`, to skip re-uploading it.
    uploaded: Option<Arc<ColorImage>>,
    /// `MediaHub::tick` when the object was last seen on screen.
    seen: u64,
}

impl Entry {
    fn upload(&mut self, ctx: &egui::Context, id: u64) {
        let Some(frame) = self.player.frame() else { return };
        if self.uploaded.as_ref().is_some_and(|u| Arc::ptr_eq(u, frame)) { return; }
        let image = ImageData::Color(Arc::clone(frame));
        let opts = TextureOptions::LINEAR;
        match &mut self.texture {
            // Same size: overwrite in place. A full `set` would make the
            // renderer allocate a new GPU texture every frame.
            Some(t) if t.size() == frame.size => t.set_partial([0, 0], image, opts),
            Some(t) => t.set(image, opts),
            None => self.texture = Some(ctx.load_texture(format!("osn_media_{id}"), image, opts)),
        }
        self.uploaded = Some(Arc::clone(frame));
    }
}

/// How long a decoded GIF nobody shows is kept, so flipping back to its
/// slide does not decode it again.
const GIF_KEEP: Duration = Duration::from_secs(90);

/// Players for the media objects on the active slide.
#[derive(Default)]
pub struct MediaHub {
    entries: HashMap<u64, Entry>,
    /// Positions of objects that left the screen (slide switch, hidden
    /// layer, undo), keyed by `MediaRef::id`, to pick up where they were.
    resume: HashMap<u64, f64>,
    /// Decoded GIFs by file name, shared by every object showing the
    /// same file, with the time each was last used.
    gifs: HashMap<String, (Arc<GifData>, Instant)>,
    tick: u64,
}

impl MediaHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Once per frame, before painting: start players for the media on
    /// `canvas`'s visible layers, stop those no longer there, advance
    /// every clock and upload new frames. `ppp` is pixels per point.
    pub fn sync(&mut self, ctx: &egui::Context, canvas: &Canvas, ppp: f32) {
        self.tick += 1;
        let tick = self.tick;
        let now = Instant::now();
        let dir = crate::persistence::paths::media_dir().ok();
        for layer in canvas.layers.iter().filter(|l| l.visible) {
            for shape in &layer.shapes {
                let Some(m) = shape.media() else { continue };
                let half = shape.obb().half;
                let on_screen = [half[0] * 2.0 * canvas.zoom * ppp, half[1] * 2.0 * canvas.zoom * ppp];
                let entry = match self.entries.entry(m.id) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(v) => {
                        let path = dir.as_ref().map(|d| d.join(&m.file)).unwrap_or_default();
                        let resume = self.resume.remove(&m.id);
                        let player = match m.kind {
                            MediaKind::Gif => {
                                let data = match self.gifs.get(&m.file) {
                                    Some((d, _)) => Arc::clone(d),
                                    None => {
                                        let d = GifData::load(path);
                                        self.gifs.insert(m.file.clone(), (Arc::clone(&d), now));
                                        d
                                    }
                                };
                                Player::Gif(GifPlayer::new(data, resume))
                            }
                            MediaKind::Video => Player::Video(Box::new(VideoPlayer::new(path, ffmpeg::tools(), resume))),
                        };
                        v.insert(Entry { player, meta: m.meta, texture: None, uploaded: None, seen: tick })
                    }
                };
                entry.seen = tick;
                if let Player::Video(v) = &mut entry.player {
                    v.set_on_screen(on_screen);
                }
                if m.kind == MediaKind::Gif {
                    if let Some(g) = self.gifs.get_mut(&m.file) { g.1 = now; }
                }
            }
        }

        // Objects gone from view: remember where they were, then drop
        // the player (which stops its decoder).
        let resume = &mut self.resume;
        self.entries.retain(|id, e| {
            if e.seen == tick { return true; }
            resume.insert(*id, e.player.position());
            false
        });
        if resume.len() > 512 { resume.clear(); }
        self.gifs.retain(|_, (data, used)| {
            let keep = Arc::strong_count(data) > 1 || now.duration_since(*used) < GIF_KEEP;
            if !keep { data.cancel(); }
            keep
        });

        for (id, e) in &mut self.entries {
            e.player.tick(now);
            e.upload(ctx, *id);
        }
    }

    /// FFmpeg was just installed or located: restart the video players
    /// so they pick it up.
    pub fn ffmpeg_changed(&mut self) {
        self.entries.retain(|_, e| !matches!(e.player, Player::Video(_)));
    }

    /// The texture showing object `id`'s current frame, once it has one.
    pub fn texture(&self, id: u64) -> Option<TextureId> {
        let e = self.entries.get(&id)?;
        e.uploaded.as_ref()?;
        e.texture.as_ref().map(TextureHandle::id)
    }

    pub fn status(&self, id: u64) -> Option<PlayerStatus> {
        let e = self.entries.get(&id)?;
        Some(match &e.player {
            Player::Gif(p) => p.status(e.meta.as_ref()),
            Player::Video(p) => p.status(e.meta.as_ref()),
        })
    }

    /// How soon a GIF or video on screen shows its next frame, while one
    /// is playing or waiting for a frame. The app repaints by this rather
    /// than flat out: a GIF changes a few dozen times a second at most,
    /// and drawing a thousand frames a second for it kept the GPU busy.
    pub fn next_frame_in(&self) -> Option<Duration> {
        let mut soonest: Option<f64> = None;
        for (id, e) in &self.entries {
            let next = match &e.player {
                Player::Gif(p) => p.next_change_in(),
                Player::Video(_) => self.status(*id).filter(|s| s.playing || s.loading).map(|s| {
                    let [num, den] = e.meta.map_or([30, 1], |m| m.frame_rate);
                    if s.loading || num == 0 { 0.05 } else { f64::from(den.max(1)) / f64::from(num) }
                }),
            };
            if let Some(n) = next {
                soonest = Some(soonest.map_or(n, |s| s.min(n)));
            }
        }
        soonest.map(|s| Duration::from_secs_f64(s.clamp(0.0, 1.0)))
    }

    pub fn command(&mut self, id: u64, cmd: Cmd) {
        let Some(e) = self.entries.get_mut(&id) else { return };
        match &mut e.player {
            Player::Gif(p) => p.command(cmd),
            Player::Video(p) => p.command(cmd, Instant::now()),
        }
    }

    /// Current frame of every playing object, for screenshots.
    pub fn current_frames(&self) -> HashMap<u64, Arc<ColorImage>> {
        self.entries
            .iter()
            .filter_map(|(id, e)| e.player.frame().map(|f| (*id, Arc::clone(f))))
            .collect()
    }
}
