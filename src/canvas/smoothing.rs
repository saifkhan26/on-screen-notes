//! Krita-style smoothing modes.
//!
//! Mirrors `KisSmoothingOptions` + the dispatch inside
//! `KisToolFreehandHelper::paint` from Krita. Five modes:
//!
//!   * `Adaptive` — our existing pipeline (PenFilter wobble + OEF +
//!     causal EMA on cache build). Kept as the default so nothing
//!     regresses for users who don't touch the new selector.
//!   * `None` — raw pass-through. Fastest, every digitiser tick lands
//!     unchanged on the canvas.
//!   * `Simple` — Krita's "Basic Smoothing": 2-tap moving average,
//!     old-tablet jitter only. No lag, almost free.
//!   * `Weighted` — Krita's "Weighted Smoothing": Gaussian-weighted
//!     history accumulation with tail-aggressiveness pressure penalty.
//!   * `Stabilizer` — Krita's "Stabilizer": rope-pull queue + delay-
//!     distance gate. Cursor leads, ink trails inside a radius.
//!
//! The state for Weighted and Stabilizer modes lives on the `Stroke`
//! itself (lazy-allocated in `push`) so a stroke that finishes carries
//! no overhead.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::canvas::stroke::PenSample;

/// Which smoothing algorithm runs while a stroke is being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmoothingType {
    /// Existing PenFilter (wobble + One Euro) plus causal EMA on cache
    /// build. Default — chosen so an unchanged config behaves exactly
    /// like before this feature landed.
    Adaptive,
    /// Krita NO_SMOOTHING. Raw samples straight to the polyline.
    None,
    /// Krita SIMPLE_SMOOTHING. 2-tap average of last two samples.
    Simple,
    /// Krita WEIGHTED_SMOOTHING. Gaussian-weighted accumulation over
    /// the recent sample history, with a tail-aggressiveness penalty
    /// on rising pressure that tightens the lift-off taper.
    Weighted,
    /// Krita STABILIZER. Sample queue averaged with a `k=(i-1)/i`
    /// running-mean, gated by a delay-distance radius so ink lags
    /// behind the cursor like a rope pull.
    Stabilizer,
}

impl Default for SmoothingType {
    fn default() -> Self { SmoothingType::Adaptive }
}

impl SmoothingType {
    pub fn label(&self) -> &'static str {
        match self {
            SmoothingType::Adaptive   => "Adaptive",
            SmoothingType::None       => "None",
            SmoothingType::Simple     => "Simple",
            SmoothingType::Weighted   => "Weighted",
            SmoothingType::Stabilizer => "Stabilizer",
        }
    }

    pub fn tooltip(&self) -> &'static str {
        match self {
            SmoothingType::Adaptive   => "Adaptive (default) — speed-aware One Euro + wobble killer",
            SmoothingType::None       => "None — raw samples, every digitiser tick",
            SmoothingType::Simple     => "Simple — Krita Basic, light 2-tap average",
            SmoothingType::Weighted   => "Weighted — Krita Weighted, Gaussian history blend",
            SmoothingType::Stabilizer => "Stabilizer — Krita Stabilizer, rope-pull queue with delay distance",
        }
    }
}

/// All knobs Krita exposes on its smoothing options, plus a render-time
/// fan-corner toggle. Defaults are tuned to feel like Krita's own
/// shipping defaults so users coming from Krita get something familiar.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SmoothingOptions {
    /// Which algorithm runs.
    pub kind: SmoothingType,

    /// Sample-buffer width for Stabilizer / Weighted. Bigger = more
    /// smoothing, more lag. Krita uses px; we follow.
    pub smoothness_distance: f32,

    /// Weighted tail-aggressiveness multiplier. Higher = pressure
    /// drop-off thins the stroke faster at lift. 0 disables.
    pub tail_aggressiveness: f32,

    /// Weighted pressure smoothing — also blends pressure across the
    /// history, not only position.
    pub smooth_pressure: bool,

    /// Stabilizer delay-distance radius. Sub-radius cursor motion does
    /// not commit ink — gives the "drag a rope" feel for clean lines.
    pub delay_distance: f32,

    /// Whether the delay-distance gate is active. Off = pure queue
    /// averaging, on = queue + radius gate (Krita's default).
    pub use_delay_distance: bool,

    /// Stabilizer drains its remaining queue on pen-up so the stroke's
    /// tail catches up to the cursor. Off = the trailing rope length
    /// stays uncovered (Krita behaviour: on by default).
    pub finish_stabilized_curve: bool,

    /// Stabilizer mixes every sensor (pressure, tilt, rotation) through
    /// the running-mean blender, not only position. On = creamy taper.
    pub stabilize_sensors: bool,

    /// Render-time toggle: inject wedge dabs at sharp turns so the
    /// outer side of a hard corner is not left as a missing wedge.
    pub fan_corners: bool,

    /// Render-time: angle change (radians) that triggers a fan-corner
    /// wedge fill. Lower = more sensitive.
    pub fan_corners_step: f32,
}

