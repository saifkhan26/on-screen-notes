//! Pen-sample smoothing filter.
//!
//! The raw pen samples we get from Wintab carry digitizer noise — a few
//! tenths of a pixel of jitter even when the pen is held still. At normal
//! drawing speed the user never sees it because successive samples move
//! several pixels and the noise is dwarfed. At **low pen speed** the noise
//! becomes the dominant motion in the stroke, and our Catmull-Rom
//! subdivision amplifies it into visible wobble — the "jagged edge" the
//! user reports.
//!
//! ## Algorithm — One Euro Filter (Casiez, Roussel, Vogel — CHI 2012)
//!
//! The 1€ filter is a simple exponential low-pass whose cutoff frequency
//! adapts to the signal's own derivative. Slow motion → low cutoff →
//! aggressive smoothing → jitter killed. Fast motion → high cutoff →
//! almost a passthrough → zero perceived lag. This is exactly the
//! trade-off we want: a still pen sees a stable position, a flicking pen
//! sees the raw fast motion.
//!
//! Reference: <https://gery.casiez.net/1euro/>
//!
//! It is the same filter used inside tldraw / Figma's pointer, Krita's
//! "Stabilizer", MyPaint's brush smoothing, and many AR/VR hand-tracking
//! pipelines.
//!
//! ## Why this and not "streamline" (Perfect Freehand)
//!
//! Streamline is an EMA between the previous *output* and the new input;
//! it's simpler but introduces a fixed lag proportional to the smoothing
//! weight. We promised the user Sticky-Notes-class latency, so we cannot
//! afford a constant lag — the speed-adaptive cutoff of 1€ gives us
//! smoothing only where we need it.

use std::collections::VecDeque;

use crate::canvas::stroke::PenSample;
use crate::config::SmoothingLevel;

/// **Wobble smoother** — Google ink-stroke-modeler approach.
///
/// The fundamental observation: digitiser jitter is visible only when the
/// pen is moving slowly. At any normal writing speed, sample-to-sample
/// motion dwarfs the noise. So instead of low-passing every sample (which
/// adds lag), we *velocity-blend* between a short moving-average and the
/// raw input: at rest → average wins (jitter killed); at speed →
/// passthrough wins (zero perceptible lag).
///
/// Reference: <https://github.com/google/ink-stroke-modeler> —
/// `WobbleSmootherParams` in `stroke_model_params.h`.
struct WobbleSmoother {
    /// (raw_pos, timestamp_seconds). Bounded by `timeout` so the window
    /// shrinks when motion is fast (newer samples come in faster than
    /// old ones expire) and grows when the pen rests.
    samples: VecDeque<([f32; 2], f32)>,
    /// Drop samples older than this many seconds. Equivalent to Google's
    /// `WobbleSmootherParams::timeout`. 40 ms covers ~8 packets at the
    /// typical 200 Hz Wintab rate — enough to average jitter at rest
    /// without ever influencing fast motion.
    timeout: f32,
    /// Below this speed (px/s), output is fully smoothed.
    speed_floor: f32,
    /// Above this speed, output is fully raw.
    speed_ceiling: f32,
}

impl WobbleSmoother {
    fn new(timeout: f32, speed_floor: f32, speed_ceiling: f32) -> Self {
        Self {
            samples: VecDeque::with_capacity(16),
            timeout,
            speed_floor,
            speed_ceiling,
        }
    }
    fn reset(&mut self) { self.samples.clear(); }

