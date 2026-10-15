//! Geometric shapes the user can place on the canvas.
//!
//! Unlike `Stroke` (a list of arbitrary samples), a `Shape` is a *closed*
//! geometric primitive whose appearance is computed from a few control
//! points. This makes shapes cheap to store and easy to manipulate.

use crate::canvas::affine::Affine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// How the outline of a Rect or Ellipse is rendered.
///
/// `DashedAnimated` produces a marching-ants stroke: the dash phase
/// advances every frame from a wall-clock reading, which makes the
/// dashes appear to crawl along the outline. Useful for highlighting
/// "this is the area I'm talking about" during live demos. The
/// pattern is stateless (no per-shape phase stored), so the same
/// outline picked up from disk picks up animation the moment it is
/// rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BorderStyle {
    /// Regular continuous outline (existing look).
    #[default]
    Solid,
    /// Marching-ants animated dashes.
    DashedAnimated,
}

/// What a `Shape::Raster` holds. The eraser treats the two differently:
/// a fill is ink the user laid down and expects to rub out, an image is
/// a picture they are annotating over and expect to survive erasing the
/// ink on top of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RasterKind {
    /// Flood-fill sticker (every raster saved before this field existed).
    #[default]
    Fill,
    /// Captured or pasted picture.
    Image,
}

fn is_zero(v: &f32) -> bool { *v == 0.0 }
fn is_fill(k: &RasterKind) -> bool { *k == RasterKind::Fill }

/// What a media picture plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    /// Animated GIF, decoded in-process.
    Gif,
    /// Video file, decoded by an `ffmpeg` child process.
    Video,
}

/// Stream facts measured at import, so the control bar can show the
/// length and frame count before a player has started.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MediaMeta {
    /// Frame rate as `num / den`. `[0, 1]` for GIFs, whose frames each
    /// carry their own delay.
    pub frame_rate: [u32; 2],
    /// Number of frames (an estimate for videos that do not record it).
    pub frames: u32,
    /// Length in seconds.
    pub duration: f64,
    /// Display size in pixels, after any rotation the file asks for.
    pub size: [u32; 2],
}

/// Hands out `MediaRef::id`s. Ids are runtime-only, so a counter that
/// restarts with the app is all they need.
static NEXT_MEDIA_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A new, never-used `MediaRef::id`.
pub fn fresh_media_id() -> u64 {
    NEXT_MEDIA_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The video or animated GIF behind a media picture. The file itself is
/// a copy kept in the app's `media/` folder; the slide stores only its
/// name, so slide files stay small.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    /// File name inside `persistence::paths::media_dir()`, named after a
    /// hash of the contents (`"<16 hex digits>.<ext>"`).
    pub file: String,
    pub kind: MediaKind,
    /// Name of the file the user inserted, for messages.
    #[serde(default)]
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MediaMeta>,
    /// Key of this object's player while the app runs. Not saved: every
    /// load mints fresh ids. Clones (undo snapshots, the transform
    /// preview) keep it, so they show the same playback; a duplicate
    /// gets a new one in `insert_and_select` and plays on its own.
    #[serde(skip, default = "fresh_media_id")]
    pub id: u64,
}

/// Oriented bounding box: a rectangle of half-extents `half` centred on
/// `center` and turned by `angle` radians (clockwise on screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Obb {
    pub center: [f32; 2],
    pub half: [f32; 2],
    pub angle: f32,
}

impl Obb {
    /// Canvas point → this box's own frame (origin at the centre, axes
    /// along its sides).
    pub fn to_local(&self, p: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.angle.sin_cos();
        let d = [p[0] - self.center[0], p[1] - self.center[1]];
        [c * d[0] + s * d[1], -s * d[0] + c * d[1]]
    }

    /// Inverse of `to_local`.
    pub fn from_local(&self, q: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.angle.sin_cos();
        [
            self.center[0] + c * q[0] - s * q[1],
            self.center[1] + s * q[0] + c * q[1],
        ]
    }