impl Default for SmoothingOptions {
    fn default() -> Self {
        Self {
            kind: SmoothingType::Adaptive,
            // Krita defaults: 20 px sample window is enough to kill
            // jitter without feeling sticky on a normal-pen-speed
            // scribble.
            smoothness_distance: 20.0,
            tail_aggressiveness: 0.15,
            smooth_pressure: false,
            delay_distance: 10.0,
            use_delay_distance: false,
            finish_stabilized_curve: true,
            stabilize_sensors: true,
            fan_corners: false,
            fan_corners_step: 0.20, // ~11° per wedge — Krita default
        }
    }
}

impl SmoothingOptions {
    /// Convenience: number of position-smoothing passes the cache
    /// builder should apply on commit.
    ///
    /// The user-facing "Stroke smoothing" Low/Med/High picker is a
    /// render-polish layer that runs over the subdivided polyline AT
    /// COMMIT TIME — it is independent of the per-sample Krita
    /// algorithm. So all modes honour the user's level; the previous
    /// "None mode forces 0 passes" override was producing a visible
    /// pen-up wiggle because the live preview path used the level but
    /// `build_cache_with` did not. Now they agree.
    ///
    /// Weighted/Stabilizer already heavily smoothed per-sample; we
    /// shave one pass off the budget so a "High" preset doesn't
    /// turn into "Very high" for those modes. Floor at 0.
    pub fn cache_passes(&self, level_passes: usize) -> usize {
        match self.kind {
            // Adaptive runs the heavy upstream PenFilter (One Euro +
            // wobble) on every sample before they even reach the
            // Stroke. The cache pass is only polish — `level_passes`
            // verbatim is correct.
            SmoothingType::Adaptive => level_passes,
            // None / Simple bypass PenFilter entirely. The polyline
            // EMA is the ONLY smoothing the line gets, so we triple
            // the budget — "High" then noticeably tames raw input
            // jitter, "Low" stays close to raw on purpose.
            SmoothingType::None | SmoothingType::Simple
                => level_passes.saturating_mul(3),
            // Weighted / Stabilizer already heavily smoothed per
            // sample. Shave a pass so "High" doesn't double-smooth.
            SmoothingType::Weighted | SmoothingType::Stabilizer
                => level_passes.saturating_sub(1),
        }
    }

    /// Does this mode want the upstream `PenFilter` (input/filter.rs)
    /// to run before `Stroke::push` sees the sample? Adaptive is the
    /// only mode that does — every Krita mode runs its own algorithm
    /// on raw input.
    pub fn use_upstream_pen_filter(&self) -> bool {
        matches!(self.kind, SmoothingType::Adaptive)
    }
}

// -- Per-stroke transient state ----------------------------------------------

/// Weighted-mode history. Krita walks the recent samples backward,
/// accumulating Gaussian-weighted contributions to produce the next
/// committed sample.
#[derive(Debug, Clone, Default)]
pub struct WeightedState {
    pub history: Vec<PenSample>,
    pub distance_history: Vec<f32>,
}

impl WeightedState {
    pub fn new() -> Self {
        Self { history: Vec::with_capacity(32), distance_history: Vec::with_capacity(32) }
    }
}