    /// Process one raw sample at time `t` (seconds). Returns smoothed
    /// position. First sample passes through unchanged.
    fn process(&mut self, raw: [f32; 2], t: f32) -> [f32; 2] {
        // Drop expired samples from the front so the window only covers
        // a short trailing time slice. At fast motion the queue stays
        // short → smoothing barely fires.
        while let Some(&(_, ts)) = self.samples.front() {
            if t - ts > self.timeout { self.samples.pop_front(); } else { break; }
        }
        self.samples.push_back((raw, t));
        let n = self.samples.len();
        if n < 2 { return raw; }

        // Instantaneous speed from the most recent two samples in the
        // window. Using the last two is what makes the blend respond
        // *immediately* to a velocity change — averaging over the whole
        // window would re-introduce lag at direction reversals.
        let (last_p, last_t) = self.samples[n - 1];
        let (prev_p, prev_t) = self.samples[n - 2];
        let dt = (last_t - prev_t).max(1.0e-4);
        let dx = last_p[0] - prev_p[0];
        let dy = last_p[1] - prev_p[1];
        let speed = (dx * dx + dy * dy).sqrt() / dt;

        // Window mean — the "smooth" candidate position.
        let mut sx = 0.0_f32;
        let mut sy = 0.0_f32;
        for &(p, _) in &self.samples {
            sx += p[0];
            sy += p[1];
        }
        let mean = [sx / n as f32, sy / n as f32];

        // Blend factor: 0 at floor → fully smoothed, 1 at ceiling →
        // fully raw. Clamped both ends so out-of-range values can't
        // flip the sign.
        let span = (self.speed_ceiling - self.speed_floor).max(1.0e-4);
        let b = ((speed - self.speed_floor) / span).clamp(0.0, 1.0);

        [
            mean[0] + (raw[0] - mean[0]) * b,
            mean[1] + (raw[1] - mean[1]) * b,
        ]
    }
}

/// One scalar 1€ filter — used independently on x, y, and pressure.
///
/// The math is short but easy to misread, so each step is annotated.
#[derive(Debug, Clone)]
struct OneEuro {
    /// Minimum cutoff frequency in Hz when motion is zero. Lower = smoother
    /// at rest, but more lag at very slow motion.
    min_cutoff: f32,
    /// How aggressively the cutoff opens up with derivative magnitude.
    /// Higher = less smoothing during fast motion (= less lag).
    beta:       f32,
    /// Cutoff for the internal derivative filter. 1 Hz is the canonical
    /// recommendation from the paper.
    d_cutoff:   f32,

    /// Previous low-pass output. `None` before the first sample.
    prev_x:  Option<f32>,
    /// Previous low-pass derivative.
    prev_dx: Option<f32>,
    /// Timestamp of the previous sample (seconds since some t0).
    prev_t:  Option<f32>,
}

impl OneEuro {
    fn new(min_cutoff: f32, beta: f32, d_cutoff: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            d_cutoff,
            prev_x:  None,
            prev_dx: None,
            prev_t:  None,
        }
    }

    /// Reset all internal state — call on pen-down so a new stroke does not
    /// inherit the previous stroke's filter state.
    fn reset(&mut self) {
        self.prev_x  = None;
        self.prev_dx = None;
        self.prev_t  = None;
    }

    /// Smoothing factor for a low-pass with cutoff `cutoff` (Hz) sampled at
    /// period `te` (seconds). Derived from the standard `RC = 1/(2π·fc)`
    /// formula.
    #[inline]
    fn alpha(cutoff: f32, te: f32) -> f32 {
        let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        1.0 / (1.0 + tau / te)
    }

    /// Filter one sample at time `t`. Returns the smoothed value.
    fn filter(&mut self, x: f32, t: f32) -> f32 {
        // First sample: nothing to smooth against; pass through and seed.
        let (prev_x, prev_t) = match (self.prev_x, self.prev_t) {
            (Some(px), Some(pt)) => (px, pt),
            _ => {
                self.prev_x  = Some(x);
                self.prev_dx = Some(0.0);
                self.prev_t  = Some(t);
                return x;
            }
        };

        // Guard against duplicate-timestamp samples (some drivers will emit
        // two packets in the same frame). A zero `te` would divide-by-zero
        // in `alpha`. Fall back to "no time has passed → no update".
        let te = (t - prev_t).max(1.0e-4);

        // 1) Low-pass the raw derivative with `d_cutoff` so its own noise
        //    doesn't drive the cutoff for the position filter.
        let dx_raw = (x - prev_x) / te;
        let a_d    = Self::alpha(self.d_cutoff, te);
        let dx     = a_d * dx_raw + (1.0 - a_d) * self.prev_dx.unwrap_or(0.0);

        // 2) Adaptive cutoff: bigger derivative → higher cutoff → less smoothing.
        let cutoff = self.min_cutoff + self.beta * dx.abs();

        // 3) Low-pass the position with the adapted cutoff.
        let a  = Self::alpha(cutoff, te);
        let xf = a * x + (1.0 - a) * prev_x;

        self.prev_x  = Some(xf);
        self.prev_dx = Some(dx);
        self.prev_t  = Some(t);
        xf
    }
}

