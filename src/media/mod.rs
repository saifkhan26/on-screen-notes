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

    fn is_playing(&self) -> bool {
        match self {
            Player::Gif(p) => p.is_playing(),
            Player::Video(p) => p.is_playing(),
        }
    }

    fn neighbour(&self, step: i8) -> Option<&Arc<ColorImage>> {
        match self {
            Player::Gif(p) => p.neighbour(isize::from(step)),
            Player::Video(p) => p.neighbour(i64::from(step)),
        }
    }
}

struct Entry {
    player: Player,
    meta: Option<MediaMeta>,
    texture: Option<TextureHandle>,
    /// The frame now in `texture` (with the onion skin, if on).
    uploaded: Option<Arc<ColorImage>>,
    /// Which frames `uploaded` was made from — the shown one and the two
    /// onion-skin ones, by address — to skip remaking it.
    made_from: [usize; 3],
    /// `MediaHub::tick` when the object was last seen on screen.
    seen: u64,
}

impl Entry {
    /// Put the frame on show into the texture — with `onion` and paused,
    /// its neighbours ghosted in (`onion_skin`).
    fn upload(&mut self, ctx: &egui::Context, id: u64, onion: bool) {
        let Some(frame) = self.player.frame() else { return };
        let ghosts = if onion && !self.player.is_playing() {
            [self.player.neighbour(-1), self.player.neighbour(1)]
        } else {
            [None, None]
        };
        let addr = |f: Option<&Arc<ColorImage>>| f.map_or(0, |f| Arc::as_ptr(f) as usize);
        let made_from = [addr(Some(frame)), addr(ghosts[0]), addr(ghosts[1])];
        if self.uploaded.is_some() && self.made_from == made_from { return; }
        let shown = if ghosts.iter().any(Option::is_some) {
            Arc::new(onion_skin(frame, ghosts[0].map(|g| &**g), ghosts[1].map(|g| &**g)))
        } else {
            Arc::clone(frame)
        };
        self.made_from = made_from;
        let frame = &shown;
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

/// How strongly a ghost shows where it differs most from the frame.
const GHOST_MAX: f32 = 0.6;

/// `frame` with the frame before it ghosted in red and the one after in
/// green — only where they differ from it, so what stays still stays
/// clean and what moves shows where it came from and where it goes. A
/// ghost is its frame pulled toward the tint, so dark lines show in
/// colour too; it is stronger where the neighbour is darker than the
/// frame (ink that was or will be there) than where it is lighter, so
/// the frame's own lines keep their weight. Neighbours of another size
/// (decoded before a resize) are left out.
pub fn onion_skin(frame: &ColorImage, before: Option<&ColorImage>, after: Option<&ColorImage>) -> ColorImage {
    let mut out = frame.clone();
    for (ghost, tint) in [(before, [1.0, 0.22, 0.22]), (after, [0.25, 1.0, 0.35])] {
        let Some(g) = ghost.filter(|g| g.size == frame.size) else { continue };
        for ((o, c), n) in out.pixels.iter_mut().zip(&frame.pixels).zip(&g.pixels) {
            let ([cr, cg, cb, ca], [nr, ng, nb, _]) = (c.to_array(), n.to_array());
            if ca == 0 {
                continue;
            }
            let diff = f32::from(cr.abs_diff(nr)) + f32::from(cg.abs_diff(ng)) + f32::from(cb.abs_diff(nb));
            // A little noise (compression, dithering) shows nothing.
            let w = ((diff / (3.0 * 255.0) - 0.04) * 5.0).clamp(0.0, 1.0) * GHOST_MAX;
            if w <= 0.0 {
                continue;
            }
            let lum = |r: u8, g: u8, b: u8| 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
            let w = if lum(nr, ng, nb) < lum(cr, cg, cb) { w } else { w * 0.4 };
            let [or, og, ob, oa] = o.to_array();
            let mix = |was: u8, ghost: u8, t: f32| {
                let ghost = f32::from(ghost) * 0.45 + t * 255.0 * 0.55;
                let v = f32::from(was) * (1.0 - w) + ghost * w;
                (v.round() as u8).min(oa)
            };
            *o = egui::Color32::from_rgba_premultiplied(mix(or, nr, tint[0]), mix(og, ng, tint[1]), mix(ob, nb, tint[2]), oa);
        }
    }
    out
}

/// How long a decoded GIF nobody shows is kept, so flipping back to its
/// slide does not decode it again.
const GIF_KEEP: Duration = Duration::from_secs(90);

/// Most videos woken for a sharper frame that may be starting up (probe
/// and first decode) at once. A play or step from the user wakes its
/// video regardless.
const MAX_STARTING: usize = 2;

/// A dormant video is woken for a sharp frame once it is shown this
/// many times bigger than its poster.
const BLURRY_POSTER: f32 = 1.5;

/// Players for the media objects on the active slide.
#[derive(Default)]
pub struct MediaHub {
    entries: HashMap<u64, Entry>,
    /// Position, and whether it was playing, of each object that left
    /// the screen (slide switch, hidden layer, undo), keyed by
    /// `MediaRef::id`, to pick up where it was.
    resume: HashMap<u64, (f64, bool)>,
    /// Onion skin: a paused GIF or video shows the frames either side of
    /// its own, ghosted in (`onion_skin`).
    onion: bool,
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
    /// `canvas`'s visible layers and in `pins` that are in `view` (the
    /// canvas area on screen), stop the rest, advance every clock and
    /// upload new frames. `ppp` is pixels per point. A pinned video is
    /// the same object as on its own slide, so both show one player.
    ///
    /// Off screen, an object needs no player: its poster is all that
    /// could show. A video paused at its start stays dormant too, until
    /// it is played or shown bigger than its poster; at most
    /// `MAX_STARTING` videos start up at a time.
    pub fn sync(&mut self, ctx: &egui::Context, canvas: &Canvas, pins: &[crate::canvas::pins::Pinned], ppp: f32, view: egui::Rect) {
        self.tick += 1;
        let tick = self.tick;
        let now = Instant::now();
        let dir = crate::persistence::paths::media_dir().ok();
        let near = view.expand(view.width().max(view.height()) * 0.25);
        // Dormant videos shown bigger than their poster: (id, how much
        // bigger).
        let mut blurry: Vec<(u64, f32)> = Vec::new();
        // Every media object that shows: the shape, its box on screen,
        // and its size there in pixels.
        let mut shown: Vec<(&crate::canvas::shape::Shape, egui::Rect, [f32; 2])> = Vec::new();
        for layer in canvas.layers.iter().filter(|l| l.visible) {
            for shape in layer.shapes.iter().filter(|s| s.media().is_some()) {
                let bb = shape.obb().aabb();
                let to_screen = |x: f32, y: f32| {
                    egui::pos2(view.min.x + canvas.pan[0] + x * canvas.zoom, view.min.y + canvas.pan[1] + y * canvas.zoom)
                };
                let half = shape.obb().half;
                let on_screen = [half[0] * 2.0 * canvas.zoom * ppp, half[1] * 2.0 * canvas.zoom * ppp];
                shown.push((shape, egui::Rect::from_two_pos(to_screen(bb[0], bb[1]), to_screen(bb[2], bb[3])), on_screen));
            }
        }
        for p in pins {
            for m in p.members {
                let crate::canvas::hit::ObjRef::Shape(i) = m.obj else { continue };
                // A pin shows even when its layer is hidden.
                let Some(layer) = p.canvas.layers.get(m.layer) else { continue };
                let Some(shape) = layer.shapes.get(i).filter(|s| s.media().is_some()) else { continue };
                let o = p.shape_obb(shape);
                let [x0, y0, x1, y1] = o.aabb();
                let rect = egui::Rect::from_min_max(view.min + egui::vec2(x0, y0), view.min + egui::vec2(x1, y1));
                shown.push((shape, rect, [o.half[0] * 2.0 * ppp, o.half[1] * 2.0 * ppp]));
            }
        }
        // An object on its slide and in its pin is one player: merge the
        // two showings, the bigger size counting.
        let mut merged: Vec<(&crate::canvas::shape::Shape, egui::Rect, [f32; 2])> = Vec::with_capacity(shown.len());
        let mut at: HashMap<u64, usize> = HashMap::new();
        for (shape, rect, size) in shown {
            let id = shape.media().map_or(0, |m| m.id);
            match at.get(&id) {
                Some(&k) => {
                    let e = &mut merged[k];
                    e.1 = e.1.union(rect);
                    e.2 = [e.2[0].max(size[0]), e.2[1].max(size[1])];
                }
                None => {
                    at.insert(id, merged.len());
                    merged.push((shape, rect, size));
                }
            }
        }
        for (shape, rect, on_screen) in merged {
            let Some(m) = shape.media() else { continue };
            if !rect.intersects(near) {
                continue;
            }
            let entry = match self.entries.entry(m.id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    let path = dir.as_ref().map(|d| d.join(&m.file)).unwrap_or_default();
                    // Nothing autoplays: a player starts paused unless
                    // the user had this object playing when it left the
                    // screen. Ids are minted per run, so after a restart
                    // every GIF and video waits for the user again.
                    let (resume, playing) = match self.resume.remove(&m.id) {
                        Some((pos, playing)) => (Some(pos), playing),
                        None => (None, false),
                    };
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
                            Player::Gif(GifPlayer::new(data, resume, playing))
                        }
                        MediaKind::Video => Player::Video(Box::new(VideoPlayer::new(path, ffmpeg::tools(), resume, playing))),
                    };
                    v.insert(Entry { player, meta: m.meta, texture: None, uploaded: None, made_from: [0; 3], seen: tick })
                }
            };
            entry.seen = tick;
            if let Player::Video(v) = &mut entry.player {
                v.set_on_screen(on_screen);
                v.set_onion(self.onion);
                if v.is_dormant() && view.intersects(rect) {
                    let poster = match shape {
                        crate::canvas::shape::Shape::Raster { px_size, .. } => px_size[0].max(px_size[1]).max(1) as f32,
                        _ => 1.0,
                    };
                    let ratio = on_screen[0].max(on_screen[1]) / poster;
                    if ratio > BLURRY_POSTER {
                        blurry.push((m.id, ratio));
                    }
                }
            }
            if m.kind == MediaKind::Gif {
                if let Some(g) = self.gifs.get_mut(&m.file) { g.1 = now; }
            }
        }