/// Stabilizer-mode deque. Filled with N copies of the first sample on
/// stroke begin so the running-mean has a stable seed; new samples
/// rotate through.
#[derive(Debug, Clone, Default)]
pub struct StabilizerState {
    pub deque: VecDeque<PenSample>,
    /// Last emitted (committed) sample. Used by the delay-distance
    /// gate to decide whether the cursor has moved far enough to drop
    /// the next painted point.
    pub last_emitted: Option<PenSample>,
}

impl StabilizerState {
    pub fn new(first: PenSample, sample_size: usize) -> Self {
        let mut deque = VecDeque::with_capacity(sample_size.max(3));
        for _ in 0..sample_size.max(3) {
            deque.push_back(first);
        }
        Self { deque, last_emitted: None }
    }
}

/// Union of per-stroke state for the explicit Krita modes. Adaptive
/// mode does not allocate any state here — it uses the existing
/// `OneEuroFilter` field on `Stroke`. Raw (`None` mode) skips the
/// `mode_state` field entirely because it has no per-mode buffer.
#[derive(Debug, Clone)]
pub enum ModeState {
    Simple { prev: Option<PenSample> },
    Weighted(WeightedState),
    Stabilizer(StabilizerState),
}

// -- Per-mode push helpers ---------------------------------------------------

/// Krita SIMPLE_SMOOTHING: 2-tap moving average of position. Old-tablet
/// jitter only — no perceptible lag at any pen speed.
pub fn simple_step(prev: &mut Option<PenSample>, raw: PenSample) -> PenSample {
    let out = match *prev {
        None => raw,
        Some(p) => PenSample {
            pos: [
                0.5 * (p.pos[0] + raw.pos[0]),
                0.5 * (p.pos[1] + raw.pos[1]),
            ],
            pressure: 0.5 * (p.pressure + raw.pressure),
            tilt: raw.tilt,
        },
    };
    *prev = Some(raw);
    out
}

/// Krita WEIGHTED_SMOOTHING: Gaussian-weighted accumulation walking the
/// history backward. Matches `kis_tool_freehand_helper.cpp`'s
/// `paint(KisPaintInformation)` weighted branch. `smoothness_distance`
/// drives sigma (3-sigma rule).
pub fn weighted_step(state: &mut WeightedState, raw: PenSample, opts: &SmoothingOptions) -> PenSample {
    if let Some(last) = state.history.last() {
        let dx = raw.pos[0] - last.pos[0];
        let dy = raw.pos[1] - last.pos[1];
        let d = (dx * dx + dy * dy).sqrt();
        state.distance_history.push(d);
    } else {
        state.distance_history.push(0.0);
    }
    state.history.push(raw);

    let n = state.history.len();
    if n < 2 {
        return raw;
    }

    let sigma = (opts.smoothness_distance / 3.0).max(1.0e-3);
    let inv_sqrt_2pi_sigma = 1.0 / ((2.0 * std::f32::consts::PI).sqrt() * sigma);
    let two_sigma2 = 2.0 * sigma * sigma;
    let tail_aggressiveness = 40.0 * opts.tail_aggressiveness;

    let mut distance_sum = 0.0_f32;
    let mut acc_x = 0.0_f32;
    let mut acc_y = 0.0_f32;
    let mut acc_p = 0.0_f32;
    let mut acc_w = 0.0_f32;

    for i in (0..n).rev() {
        let next = state.history[i];
        let mut d = state.distance_history[i] as f32;

        if i < n - 1 {
            let pressure_grad = next.pressure - state.history[i + 1].pressure;
            if pressure_grad > 0.0 {
                let bumped = pressure_grad
                    * tail_aggressiveness
                    * (1.0 - next.pressure);
                d += bumped * 3.0 * sigma;
            }
        }
        distance_sum += d;
        let rate = inv_sqrt_2pi_sigma * (-(distance_sum * distance_sum) / two_sigma2).exp();
        acc_x += rate * next.pos[0];
        acc_y += rate * next.pos[1];
        if opts.smooth_pressure {
            acc_p += rate * next.pressure;
        }
        acc_w += rate;

        // Early exit: tail contribution decays exponentially, so once
        // the cumulative distance is well past 3σ we are adding noise.
        if distance_sum > 5.0 * sigma {
            break;
        }
    }

    if acc_w <= 1.0e-6 {
        return raw;
    }
    let pos = [acc_x / acc_w, acc_y / acc_w];
    let pressure = if opts.smooth_pressure {
        (acc_p / acc_w).clamp(0.0, 1.0)
    } else {
        raw.pressure
    };
    PenSample { pos, pressure, tilt: raw.tilt }
}