/// Per-pen-stroke smoothing state. One filter on each of x, y, pressure.
///
/// Parameters are tuned for a 1080p screen and a 200 Hz tablet sample rate
/// (the common case on Wacom Intuos / Huion H-series). They were chosen
/// empirically:
///
///   * `min_cutoff = 1.0 Hz`  — heavy smoothing of a still pen.
///   * `beta       = 0.05`     — opens up quickly with motion; at typical
///                              writing speeds (~500 px/s) the effective
///                              cutoff is well above 25 Hz so the filter
///                              behaves almost like a passthrough.
///   * `d_cutoff   = 1.0 Hz`  — canonical Casiez value.
pub struct PenFilter {
    fx: OneEuro,
    fy: OneEuro,
    fp: OneEuro,
    /// Velocity-blended low-speed wobble killer. Active on every
    /// smoothing level — invisible at writing speed, eliminates the
    /// low-speed kinks that pure passthrough leaves behind.
    wobble: WobbleSmoother,
    /// Active smoothing preset — drives the One Euro parameters above
    /// plus the min-distance gate. Re-applied whenever the user
    /// changes the level from the toolbar.
    level: SmoothingLevel,
    /// Min-distance gate in pixels — wider at higher smoothing levels
    /// so a still-but-jittering pen contributes no samples at all.
    min_dist_px: f32,
    /// Window-client coordinate of the last *accepted* sample. Used by the
    /// min-distance gate.
    last_accepted_pos: Option<[f32; 2]>,
    /// Wall-clock t0 used as the time origin for the filter. We don't need
    /// absolute timestamps — only deltas — so an `Instant` captured at
    /// construction time is enough.
    t0: std::time::Instant,
}

impl Default for PenFilter {
    fn default() -> Self { Self::for_level(SmoothingLevel::Medium) }
}

/// Per-level One Euro tunings — (pos_min_cutoff, pos_beta, min_dist_px).
/// Pressure filter parameters are level-independent: pressure must always
/// track the user's intent quickly, so we don't compromise that.
fn pos_params(level: SmoothingLevel) -> (f32, f32, f32) {
    match level {
        // Light touch — high cutoff, big beta. Pen samples reach the
        // canvas with almost no delay; some jitter is the price.
        SmoothingLevel::Low    => (2.0, 0.15, 0.5),
        // Default — the tuning we landed on after iterative testing.
        SmoothingLevel::Medium => (0.5, 0.07, 1.25),
        // Heavy — very low cutoff, small beta, wide deduplication.
        // Adds a few ms of perceptible lag but lines look immaculate.
        SmoothingLevel::High   => (0.2, 0.03, 2.0),
    }
}

impl PenFilter {
    #[allow(dead_code)]
    pub fn new() -> Self { Self::for_level(SmoothingLevel::Medium) }

