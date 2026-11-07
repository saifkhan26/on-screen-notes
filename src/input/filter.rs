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

/// One 1€ filter over an `N`-component value: position (`N = 2`) or
/// pressure (`N = 1`).
///
/// All components share one cutoff, taken from the speed (the length of
/// the filtered derivative). Filtering x and y separately gives the slow
/// axis a lower cutoff, so more lag, than the fast one; the point then
/// leaves the line the pen drew, and a shallow line comes out as flat
/// runs and steep catch-ups. With one cutoff each output is a weighted
/// mix of input points, so it stays on the pen's path.
///
/// The math is short but easy to misread, so each step is annotated.
#[derive(Debug, Clone)]
struct OneEuro<const N: usize> {
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
    prev_x:  Option<[f32; N]>,
    /// Previous low-pass derivative.
    prev_dx: [f32; N],
    /// Timestamp of the previous sample (seconds since some t0).
    prev_t:  Option<f32>,
}

impl<const N: usize> OneEuro<N> {
    fn new(min_cutoff: f32, beta: f32, d_cutoff: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            d_cutoff,
            prev_x:  None,
            prev_dx: [0.0; N],
            prev_t:  None,
        }
    }

    /// Reset all internal state — call on pen-down so a new stroke does not
    /// inherit the previous stroke's filter state.
    fn reset(&mut self) {
        self.prev_x  = None;
        self.prev_dx = [0.0; N];
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
    fn filter(&mut self, x: [f32; N], t: f32) -> [f32; N] {
        // First sample: nothing to smooth against; pass through and seed.
        let (prev_x, prev_t) = match (self.prev_x, self.prev_t) {
            (Some(px), Some(pt)) => (px, pt),
            _ => {
                self.prev_x  = Some(x);
                self.prev_dx = [0.0; N];
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
        let a_d = Self::alpha(self.d_cutoff, te);
        let mut dx = [0.0; N];
        for i in 0..N {
            let dx_raw = (x[i] - prev_x[i]) / te;
            dx[i] = a_d * dx_raw + (1.0 - a_d) * self.prev_dx[i];
        }

        // 2) Adaptive cutoff: faster → higher cutoff → less smoothing.
        let speed = dx.iter().map(|v| v * v).sum::<f32>().sqrt();
        let cutoff = self.min_cutoff + self.beta * speed;

        // 3) Low-pass every component with that one cutoff.
        let a = Self::alpha(cutoff, te);
        let mut xf = [0.0; N];
        for i in 0..N {
            xf[i] = a * x[i] + (1.0 - a) * prev_x[i];
        }

        self.prev_x  = Some(xf);
        self.prev_dx = dx;
        self.prev_t  = Some(t);
        xf
    }
}

/// Per-pen-stroke smoothing state. One filter on position, one on pressure.
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
    fpos: OneEuro<2>,
    fp: OneEuro<1>,
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
    /// Time origin for `begin_batch`; only differences matter.
    clock: std::time::Instant,
    /// When the previous batch was polled, in seconds on `clock`.
    last_poll: Option<f64>,
    /// Time of the last packet handed out, and the gap to the next one in
    /// this batch (seconds on `clock`). See `begin_batch`.
    next_t: f64,
    step: f64,
    /// Start of the current stroke on `clock`. The filters get times
    /// relative to it, so an `f32` keeps them exact however long the app
    /// has been running.
    stroke_t0: f64,
}

/// Longest gap assumed between two packets of one batch: tablets send
/// 130–270 packets a second while the pen moves.
const MAX_PACKET_GAP: f64 = 0.01;

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
            fpos: OneEuro::new(mc, beta, 1.0),
            // Pressure tracks user intent directly across all levels.
            fp: OneEuro::new(3.0, 0.2, 1.0),
            wobble: WobbleSmoother::new(0.04, 50.0, 55.0),
            level,
            min_dist_px: min_dist,
            last_accepted_pos: None,
            clock: std::time::Instant::now(),
            last_poll: None,
            next_t: 0.0,
            step: 0.0,
            stroke_t0: 0.0,
        }
    }

    /// Call once per poll with the number of packets it returned, before
    /// filtering them.
    ///
    /// Wintab is drained once a frame, so a batch holds every packet since
    /// the last frame. Stamped with the time they are filtered at, those
    /// packets come microseconds apart and the frame's first one a whole
    /// frame after the last; the filter's cutoff then jumps every frame
    /// and bends curves into a wobble. They are spread evenly over the
    /// time since the last poll instead, at most `MAX_PACKET_GAP` apart.
    pub fn begin_batch(&mut self, packets: usize) {
        let now = self.clock.elapsed().as_secs_f64();
        self.begin_batch_at(packets, now);
    }

    fn begin_batch_at(&mut self, packets: usize, now: f64) {
        let n = packets.max(1) as f64;
        let earliest = now - n * MAX_PACKET_GAP;
        let start = self.last_poll.map_or(earliest, |p| p.max(earliest)).min(now);
        self.next_t = start;
        self.step = (now - start) / n;
        self.last_poll = Some(now);
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
        self.fpos.reset();
        self.fp.reset();
        self.wobble.reset();
        self.last_accepted_pos = None;
        self.stroke_t0 = self.next_t;
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
        self.next_t += self.step;
        let t = (self.next_t - self.stroke_t0) as f32;
        self.filter_at(sample, force_accept, t)
    }

    /// `filter` with the sample's time (seconds into the stroke) given.
    fn filter_at(&mut self, sample: PenSample, force_accept: bool, t: f32) -> Option<PenSample> {

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
        let [fx, fy] = self.fpos.filter(wobble_pos, t);
        let [fp] = self.fp.filter([sample.pressure], t);
        let fp = fp.clamp(0.0, 1.0);
        self.last_accepted_pos = Some([fx, fy]);
        Some(PenSample {
            pos: [fx, fy],
            pressure: fp,
            tilt: sample.tilt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::smoothing::{SmoothingOptions, SmoothingType};
    use crate::canvas::stroke::{Stroke, StrokeStyle};
    use std::f32::consts::PI;

    const A: [f32; 2] = [100.0, 500.0];
    const B: [f32; 2] = [900.0, 330.0];
    const C: [f32; 2] = [400.0, 400.0];
    const R: f32 = 120.0;

    fn sample(pos: [f32; 2]) -> PenSample {
        PenSample { pos, pressure: 0.6, tilt: [0.0, 0.0] }
    }

    /// A hand-drawn stroke: 200 Hz packets along a shallow line or a
    /// circle, speeding up and slowing down with a 7 Hz speed tremor.
    /// `round` snaps them to whole pixels, as Wintab's are.
    fn packets(circle: bool, dur: f32, phase: f32, round: bool) -> Vec<[f32; 2]> {
        let n = (dur * 200.0) as usize;
        (0..=n)
            .map(|k| {
                let t = k as f32 / 200.0;
                let tau = t / dur;
                let s = 10.0 * tau.powi(3) - 15.0 * tau.powi(4) + 6.0 * tau.powi(5)
                    + 0.004 * (2.0 * PI * 7.0 * t + phase).sin() * (PI * tau).sin();
                let p = if circle {
                    [C[0] + R * (2.0 * PI * s).cos(), C[1] + R * (2.0 * PI * s).sin()]
                } else {
                    [A[0] + (B[0] - A[0]) * s, A[1] + (B[1] - A[1]) * s]
                };
                if round { [p[0].round(), p[1].round()] } else { p }
            })
            .collect()
    }

    /// Feed `pk` through the filter the way the app does: drained once a
    /// frame (60 fps, every 7th frame dropped).
    fn filtered(level: SmoothingLevel, pk: &[[f32; 2]]) -> Vec<PenSample> {
        let mut f = PenFilter::for_level(level);
        let mut out = Vec::new();
        let (mut frame, mut k, mut i) = (0.0_f64, 0, 0);
        while i < pk.len() {
            k += 1;
            frame += if k % 7 == 0 { 1.0 / 30.0 } else { 1.0 / 60.0 };
            let batch: Vec<usize> = (i..pk.len()).take_while(|&j| j as f64 / 200.0 <= frame).collect();
            f.begin_batch_at(batch.len(), frame);
            for j in batch {
                if j == 0 { f.reset(); }
                let edge = j == 0 || j == pk.len() - 1;
                out.extend(f.filter(sample(pk[j]), edge));
                i += 1;
            }
        }
        out
    }

    /// The committed spine of an Adaptive-mode stroke.
    fn spine(level: SmoothingLevel, pk: &[[f32; 2]]) -> Vec<[f32; 2]> {
        let opts = SmoothingOptions { kind: SmoothingType::Adaptive, ..SmoothingOptions::default() };
        let mut s = Stroke::with_style_and_smoothing([0, 0, 0, 255], 7.0, StrokeStyle::Default, opts);
        for p in filtered(level, pk) {
            s.push(p);
        }
        s.build_cache_with(level.position_passes());
        s.cache.unwrap().points
    }

    /// Farthest any point (ends trimmed) is from the true path.
    fn max_off(circle: bool, pts: &[[f32; 2]]) -> f32 {
        let m = pts.len();
        pts[m / 20..m - m / 20]
            .iter()
            .map(|p| {
                if circle {
                    ((p[0] - C[0]).hypot(p[1] - C[1]) - R).abs()
                } else {
                    let d = [B[0] - A[0], B[1] - A[1]];
                    ((p[0] - A[0]) * -d[1] + (p[1] - A[1]) * d[0]).abs() / d[0].hypot(d[1])
                }
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_straight_stroke_stays_on_its_line() {
        // One cutoff for both axes keeps every output a mix of points on
        // the line. Separate x / y filters lagged the slow axis and
        // pulled the stroke up to 3 px off it, in steps.
        for level in [SmoothingLevel::Medium, SmoothingLevel::High] {
            for dur in [0.6, 1.5] {
                let off = max_off(false, &filtered(level, &packets(false, dur, 0.0, false))
                    .iter().map(|s| s.pos).collect::<Vec<_>>());
                assert!(off < 1.0e-3, "{level:?} {dur}s: {off} px off the line");
            }
        }
    }

    #[test]
    fn whole_pixel_input_draws_a_straight_line() {
        // Rounding alone is up to half a pixel; smoothing must not add to it.
        for level in [SmoothingLevel::Medium, SmoothingLevel::High] {
            for ph in 0..8 {
                for dur in [0.6, 1.5] {
                    let off = max_off(false, &spine(level, &packets(false, dur, ph as f32 * 0.8, true)));
                    assert!(off < 0.6, "{level:?} {dur}s: {off} px off the line");
                }
            }
        }
    }

    #[test]
    fn a_fast_circle_stays_round() {
        // Lag pulls a fast circle in a little, evenly. Packets filtered at
        // the frame's time instead bent it by up to 6 px.
        for ph in 0..8 {
            let off = max_off(true, &spine(SmoothingLevel::Medium, &packets(true, 0.6, ph as f32 * 0.8, true)));
            assert!(off < 2.5, "{off} px off the circle");
        }
    }

    #[test]
    fn a_batch_is_spread_over_the_time_since_the_last_poll() {
        let mut f = PenFilter::for_level(SmoothingLevel::Medium);
        f.begin_batch_at(1, 1.0);
        f.begin_batch_at(4, 1.02);
        let ts: Vec<f64> = (0..4).map(|_| { f.next_t += f.step; f.next_t }).collect();
        for (t, want) in ts.iter().zip([1.005, 1.01, 1.015, 1.02]) {
            assert!((t - want).abs() < 1e-9, "{ts:?}");
        }
        // After a pause the packets are put no more than 10 ms apart.
        f.begin_batch_at(2, 5.0);
        assert!((f.next_t - 4.98).abs() < 1e-9 && (f.step - 0.01).abs() < 1e-9);
    }
}
