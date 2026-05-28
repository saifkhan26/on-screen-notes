//! User-tunable application configuration.
//!
//! `AppConfig` holds preferences that survive across runs (default tool, pen
//! size, color, hotkey strings, etc.). It is serialised to RON and loaded at
//! startup by `crate::persistence`.

use serde::{Deserialize, Serialize};

use crate::canvas::stroke::StrokeStyle;

/// Pen-input smoothing preset.
///
/// Three buckets trade off latency vs. line smoothness. The active level
/// drives both the One Euro filter's parameters (input side) and how many
/// moving-average passes are applied to the subdivided polyline (render
/// side). Persisted so the user's preferred level survives restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmoothingLevel {
    /// Almost-passthrough — least latency, most jitter visible.
    Low,
    /// Balanced default — kills jitter without obvious lag.
    Medium,
    /// Heavy smoothing — looks immaculate but adds a slight visible lag.
    High,
}

impl SmoothingLevel {
    pub fn label(&self) -> &'static str {
        match self {
            SmoothingLevel::Low    => "Low",
            SmoothingLevel::Medium => "Medium",
            SmoothingLevel::High   => "High",
        }
    }
    /// Number of symmetric MA passes to run over the subdivided polyline
    /// positions at render time. More passes = smoother curve.
    pub fn position_passes(&self) -> usize {
        // Each pass is a causal EMA with α = 0.7 (≈ 3-sample time
        // constant). Multiple passes compound the effective tail
        // length. Causal smoothing means new pen samples never alter
        // past positions, so there is no mid-stroke ringing wobble.
        match self {
            // Zero passes = raw control points fed straight into the
            // quad-Bezier subdivision. No smoothing.
            SmoothingLevel::Low    => 0,
            // Moderate damping — ~6-sample tail (~30 ms at 200 Hz).
            SmoothingLevel::Medium => 2,
            // Heavy damping — ~12-sample tail (~60 ms at 200 Hz).
            SmoothingLevel::High   => 4,
        }
    }
}

fn default_smoothing_level() -> SmoothingLevel { SmoothingLevel::Medium }

/// Preferred container for screen recordings.
///
/// `Mp4IfAvailable` falls back to GIF when `ffmpeg` is not on PATH —
/// MP4 needs an external encoder we shell out to; bundling libx264
/// would dwarf the binary. Default `GifOnly` keeps zero-dep
/// recording working out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RecordingFormat {
    #[default]
    GifOnly,
    Mp4IfAvailable,
}

/// Persistent user preferences. Changing fields here is a breaking change to
/// the on-disk format; if we add fields later, give them `#[serde(default)]`
/// so old config files still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Default base pen width in logical pixels. Pressure scales the actual
    /// rendered width between 0 and this value.
    pub default_pen_size: f32,

    /// RGBA color used by the pen tool, in 0..=255 range per channel.
    pub default_pen_color: [u8; 4],

    /// Number of canvases created on first launch.
    pub initial_canvas_count: usize,

    /// Hotkey to toggle the main overlay visibility (parsed by global-hotkey).
    pub hotkey_toggle_overlay: String,

    /// Hotkey to take a screenshot composited with annotations.
    pub hotkey_screenshot: String,

    /// Where to save screenshots. If `None`, falls back to the user's
    /// pictures dir + `on-screen-notes/`.
    pub screenshot_dir: Option<std::path::PathBuf>,

    /// Background opacity for the overlay window, 0.0 (fully transparent —
    /// see what's behind) to 1.0 (fully opaque dark card). Persisted so the
    /// user's preferred dimming survives restarts. `#[serde(default)]` keeps
    /// old config files loading.
    #[serde(default = "default_bg_opacity")]
    pub bg_opacity: f32,

    /// Background tint RGB (0..=255 per channel) used when `bg_opacity > 0`.
    /// Defaults to near-black so old behaviour is preserved.
    #[serde(default = "default_bg_color")]
    pub bg_color: [u8; 3],

    /// Pinned colour palette shown in the toolbar. Editable at runtime via
    /// the colour picker (right-click a swatch). Persisted so the user's
    /// custom palette survives restarts. `#[serde(default)]` falls back to
    /// the built-in palette for older config files.
    #[serde(default = "default_palette")]
    pub palette: Vec<[u8; 4]>,

    /// Active pen-input smoothing preset.
    #[serde(default = "default_smoothing_level")]
    pub smoothing_level: SmoothingLevel,

    /// Last-used stroke style for the Pen / FreehandArrow tools. Persisted
    /// so a user who prefers the pencil look does not have to re-pick it
    /// every launch.
    #[serde(default)]
    pub default_stroke_style: StrokeStyle,

    /// Pressure-curve exponent applied to raw pen pressure before width
    /// is computed (`width ∝ raw.powf(curve)`). 1.0 = linear; <1.0 =
    /// lighter touch (amplify low pressure); >1.0 = heavier touch.
    /// Persisted so the user's calibration survives restarts.
    #[serde(default = "default_pressure_curve")]
    pub pressure_curve: f32,

    /// Preferred output container for recordings. `Mp4IfAvailable`
    /// auto-falls-back to GIF when `ffmpeg` is missing.
    #[serde(default)]
    pub recording_format: RecordingFormat,
}

fn default_pressure_curve() -> f32 { 1.0 }

fn default_bg_opacity() -> f32 { 0.0 }
fn default_bg_color() -> [u8; 3] { [0, 0, 0] }

pub fn default_palette() -> Vec<[u8; 4]> {
    vec![
        [232, 71,  71,  255], // red
        [255, 158, 35,  255], // orange
        [240, 215, 55,  255], // yellow
        [83,  201, 91,  255], // green
        [40,  165, 235, 255], // sky blue
        [120, 100, 230, 255], // violet
        [240, 240, 240, 255], // white
        [30,  30,  35,  255], // near-black
    ]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            // 6 px is a comfortable mid-width for a typical pen tip.
            default_pen_size: 6.0,
            // Opaque red — easy to see over arbitrary backgrounds.
            default_pen_color: [220, 30, 30, 255],
            // Three blank canvases gives the user something to switch into
            // immediately when they press the right-arrow key.
            initial_canvas_count: 3,
            // Reasonable defaults; user can edit on disk.
            hotkey_toggle_overlay: "ctrl+shift+KeyN".into(),
            hotkey_screenshot:     "ctrl+shift+KeyS".into(),
            screenshot_dir: None,
            bg_opacity: 0.0,
            bg_color: [0, 0, 0],
            palette: default_palette(),
            smoothing_level: SmoothingLevel::Medium,
            default_stroke_style: StrokeStyle::Default,
            pressure_curve: 1.0,
            recording_format: RecordingFormat::default(),
        }
    }
}