    /// Build a filter pre-configured for a given smoothing preset.
    pub fn for_level(level: SmoothingLevel) -> Self {
        let (mc, beta, min_dist) = pos_params(level);
        // Wobble-smoother parameters mirror Google ink-stroke-modeler's
        // defaults (1.31 cm/s floor, 1.44 cm/s ceiling, 40 ms window)
        // converted to logical pixels at ~96 DPI. The band is narrow on
        // purpose — anything resembling normal writing speed
        // (>~55 px/s) is fully raw, so the user perceives "no
        // stabilization" while sub-stroke noise at rest is killed.
        Self {
            fx: OneEuro::new(mc, beta, 1.0),
            fy: OneEuro::new(mc, beta, 1.0),
            // Pressure tracks user intent directly across all levels.
            fp: OneEuro::new(3.0, 0.2, 1.0),
            wobble: WobbleSmoother::new(0.04, 50.0, 55.0),
            level,
            min_dist_px: min_dist,
            last_accepted_pos: None,
            t0: std::time::Instant::now(),
        }
    }

    /// Apply a new smoothing level. Re-creates the internal filters with
    /// the matching parameters and clears any in-flight state so the
    /// transition does not produce a stale jump.
    pub fn set_level(&mut self, level: SmoothingLevel) {
        if level == self.level { return; }
        *self = Self::for_level(level);
    }

    /// Reset state for a new stroke. Call on pen-down.
    pub fn reset(&mut self) {
        self.fx.reset();
        self.fy.reset();
        self.fp.reset();
        self.wobble.reset();
        self.last_accepted_pos = None;
        self.t0 = std::time::Instant::now();
    }

    /// Currently active level — used by the toolbar to render the active
    /// chip in the smoothing selector.
    #[allow(dead_code)]
    pub fn level(&self) -> SmoothingLevel { self.level }

    /// Smooth `sample`. Returns `None` if the sample is within
    /// `min_dist_px` pixels of the previously accepted one (drops
    /// near-duplicate samples that just bloat the stroke without adding
    /// information). `force_accept` skips the gate — used for the first /
    /// last sample of a stroke so pen-down/up always land exactly.
    pub fn filter(&mut self, sample: PenSample, force_accept: bool) -> Option<PenSample> {
        let t = self.t0.elapsed().as_secs_f32();

        // Stage 1 — wobble smoothing. Runs on every level. At drawing
        // speed it is a passthrough (output == raw); only when the pen
        // is moving below ~55 px/s does it blend in the short window
        // average. This is what kills the low-speed kinks visible at
        // Low level without adding any perceptible lag to fast motion.
        let wobble_pos = self.wobble.process(sample.pos, t);

        if self.level == SmoothingLevel::Low {
            // Low = wobble-only. No One Euro, narrow dedup gate.
            if !force_accept {
                if let Some(prev) = self.last_accepted_pos {
                    let dx = wobble_pos[0] - prev[0];
                    let dy = wobble_pos[1] - prev[1];
                    if dx * dx + dy * dy < 0.04 {
                        return None;
                    }
                }
            }
            self.last_accepted_pos = Some(wobble_pos);
            return Some(PenSample {
                pos: wobble_pos,
                pressure: sample.pressure,
                tilt: sample.tilt,
            });
        }

        if !force_accept {
            if let Some(prev) = self.last_accepted_pos {
                let dx = wobble_pos[0] - prev[0];
                let dy = wobble_pos[1] - prev[1];
                if dx * dx + dy * dy < self.min_dist_px * self.min_dist_px {
                    return None;
                }
            }
        }

        // Stage 2 — One Euro on the wobble-cleaned signal. Adds speed-
        // adaptive low-pass for the user-visible "Medium" / "High"
        // settings.
        let fx = self.fx.filter(wobble_pos[0], t);
        let fy = self.fy.filter(wobble_pos[1], t);
        let fp = self.fp.filter(sample.pressure, t).clamp(0.0, 1.0);
        self.last_accepted_pos = Some([fx, fy]);
        Some(PenSample {
            pos: [fx, fy],
            pressure: fp,
            tilt: sample.tilt,
        })
    }
}