    /// Corners in canvas coords: top-left, top-right, bottom-right,
    /// bottom-left of the unrotated box.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        let [hw, hh] = self.half;
        [
            self.from_local([-hw, -hh]),
            self.from_local([hw, -hh]),
            self.from_local([hw, hh]),
            self.from_local([-hw, hh]),
        ]
    }

    /// Axis-aligned box `[x0, y0, x1, y1]` enclosing this one.
    pub fn aabb(&self) -> [f32; 4] {
        let cs = self.corners();
        let mut bb = [cs[0][0], cs[0][1], cs[0][0], cs[0][1]];
        for p in &cs[1..] {
            bb[0] = bb[0].min(p[0]);
            bb[1] = bb[1].min(p[1]);
            bb[2] = bb[2].max(p[0]);
            bb[3] = bb[3].max(p[1]);
        }
        bb
    }
}

/// Coarse `[width, height]` of a text label. The real layout only
/// happens at paint time, so this is an estimate: each line is taken as
/// `0.6 × font_size` per character and `1.25 × font_size` tall. Good
/// enough for hit-testing and framing.
pub fn text_extent(content: &str, font_size: f32) -> [f32; 2] {
    let mut lines = 0usize;
    let mut max_chars = 0usize;
    for l in content.split('\n') {
        lines += 1;
        max_chars = max_chars.max(l.chars().count());
    }
    [
        (font_size * 0.6 * max_chars.max(1) as f32).max(font_size),
        (font_size * 1.25 * lines.max(1) as f32).max(font_size),
    ]
}

/// All shape variants live in one enum so a Canvas can store them in a single
/// `Vec<Shape>` without trait objects (which complicate serde).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Shape {
    /// Axis-aligned rectangle defined by two opposite corners.
    Rect {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        /// Outline style. `#[serde(default)]` keeps RON files written
        /// before this field existed loading as plain `Solid`.
        #[serde(default)]
        border: BorderStyle,
        /// Turn about the shape's centre, radians, clockwise on screen.
        /// `#[serde(default)]` loads every shape saved before rotation
        /// existed as unrotated; a zero is not written back, so an
        /// untouched shape keeps its old on-disk form.
        #[serde(default, skip_serializing_if = "is_zero")]
        rotation: f32,
    },
    /// Ellipse defined by its bounding rectangle (corners `a` and `b`).
    Ellipse {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        /// Outline style. `#[serde(default)]` keeps old files loading
        /// as solid-outlined ellipses.
        #[serde(default)]
        border: BorderStyle,
        /// Same meaning and save behaviour as `Rect::rotation`.
        #[serde(default, skip_serializing_if = "is_zero")]
        rotation: f32,
    },
    /// Straight line from `a` to `b`.
    Line {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        /// Outline style. `#[serde(default)]` keeps RON files written
        /// before this field existed loading as plain `Solid`. The
        /// only producer today is the Line shape tool; ruler-mode
        /// snap still commits a `Stroke` so its line is unaffected.
        #[serde(default)]
        border: BorderStyle,
    },
    /// Arrow from `a` to `b`. Arrowhead is drawn at `b`. Only the
    /// shaft honours `border`; the head stays solid so the
    /// direction reads cleanly even when the shaft is dashed.
    Arrow {
        a: [f32; 2],
        b: [f32; 2],
        color: [u8; 4],
        stroke_width: f32,
        #[serde(default)]
        border: BorderStyle,
    },
    /// Free-floating text label anchored at `pos` (top-left of the
    /// first line's baseline box). Lines are split on `\n`. Rendered
    /// in egui's default proportional font at `font_size` (canvas-local
    /// logical pixels). Screenshots of canvases containing Text shapes
    /// do not yet render the text glyphs (tiny-skia has no font
    /// rasteriser); the on-screen render is the source of truth until
    /// we bundle a font + fontdue.
    Text {
        pos: [f32; 2],
        content: String,
        font_size: f32,
        color: [u8; 4],
        /// Turn about `pos` (not the centre — the label's real size is
        /// only known at paint time, and `pos` is the pivot egui's text
        /// rotation uses). Radians, clockwise on screen.
        #[serde(default, skip_serializing_if = "is_zero")]
        rotation: f32,
    },
    /// Raster sticker — a PNG-backed image dropped at `pos` with
    /// canvas-local `size`. Produced by the flood-fill tool (which
    /// rasterises the click region), by the lasso screen capture, and
    /// by pasting an image.
    Raster {
        /// Top-left canvas-local position.
        pos: [f32; 2],
        /// Logical canvas-local width / height.
        size: [f32; 2],
        /// PNG-encoded bytes. Transparent outside the filled region.
        /// Shared, so cloning a shape (transform preview, undo
        /// snapshots, duplicate) never copies the image. Serialises
        /// exactly as the plain byte list it replaced.
        png: Arc<Vec<u8>>,
        /// Physical-pixel dimensions of the PNG image.
        px_size: [u32; 2],
        /// Cached colour for tinting helpers / hit-testing. The
        /// PNG itself already carries pixel colour; this is only
        /// the "primary" fill colour so the eraser / hit-test can
        /// reason about it without decoding.
        color: [u8; 4],
        /// Turn about the image's centre; see `Rect::rotation`.
        #[serde(default, skip_serializing_if = "is_zero")]
        rotation: f32,
        /// Fill sticker or picture. Defaults to `Fill`, which is what
        /// every raster saved before this field existed is.
        #[serde(default, skip_serializing_if = "is_fill")]
        kind: RasterKind,
        /// Set when the picture plays a video or animated GIF. `png` is
        /// then a small poster frame: thumbnails, flood fill and
        /// screenshots without a live frame draw it, and app builds
        /// from before this field existed load the slide and show it
        /// as a still image. `kind` is always `Image`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media: Option<Box<MediaRef>>,
    },
}