        // Objects gone from view: remember where they were, then drop
        // the player (which stops its decoder). A dormant video has
        // nothing to remember, and must come back dormant.
        let resume = &mut self.resume;
        self.entries.retain(|id, e| {
            if e.seen == tick { return true; }
            if !matches!(&e.player, Player::Video(v) if v.is_dormant()) {
                resume.insert(*id, (e.player.position(), e.player.is_playing()));
            }
            false
        });

        // Wake the most blurred dormant videos, a few at a time.
        let starting = self.entries.values().filter(|e| matches!(&e.player, Player::Video(v) if v.is_starting())).count();
        blurry.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (id, _) in blurry.into_iter().take(MAX_STARTING.saturating_sub(starting)) {
            if let Some(Entry { player: Player::Video(v), .. }) = self.entries.get_mut(&id) {
                v.wake();
            }
        }
        if resume.len() > 512 { resume.clear(); }
        self.gifs.retain(|_, (data, used)| {
            let keep = Arc::strong_count(data) > 1 || now.duration_since(*used) < GIF_KEEP;
            if !keep { data.cancel(); }
            keep
        });

        let onion = self.onion;
        for (id, e) in &mut self.entries {
            e.player.tick(now);
            e.upload(ctx, *id, onion);
        }
    }

    /// Onion skin on or off, for every GIF and video.
    pub fn set_onion(&mut self, on: bool) {
        self.onion = on;
    }

    pub fn onion(&self) -> bool {
        self.onion
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
                Player::Video(v) if v.onion_pending() => Some(0.05),
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

    /// Current frame of every playing object, for screenshots. Never the
    /// onion skin, which is a view of the frames, not one of them.
    pub fn current_frames(&self) -> HashMap<u64, Arc<ColorImage>> {
        self.entries
            .iter()
            .filter_map(|(id, e)| e.player.frame().map(|f| (*id, Arc::clone(f))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    /// A white 4×1 frame with a black dot at `x`.
    fn dot_at(x: usize) -> ColorImage {
        let mut img = ColorImage::new([4, 1], Color32::WHITE);
        img.pixels[x] = Color32::BLACK;
        img
    }

    #[test]
    fn the_onion_skin_ghosts_only_what_moved() {
        // The dot went 0 -> 1 -> 2.
        let (before, now, after) = (dot_at(0), dot_at(1), dot_at(2));
        let out = onion_skin(&now, Some(&before), Some(&after));
        let [r, g, b, _] = out.pixels[0].to_array();
        assert!(r > g && r > b && g < 200, "where it was: a red ghost, {:?}", out.pixels[0]);
        let [r, g, b, _] = out.pixels[2].to_array();
        assert!(g > r && g > b && r < 200, "where it goes: a green ghost, {:?}", out.pixels[2]);
        assert_eq!(out.pixels[3], Color32::WHITE, "what never changed stays as it was");
        let lum = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        assert!(lum(out.pixels[1]) < 300, "the dot itself stays dark: {:?}", out.pixels[1]);
        // Without neighbours (the ends, or playing) the frame is untouched,
        // and a neighbour of another size is left out.
        assert_eq!(onion_skin(&now, None, None).pixels, now.pixels);
        let other = ColorImage::new([2, 2], Color32::BLACK);
        assert_eq!(onion_skin(&now, Some(&other), None).pixels, now.pixels);
    }
}