/// Krita STABILIZER `getStabilizedPaintInfo`: blend the deque into the
/// new sample using the running-mean coefficient `k = (i-1)/i`.
fn stabilizer_blend(deque: &VecDeque<PenSample>, last: PenSample, opts: &SmoothingOptions) -> PenSample {
    let mut out = last;
    if deque.len() <= 1 {
        return out;
    }
    let mut i: usize = 2;
    for s in deque.iter().skip(1) {
        let k = (i as f32 - 1.0) / i as f32;
        out.pos[0] = out.pos[0] * k + s.pos[0] * (1.0 - k);
        out.pos[1] = out.pos[1] * k + s.pos[1] * (1.0 - k);
        if opts.stabilize_sensors {
            out.pressure = out.pressure * k + s.pressure * (1.0 - k);
            out.tilt[0]  = out.tilt[0]  * k + s.tilt[0]  * (1.0 - k);
            out.tilt[1]  = out.tilt[1]  * k + s.tilt[1]  * (1.0 - k);
        }
        i += 1;
    }
    out
}

/// Krita STABILIZER step. Returns `Some(sample)` to commit, or `None`
/// when the delay-distance gate decided the cursor has not moved far
/// enough. `force` bypasses the gate (use for pen-up drain).
pub fn stabilizer_step(
    state: &mut StabilizerState,
    raw: PenSample,
    opts: &SmoothingOptions,
    force: bool,
) -> Option<PenSample> {
    let last_committed = state.last_emitted.unwrap_or_else(|| {
        // First call: anchor at the raw point so the deque-blended
        // output starts where the user pressed.
        let s = state.deque.front().copied().unwrap_or(raw);
        state.last_emitted = Some(s);
        s
    });

    if !force && opts.use_delay_distance {
        let dx = raw.pos[0] - last_committed.pos[0];
        let dy = raw.pos[1] - last_committed.pos[1];
        if dx * dx + dy * dy < opts.delay_distance * opts.delay_distance {
            // Cursor inside the radius — hold position, no new ink.
            // Krita also peeks at the deque head to refresh its
            // contents; we copy that behaviour so a held pen does not
            // accumulate stale history.
            for slot in state.deque.iter_mut() {
                *slot = last_committed;
            }
            return None;
        }
    }

    let blended = stabilizer_blend(&state.deque, raw, opts);
    state.deque.pop_front();
    state.deque.push_back(raw);
    state.last_emitted = Some(blended);
    Some(blended)
}

/// Drain the stabilizer queue on stroke end so the trailing rope
/// catches up to the cursor.
pub fn stabilizer_finish(state: &mut StabilizerState, opts: &SmoothingOptions) -> Vec<PenSample> {
    if !opts.finish_stabilized_curve {
        return Vec::new();
    }
    let mut out = Vec::new();
    // We pop one sample at a time and re-blend so the trail decays
    // smoothly. Bounded by the deque size we filled in `new`.
    let steps = state.deque.len();
    for _ in 0..steps {
        if state.deque.len() <= 1 { break; }
        // Use the last_emitted as the "raw" target so the catch-up
        // converges on where the user left the cursor.
        let target = state.last_emitted
            .or_else(|| state.deque.back().copied())
            .unwrap_or_default();
        let blended = stabilizer_blend(&state.deque, target, opts);
        state.deque.pop_front();
        state.last_emitted = Some(blended);
        out.push(blended);
    }
    out
}

impl Default for PenSample {
    fn default() -> Self {
        Self { pos: [0.0, 0.0], pressure: 0.0, tilt: [0.0, 0.0] }
    }
}