impl Shape {
    /// Color accessor — useful for "highlight selected shape" rendering.
    #[allow(dead_code)]
    pub fn color(&self) -> [u8; 4] {
        match self {
            Shape::Rect    { color, .. } => *color,
            Shape::Ellipse { color, .. } => *color,
            Shape::Line    { color, .. } => *color,
            Shape::Arrow   { color, .. } => *color,
            Shape::Text    { color, .. } => *color,
            Shape::Raster  { color, .. } => *color,
        }
    }

    /// The media this shape plays, if it is a video or GIF picture.
    pub fn media(&self) -> Option<&MediaRef> {
        match self {
            Shape::Raster { media: Some(m), .. } => Some(m),
            _ => None,
        }
    }

    /// Shift this shape by `d` canvas-local pixels. Control points move;
    /// sizes, colours and stroke widths do not.
    pub fn translate(&mut self, d: [f32; 2]) {
        let shift = |p: &mut [f32; 2]| {
            p[0] += d[0];
            p[1] += d[1];
        };
        match self {
            Shape::Rect    { a, b, .. }
            | Shape::Ellipse { a, b, .. }
            | Shape::Line    { a, b, .. }
            | Shape::Arrow   { a, b, .. } => {
                shift(a);
                shift(b);
            }
            Shape::Text   { pos, .. } => shift(pos),
            Shape::Raster { pos, .. } => shift(pos),
        }
    }

    /// Apply the similarity transform `p → a * p + b` in canvas-local
    /// pixels. Unlike `translate`, everything with a length scales:
    /// stroke widths, font sizes, and the raster sticker's canvas-local
    /// extent — so a scaled shape keeps its proportions instead of
    /// growing a relatively heavier outline as it shrinks.
    ///
    /// `Raster::px_size` is left alone: it describes the PNG payload,
    /// not the canvas footprint, and the renderer stretches the decoded
    /// bytes into `size`.
    pub fn transform(&mut self, a: f32, b: [f32; 2]) {
        let map = |p: &mut [f32; 2]| {
            p[0] = a * p[0] + b[0];
            p[1] = a * p[1] + b[1];
        };
        match self {
            Shape::Rect    { a: p, b: q, stroke_width, .. }
            | Shape::Ellipse { a: p, b: q, stroke_width, .. }
            | Shape::Line    { a: p, b: q, stroke_width, .. }
            | Shape::Arrow   { a: p, b: q, stroke_width, .. } => {
                map(p);
                map(q);
                *stroke_width *= a;
            }
            Shape::Text { pos, font_size, .. } => {
                map(pos);
                *font_size *= a;
            }
            Shape::Raster { pos, size, .. } => {
                map(pos);
                size[0] *= a;
                size[1] *= a;
            }
        }
    }

    /// Recolour and / or re-width the shape from the toolbar. `width` is
    /// the pen-size slider value: outline width for the drawn shapes,
    /// and — through the same `× 3` rule the Text tool uses when a label
    /// is first placed — the font size for text. Rasters carry their
    /// colours in the PNG and are left alone.
    pub fn restyle(&mut self, color: Option<[u8; 4]>, width: Option<f32>) {
        match self {
            Shape::Rect    { color: c, stroke_width, .. }
            | Shape::Ellipse { color: c, stroke_width, .. }
            | Shape::Line    { color: c, stroke_width, .. }
            | Shape::Arrow   { color: c, stroke_width, .. } => {
                if let Some(new) = color { *c = new; }
                if let Some(w) = width { *stroke_width = w; }
            }
            Shape::Text { color: c, font_size, .. } => {
                if let Some(new) = color { *c = new; }
                if let Some(w) = width { *font_size = (w.max(6.0) * 3.0).clamp(12.0, 240.0); }
            }
            Shape::Raster { .. } => {}
        }
    }

    /// The shape's own oriented box, without stroke padding. Lines and
    /// arrows have no orientation of their own, so theirs is the
    /// axis-aligned box around the two end points.
    pub fn obb(&self) -> Obb {
        match self {
            Shape::Rect { a, b, rotation, .. } | Shape::Ellipse { a, b, rotation, .. } => Obb {
                center: [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
                half: [(b[0] - a[0]).abs() * 0.5, (b[1] - a[1]).abs() * 0.5],
                angle: *rotation,
            },
            Shape::Line { a, b, .. } | Shape::Arrow { a, b, .. } => Obb {
                center: [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
                half: [(b[0] - a[0]).abs() * 0.5, (b[1] - a[1]).abs() * 0.5],
                angle: 0.0,
            },
            Shape::Raster { pos, size, rotation, .. } => Obb {
                center: [pos[0] + size[0] * 0.5, pos[1] + size[1] * 0.5],
                half: [size[0].abs() * 0.5, size[1].abs() * 0.5],
                angle: *rotation,
            },
            Shape::Text { pos, content, font_size, rotation, .. } => {
                let [w, h] = text_extent(content, *font_size);
                let (s, c) = rotation.sin_cos();
                // The label turns about `pos`, so its centre swings
                // around that corner.
                Obb {
                    center: [
                        pos[0] + c * w * 0.5 - s * h * 0.5,
                        pos[1] + s * w * 0.5 + c * h * 0.5,
                    ],
                    half: [w * 0.5, h * 0.5],
                    angle: *rotation,
                }
            }
        }
    }

    /// Apply an affine transform. Lines and arrows map their end points.
    /// The box-like shapes cannot hold a general affine, so they take
    /// the transform apart instead: the centre is mapped, the new
    /// rotation is where the box's own x-axis ends up, and each
    /// half-extent scales by how much its axis was stretched. That is
    /// exact for every transform the Select tool produces — a rotation,
    /// a uniform scale, or a stretch along the box's own sides.
    ///
    /// Stroke widths and font sizes scale by `Affine::linear_scale`.
    pub fn apply_affine(&mut self, m: &Affine) {
        let s = m.linear_scale();
        let len = |v: [f32; 2]| (v[0] * v[0] + v[1] * v[1]).sqrt();
        // Maps a box given as (centre, half-extents, rotation).
        let map_box = |center: [f32; 2], half: [f32; 2], rotation: f32| {
            let (sin, cos) = rotation.sin_cos();
            let u = m.apply_vec([cos, sin]);
            let v = m.apply_vec([-sin, cos]);
            (m.apply(center), [half[0] * len(u), half[1] * len(v)], u[1].atan2(u[0]))
        };
        match self {
            Shape::Line { a, b, stroke_width, .. } | Shape::Arrow { a, b, stroke_width, .. } => {
                *a = m.apply(*a);
                *b = m.apply(*b);
                *stroke_width *= s;
            }
            Shape::Rect { a, b, stroke_width, rotation, .. }
            | Shape::Ellipse { a, b, stroke_width, rotation, .. } => {
                let center = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
                let half = [(b[0] - a[0]).abs() * 0.5, (b[1] - a[1]).abs() * 0.5];
                let (c, h, r) = map_box(center, half, *rotation);
                *a = [c[0] - h[0], c[1] - h[1]];
                *b = [c[0] + h[0], c[1] + h[1]];
                *rotation = r;
                *stroke_width *= s;
            }
            Shape::Raster { pos, size, rotation, .. } => {
                let center = [pos[0] + size[0] * 0.5, pos[1] + size[1] * 0.5];
                let half = [size[0].abs() * 0.5, size[1].abs() * 0.5];
                let (c, h, r) = map_box(center, half, *rotation);
                *pos = [c[0] - h[0], c[1] - h[1]];
                *size = [h[0] * 2.0, h[1] * 2.0];
                *rotation = r;
            }
            Shape::Text { pos, font_size, rotation, .. } => {
                let (sin, cos) = rotation.sin_cos();
                let u = m.apply_vec([cos, sin]);
                let v = m.apply_vec([-sin, cos]);
                *pos = m.apply(*pos);
                *rotation = u[1].atan2(u[0]);
                // One font size covers both axes; the line-height axis
                // is the one the user reads the size from.
                *font_size *= len(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(a: [f32; 2], b: [f32; 2]) -> Shape {
        Shape::Rect { a, b, color: [0; 4], stroke_width: 2.0, border: BorderStyle::Solid, rotation: 0.0 }
    }

    #[test]
    fn shapes_saved_before_rotation_existed_still_load() {
        let old = "Rect(a:(1.0,2.0),b:(5.0,8.0),color:(1,2,3,255),stroke_width:3.0)";
        match ron::from_str::<Shape>(old).unwrap() {
            Shape::Rect { rotation, border, .. } => {
                assert_eq!(rotation, 0.0);
                assert_eq!(border, BorderStyle::Solid);
            }
            other => panic!("{other:?}"),
        }
        let old = "Text(pos:(1.0,2.0),content:\"hi\",font_size:18.0,color:(1,2,3,255))";
        assert!(matches!(ron::from_str::<Shape>(old).unwrap(), Shape::Text { rotation, .. } if rotation == 0.0));
        let old = "Raster(pos:(0.0,0.0),size:(2.0,2.0),png:[1,2,3],px_size:(2,2),color:(9,9,9,255))";
        match ron::from_str::<Shape>(old).unwrap() {
            Shape::Raster { png, rotation, kind, .. } => {
                assert_eq!(*png, vec![1u8, 2, 3]);
                assert_eq!(rotation, 0.0);
                assert_eq!(kind, RasterKind::Fill);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn untouched_shapes_serialise_without_the_new_fields() {
        let text = ron::to_string(&rect([0.0, 0.0], [4.0, 4.0])).unwrap();
        assert!(!text.contains("rotation"), "{text}");
        let raster = Shape::Raster {
            pos: [0.0, 0.0], size: [1.0, 1.0], png: Arc::new(vec![7]), px_size: [1, 1],
            color: [0; 4], rotation: 0.0, kind: RasterKind::Fill, media: None,
        };
        let text = ron::to_string(&raster).unwrap();
        assert!(!text.contains("rotation") && !text.contains("kind") && !text.contains("media"), "{text}");
    }

    #[test]
    fn rotation_and_kind_round_trip() {
        let raster = Shape::Raster {
            pos: [3.0, 4.0], size: [10.0, 6.0], png: Arc::new(vec![7, 8]), px_size: [2, 1],
            color: [0; 4], rotation: 0.5, kind: RasterKind::Image, media: None,
        };
        let back: Shape = ron::from_str(&ron::to_string(&raster).unwrap()).unwrap();
        assert!(matches!(back, Shape::Raster { rotation, kind: RasterKind::Image, .. } if (rotation - 0.5).abs() < 1e-6));
    }

    fn media_raster() -> Shape {
        Shape::Raster {
            pos: [3.0, 4.0], size: [10.0, 6.0], png: Arc::new(vec![7, 8]), px_size: [2, 1],
            color: [0; 4], rotation: 0.0, kind: RasterKind::Image,
            media: Some(Box::new(MediaRef {
                file: "0123456789abcdef.mp4".into(),
                kind: MediaKind::Video,
                source_name: "clip.mp4".into(),
                meta: Some(MediaMeta { frame_rate: [30000, 1001], frames: 300, duration: 10.01, size: [1920, 1080] }),
                id: fresh_media_id(),
            })),
        }
    }

    #[test]
    fn media_round_trips_without_its_runtime_id() {
        let shape = media_raster();
        let text = ron::to_string(&shape).unwrap();
        assert!(!text.contains("id:"), "{text}");
        let back: Shape = ron::from_str(&text).unwrap();
        let (a, b) = (shape.media().unwrap(), back.media().unwrap());
        assert_eq!((&a.file, a.kind, &a.source_name, a.meta), (&b.file, b.kind, &b.source_name, b.meta));
        // A load mints a fresh id rather than reusing a saved one.
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn builds_without_media_load_slides_that_have_it() {
        // The `Raster` variant as it was before `media` existed. ron
        // skips fields a type does not know, so an older build opens a
        // slide with a video on it and shows the poster frame.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        enum OldShape {
            Raster {
                pos: [f32; 2],
                size: [f32; 2],
                png: Vec<u8>,
                px_size: [u32; 2],
                color: [u8; 4],
                #[serde(default)]
                rotation: f32,
                #[serde(default)]
                kind: RasterKind,
            },
        }
        let text = ron::to_string(&media_raster()).unwrap();
        let old: OldShape = ron::from_str(&text).unwrap();
        let OldShape::Raster { png, kind, .. } = old;
        assert_eq!((png, kind), (vec![7, 8], RasterKind::Image));
    }

    #[test]
    fn rotating_a_rect_keeps_its_size_and_sets_the_angle() {
        let mut r = rect([0.0, 0.0], [10.0, 4.0]);
        r.apply_affine(&Affine::rotate_about([5.0, 2.0], 0.7));
        let o = r.obb();
        assert!((o.center[0] - 5.0).abs() < 1e-3 && (o.center[1] - 2.0).abs() < 1e-3);
        assert!((o.half[0] - 5.0).abs() < 1e-3 && (o.half[1] - 2.0).abs() < 1e-3);
        assert!((o.angle - 0.7).abs() < 1e-4);
    }

    #[test]
    fn translate_and_scale_leave_rotation_at_exactly_zero() {
        // Unrotated shapes must stay on the unrotated render path.
        let mut r = rect([0.0, 0.0], [10.0, 4.0]);
        r.apply_affine(&Affine::translate([3.0, -2.0]).then(Affine::scale_about([1.0, 1.0], 2.0, 0.5)));
        assert!(matches!(r, Shape::Rect { rotation, .. } if rotation == 0.0));
    }

    #[test]
    fn a_transform_followed_by_its_inverse_restores_the_shape() {
        let mut r = rect([2.0, 3.0], [12.0, 9.0]);
        let pivot = [40.0, -7.0];
        r.apply_affine(&Affine::rotate_about(pivot, 1.1));
        r.apply_affine(&Affine::scale_about(pivot, 2.0, 2.0));
        r.apply_affine(&Affine::scale_about(pivot, 0.5, 0.5));
        r.apply_affine(&Affine::rotate_about(pivot, -1.1));
        match r {
            Shape::Rect { a, b, stroke_width, rotation, .. } => {
                assert!((a[0] - 2.0).abs() < 1e-2 && (a[1] - 3.0).abs() < 1e-2, "{a:?}");
                assert!((b[0] - 12.0).abs() < 1e-2 && (b[1] - 9.0).abs() < 1e-2, "{b:?}");
                assert!((stroke_width - 2.0).abs() < 1e-3);
                assert!(rotation.abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stretching_a_rotated_box_along_its_own_side_keeps_it_a_box() {
        // Stretch ×3 along the box's local x-axis, as a side handle does.
        let mut r = rect([0.0, 0.0], [10.0, 4.0]);
        let angle = 0.6;
        r.apply_affine(&Affine::rotate_about([5.0, 2.0], angle));
        let stretch = Affine::rotate_about([5.0, 2.0], -angle)
            .then(Affine::scale_about([5.0, 2.0], 3.0, 1.0))
            .then(Affine::rotate_about([5.0, 2.0], angle));
        r.apply_affine(&stretch);
        let o = r.obb();
        assert!((o.half[0] - 15.0).abs() < 1e-2 && (o.half[1] - 2.0).abs() < 1e-2, "{o:?}");
        assert!((o.angle - angle).abs() < 1e-4);
    }

    #[test]
    fn text_turns_about_its_anchor() {
        let mut t = Shape::Text { pos: [10.0, 10.0], content: "ab".into(), font_size: 20.0, color: [0; 4], rotation: 0.0 };
        t.apply_affine(&Affine::rotate_about([10.0, 10.0], 0.3).then(Affine::scale_about([10.0, 10.0], 2.0, 2.0)));
        assert!(matches!(t, Shape::Text { pos, font_size, rotation, .. }
            if (pos[0] - 10.0).abs() < 1e-3 && (font_size - 40.0).abs() < 1e-2 && (rotation - 0.3).abs() < 1e-4));
    }
}
