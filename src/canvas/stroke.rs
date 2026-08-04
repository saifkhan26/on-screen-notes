//! `PenSample` and `Stroke` — the raw building blocks of an ink stroke.
//!
//! A `PenSample` is one tick of pen telemetry: position, pressure, optional
//! tilt. A `Stroke` is the ordered list of samples that make up one drag
//! from pen-down to pen-up, plus the visual properties (color, base width).
//!
//! Why pressure is `f32` 0..=1: every backend normalises into this range so
//! we never have to worry about device-specific maximum levels (1024, 4096, …).

use serde::{Deserialize, Serialize};

use crate::canvas::smoothing::{
    self, ModeState, SmoothingOptions, SmoothingType, StabilizerState, WeightedState,
};

/// A single pen reading at one instant in time.
///
/// Position is stored in the canvas's own logical coordinates (not screen
/// pixels). Conversion to screen happens at render time via the Canvas's
/// pan/zoom transform.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PenSample {
    /// Position in canvas-local coordinates.
    pub pos: [f32; 2],
    /// 0.0 = no contact / no pressure, 1.0 = max pressure. We leave the value
    /// untouched here; pressure-curve reshaping happens in the stroke renderer.
    pub pressure: f32,
    /// Tilt as `[x, y]` in radians. Zero when not provided by the device.
    /// Useful for future "calligraphy" features.
    pub tilt: [f32; 2],
}

impl PenSample {
    /// Convenience constructor when the only thing you have is a position
    /// (e.g. mouse fallback when no pen is connected).
    pub fn from_pos(pos: [f32; 2]) -> Self {
        Self { pos, pressure: 1.0, tilt: [0.0, 0.0] }
    }
}

/// Visual style for a stroke. Picked at the moment the user pen-downs and
/// then frozen into the stroke for the rest of its life — switching the
/// active style later does not retro-recolour earlier strokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeStyle {
    /// Smooth opaque ribbon (existing pen look).
    Default,
    /// Translucent, grainy line meant to read as a graphite pencil mark.
    Pencil,
    /// Highlighter-style translucent constant-width ribbon. Pressure
    /// does not modulate width, so overlapping strokes read as
    /// uniform marker tracks; alpha is ~45 % so stacked passes
    /// darken into a multi-pass highlight.
    Marker,
    /// Spray-can disc-stamp walk at low per-stamp alpha. The
    /// renderer stamps a halo of soft discs along the polyline so
    /// holding the pen still in one spot builds up density toward
    /// solid colour — same idle-density build users expect from
    /// Photoshop's airbrush.
    Airbrush,
}

impl Default for StrokeStyle {
    fn default() -> Self { StrokeStyle::Default }
}

fn default_stroke_style() -> StrokeStyle { StrokeStyle::Default }

/// One pen-down-to-pen-up stroke, with all its visual properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stroke {
    /// All recorded samples, in order. The first sample is the pen-down
    /// position, the last is the pen-up.
    pub samples: Vec<PenSample>,
    /// Stroke color as RGBA bytes.
    pub color: [u8; 4],
    /// Maximum width in canvas pixels. The actual width at each segment is
    /// `base_width * pressure`, so a low-pressure tap produces a thin line.
    pub base_width: f32,

    /// Visual style applied at paint time. `#[serde(default)]` keeps older
    /// RON files (saved before the field existed) loading as plain
    /// `Default` strokes.
    #[serde(default = "default_stroke_style")]
    pub style: StrokeStyle,

    /// If true, the renderer also fills the polygon enclosed by the
    /// stroke's polyline (auto-joining start ↔ end). Used by the Fill
    /// tool. `#[serde(default)]` keeps strokes saved before this field
    /// existed loading as plain outlines.
    #[serde(default)]
    pub filled: bool,

    /// Pre-tessellated polyline cache built lazily on first paint and
    /// invalidated whenever samples change. We keep it out of the on-disk
    /// format (`#[serde(skip)]`) — it's pure render scratch. When this
    /// cache is `Some` we skip the Catmull-Rom subdivision step on every
    /// paint, which is the hot loop for canvases with many strokes.
    #[serde(skip)]
    pub cache: Option<StrokeCache>,

    /// Per-stroke One Euro Filter state. Filtered positions are baked
    /// into `samples` as they arrive, so the filter itself is pure
    /// scratch (`#[serde(skip)]`). Lazy-initialised on the first sample
    /// in `push()` so we don't pay the heap allocation for the (many)
    /// strokes loaded from disk that never receive new samples.
    #[serde(skip)]
    filter: Option<OneEuroFilter>,

    /// Snapshot of the user's smoothing options at stroke begin. Held
    /// per stroke so mid-stroke UI changes do not retro-apply to ink
    /// already committed. `#[serde(skip)]` because it's pure runtime
    /// state — committed strokes don't carry the parameters used to
    /// build them.
    #[serde(skip)]
    pub smoothing: SmoothingOptions,

    /// Per-mode transient state (Weighted history, Stabilizer deque, …).
    /// `None` until the first sample arrives — that lets a deserialised
    /// stroke skip the allocation entirely. Adaptive mode never touches
    /// this field (it uses `filter` above).
    #[serde(skip)]
    mode_state: Option<ModeState>,
}

/// **Causal** forward EMA over positions
/// (`p[i] = α·p[i] + (1-α)·p[i-1]`). Each output depends only on
/// earlier inputs — never on later ones. This is critical for live
/// drawing: when a new pen sample arrives, the smoothed positions of
/// every prior sample stay frozen, so the user does not see the
/// already-drawn tail wobble as the stroke grows.
///
/// (A symmetric 3-tap MA would smooth more aggressively for the same
/// number of passes, but every new sample would re-shape the last few
/// points of the stroke, which read as a visible ringing wobble at
/// direction changes — the exact artifact this function avoids.)
///
/// `α = 0.7` is a good trade-off: 1 pass settles to 95 % of a step
/// input within ~3 samples (~15 ms at 200 Hz Wintab), which is below
/// human perception, while still attenuating digitiser jitter. Call
/// multiple times to widen the effective response.
pub(crate) fn smooth_positions_in_place(points: &mut [[f32; 2]]) {
    let n = points.len();
    if n < 2 { return; }
    let alpha = 0.7_f32;
    for i in 1..n {
        points[i] = [
            alpha * points[i][0] + (1.0 - alpha) * points[i - 1][0],
            alpha * points[i][1] + (1.0 - alpha) * points[i - 1][1],
        ];
    }
}

/// **One Euro Filter** — adaptive low-pass for pen input.
///
/// Reference: Casiez, Roussel, Vogel — "1€ Filter: A Simple Speed-based
/// Low-pass Filter for Noisy Input in Interactive Systems" (CHI 2012).
/// Same algorithm Krita's "Stabilizer Basic" / MyPaint use.
///
/// The key trick: the cutoff frequency is **derived from pen speed**.
/// At slow pen speed the cutoff drops → heavy filter → digitiser tremor
/// is killed. At high pen speed the cutoff rises → near pass-through →
/// quick scribbles don't feel laggy. A plain fixed EMA cannot do both
/// at once, which is exactly the artefact the user reported (slow
/// circles wobble even though fast scribbles look fine).
///
/// Sample rate `te` is assumed constant at ~180 Hz (Wintab packet rate
/// is 133–200 Hz; the formula is forgiving of drift).
#[derive(Debug, Clone)]
pub(crate) struct OneEuroFilter {
    /// Baseline cutoff (Hz). Lower = heavier filter at zero pen speed.
    /// 1.0 Hz is the paper default and what Krita ships with — gives
    /// rock-steady slow circles without feeling sluggish to the user.
    min_cutoff: f32,
    /// Velocity coupling. Higher = filter relaxes faster as the pen
    /// speeds up. 0.05 sits in the middle of the Krita / MyPaint range
    /// (0.001–0.05) and prevents perceptible lag on fast scribbles
    /// while still letting `min_cutoff` dominate at rest.
    beta: f32,
    /// Cutoff for the velocity estimate. Paper recommends 1.0 Hz and
    /// notes "almost nobody changes it" — we follow.
    d_cutoff: f32,
    /// Sample period (s). Wintab packets arrive ~5.5 ms apart on
    /// average. Treating this as constant is fine: a 30 % drift in te
    /// shifts the effective cutoffs by the same factor and is masked
    /// by the adaptive cutoff itself.
    te: f32,

    /// Previous filter output (None on the very first sample).
    x_prev: Option<[f32; 2]>,
    /// Previous filtered velocity (px/s). Used to low-pass the noisy
    /// raw velocity estimate before it feeds back into the cutoff.
    dx_prev: [f32; 2],
}

impl OneEuroFilter {
    pub fn new() -> Self {
        Self {
            min_cutoff: 1.0,
            beta:       0.05,
            d_cutoff:   1.0,
            te:         1.0 / 180.0,
            x_prev:     None,
            dx_prev:    [0.0, 0.0],
        }
    }

    /// Standard exponential-smoothing α from cutoff frequency `fc`
    /// (Hz). Derived from the discrete first-order low-pass response:
    /// `α = 1 / (1 + τ / te)` where `τ = 1 / (2π·fc)`.
    #[inline]
    fn alpha(&self, fc: f32) -> f32 {
        let tau = 1.0 / (2.0 * std::f32::consts::PI * fc);
        1.0 / (1.0 + tau / self.te)
    }

    /// Push a raw position `x`; receive the filtered position.
    pub fn filter(&mut self, x: [f32; 2]) -> [f32; 2] {
        // First sample bootstraps state — no derivative yet, return as-is.
        let prev = match self.x_prev {
            Some(p) => p,
            None => {
                self.x_prev = Some(x);
                return x;
            }
        };

        // 1) Estimate raw 2-D velocity (px/s).
        let dx = [
            (x[0] - prev[0]) / self.te,
            (x[1] - prev[1]) / self.te,
        ];
        // 2) Low-pass the velocity at `d_cutoff` so the cutoff signal
        //    itself isn't noisy.
        let ad = self.alpha(self.d_cutoff);
        let dx_filt = [
            ad * dx[0] + (1.0 - ad) * self.dx_prev[0],
            ad * dx[1] + (1.0 - ad) * self.dx_prev[1],
        ];
        self.dx_prev = dx_filt;

        // 3) Per-sample cutoff: rises with filtered speed → quick
        //    strokes get less smoothing, slow strokes get more.
        let speed = (dx_filt[0] * dx_filt[0] + dx_filt[1] * dx_filt[1]).sqrt();
        let fc = self.min_cutoff + self.beta * speed;
        let a = self.alpha(fc);

        // 4) Final low-pass on position.
        let filt = [
            a * x[0] + (1.0 - a) * prev[0],
            a * x[1] + (1.0 - a) * prev[1],
        ];
        self.x_prev = Some(filt);
        filt
    }
}

/// **Bidirectional Gaussian smoothing** over polyline positions.
///
/// Unlike `smooth_positions_in_place` (causal forward EMA) this is a
/// symmetric Gaussian convolution — every output sample averages a
/// window of past *and* future samples. That makes it dramatically
/// more effective at killing hand-tremor wiggle (~10 Hz), which a
/// pure causal filter can only attenuate at the cost of lag.
///
/// We use it inside `build_cache_with` after the polyline is built:
/// the stroke is **already complete** at that point, so there is no
/// causality requirement — non-causal smoothing has zero perceptible
/// downside and fixes the residual wiggle the causal EMA leaves
/// behind.
///
/// `sigma` is in polyline-index units (one pen sample is ~8
/// subdivided points after `build_cache_with`). σ = 3-6 typically
/// kills tremor while preserving deliberate curvature.
///
/// Currently unused: the ribbon-spine builder produces a sparse,
/// already-smooth polyline (centripetal spline + decimation), so an
/// index-space Gaussian would round deliberate corners. Kept for
/// optional future polish tuning.
#[allow(dead_code)]
pub(crate) fn gaussian_smooth_positions_bidir(pts: &mut [[f32; 2]], sigma: f32) {
    let n = pts.len();
    if n < 3 || sigma <= 0.0 { return; }
    // Kernel radius: 3σ covers 99.7 % of the Gaussian mass.
    let radius = (sigma * 3.0).ceil() as isize;
    let two_sigma2 = 2.0 * sigma * sigma;
    let kernel_len = (2 * radius + 1) as usize;
    let mut kernel: Vec<f32> = Vec::with_capacity(kernel_len);
    let mut sum = 0.0_f32;
    for i in -radius..=radius {
        let w = (-((i * i) as f32) / two_sigma2).exp();
        kernel.push(w);
        sum += w;
    }
    // Normalise so the convolution preserves total weight.
    for w in kernel.iter_mut() { *w /= sum; }

    let mut out: Vec<[f32; 2]> = vec![[0.0; 2]; n];
    for i in 0..n {
        let mut ax = 0.0_f32;
        let mut ay = 0.0_f32;
        for k in -radius..=radius {
            // Clamp at the ends so endpoints don't drift inward.
            let idx = (i as isize + k).clamp(0, (n - 1) as isize) as usize;
            let w = kernel[(k + radius) as usize];
            ax += pts[idx][0] * w;
            ay += pts[idx][1] * w;
        }
        out[i] = [ax, ay];
    }
    pts.copy_from_slice(&out);
}

/// Bidirectional Gaussian smoothing on widths — same idea as the
/// position variant. Keeps the ribbon's width transitions matching
/// the smoothed centerline so we don't get smooth geometry with
/// jagged thickness.
///
/// Currently unused (see `gaussian_smooth_positions_bidir`); kept for
/// optional future polish tuning.
#[allow(dead_code)]
pub(crate) fn gaussian_smooth_widths_bidir(widths: &mut [f32], sigma: f32) {
    let n = widths.len();
    if n < 3 || sigma <= 0.0 { return; }
    let radius = (sigma * 3.0).ceil() as isize;
    let two_sigma2 = 2.0 * sigma * sigma;
    let kernel_len = (2 * radius + 1) as usize;
    let mut kernel: Vec<f32> = Vec::with_capacity(kernel_len);
    let mut sum = 0.0_f32;
    for i in -radius..=radius {
        let w = (-((i * i) as f32) / two_sigma2).exp();
        kernel.push(w);
        sum += w;
    }
    for w in kernel.iter_mut() { *w /= sum; }

    let mut out = vec![0.0_f32; n];
    for i in 0..n {
        let mut a = 0.0_f32;
        for k in -radius..=radius {
            let idx = (i as isize + k).clamp(0, (n - 1) as isize) as usize;
            let w = kernel[(k + radius) as usize];
            a += widths[idx] * w;
        }
        out[i] = a;
    }
    widths.copy_from_slice(&out);
}

/// One **causal** EMA pass over widths (`w[i] = α·w[i] + (1-α)·w[i-1]`).
///
/// Each output width depends only on earlier widths — never on later
/// ones. That guarantees pressing harder at the end of a stroke cannot
/// thicken the start retroactively. The transition is also kept short:
/// with `α = 0.7` a 1-pass sweep reaches 95 % of a step input in ~3
/// polyline points (~0.4 ms at 200 Hz × 8 substeps), so a deliberate
/// pressure spike thickens the stroke immediately where the user
/// applied it instead of being smeared across the whole stroke.
pub(crate) fn smooth_widths_causal(widths: &mut [f32]) {
    if widths.len() < 2 { return; }
    let alpha = 0.7_f32;
    for i in 1..widths.len() {
        widths[i] = alpha * widths[i] + (1.0 - alpha) * widths[i - 1];
    }
}

/// **Centripetal** Catmull-Rom interpolation at parameter `u` ∈ [0, 1]
/// along the segment between `p1` and `p2`. `p0` and `p3` are the
/// preceding and following control points (clamped at endpoints).
///
/// Centripetal (α = 0.5, after Yuksel et al. 2009) is the variant
/// that **does not overshoot or form cusps** when the control points
/// are tightly clustered. The classical uniform Catmull-Rom (α = 0)
/// we used before would amplify low-speed sampling noise into visible
/// wobble — exactly the "jagged at slow pen" artifact. Centripetal
/// parametrises each segment by `‖p_{i+1} − p_i‖^α`, which makes the
/// spline behave gracefully when adjacent samples sit on top of each
/// other.
#[allow(dead_code)]
pub(crate) fn catmull_rom(
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    u: f32,
) -> [f32; 2] {
    // Knot spacing for α = 0.5. We add a tiny epsilon so two coincident
    // control points (distance = 0) never produce a zero-width interval
    // that would divide-by-zero later in the de Boor evaluation.
    let alpha = 0.5_f32;
    let knot = |a: [f32; 2], b: [f32; 2]| -> f32 {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        (dx * dx + dy * dy).sqrt().powf(alpha).max(1.0e-6)
    };
    let t0 = 0.0_f32;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let t  = t1 + (t2 - t1) * u;

    // 2-D linear interpolation helper.
    let lerp2 = |w: f32, a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        [a[0] + (b[0] - a[0]) * w, a[1] + (b[1] - a[1]) * w]
    };

    // De Boor evaluation in three stages — standard Catmull-Rom via
    // repeated linear interpolation over the four control points.
    let a1 = lerp2((t - t0) / (t1 - t0), p0, p1);
    let a2 = lerp2((t - t1) / (t2 - t1), p1, p2);
    let a3 = lerp2((t - t2) / (t3 - t2), p2, p3);
    let b1 = lerp2((t - t0) / (t2 - t0), a1, a2);
    let b2 = lerp2((t - t1) / (t3 - t1), a2, a3);
    lerp2((t - t1) / (t2 - t1), b1, b2)
}

/// Pre-tessellated polyline for one stroke.
#[derive(Debug, Clone)]
pub struct StrokeCache {
    /// Subdivided polyline points, in **canvas-local** coordinates (no
    /// pan, no zoom applied). The renderer translates and scales these on
    /// every paint, which is far cheaper than recomputing Catmull-Rom.
    pub points: Vec<[f32; 2]>,
    /// Per-point full width in canvas-local pixels. The renderer multiplies
    /// by the canvas's current zoom on each paint.
    pub widths: Vec<f32>,
}

impl Stroke {
    /// Build a stroke with a chosen visual style.
    pub fn with_style(color: [u8; 4], base_width: f32, style: StrokeStyle) -> Self {
        Self {
            samples: Vec::with_capacity(64),
            color,
            base_width,
            style,
            filled: false,
            cache: None,
            filter: None,
            smoothing: SmoothingOptions::default(),
            mode_state: None,
        }
    }

    /// Build a stroke with a chosen visual style **and** smoothing
    /// snapshot. Use this when the caller (pen tool) wants to lock
    /// the active `SmoothingOptions` into the stroke at pen-down.
    pub fn with_style_and_smoothing(
        color: [u8; 4],
        base_width: f32,
        style: StrokeStyle,
        smoothing: SmoothingOptions,
    ) -> Self {
        Self {
            samples: Vec::with_capacity(64),
            color,
            base_width,
            style,
            filled: false,
            cache: None,
            filter: None,
            smoothing,
            mode_state: None,
        }
    }

    /// Build a stroke that the renderer will fill (legacy Fill
    /// tool). The outline is still rendered on top so the user
    /// sees their actual pen path; the interior gets the polygon
    /// fill. Currently unused — the Fill tool moved to raster
    /// flood-fill — but kept so older save files restore cleanly.
    #[allow(dead_code)]
    pub fn with_fill(color: [u8; 4], base_width: f32) -> Self {
        Self {
            samples: Vec::with_capacity(64),
            color,
            base_width,
            style: StrokeStyle::Default,
            filled: true,
            cache: None,
            filter: None,
            smoothing: SmoothingOptions::default(),
            mode_state: None,
        }
    }

    /// Append a sample. Smoothing happens at render time (Catmull-Rom).
    /// We do clear the render cache because adding a sample changes the
    /// stroke's geometry — next paint will rebuild.
    ///
    /// ## Pipeline
    ///
    /// 1. **One Euro Filter on position.** Adaptive low-pass — heavy at
    ///    slow pen speed (kills tremor on slow circles), passthrough at
    ///    high pen speed (no lag on fast scribbles). The single most
    ///    important step for "feels like Sticky Notes" smoothness;
    ///    plain EMA could not do both.
    /// 2. **Duplicate / sub-pixel gate.** After filtering, drop the new
    ///    sample if it sits within `min_dist` of the previous kept
    ///    sample. Keeps the polyline from accumulating near-coincident
    ///    points that bloat the Catmull-Rom subdivision and produce
    ///    degenerate substeps.
    ///
    /// `min_dist` scales with `base_width` so a fat brush gets a coarser
    /// gate (you cannot see sub-2-px detail on a 20-px line). Floor at
    /// 1.0 px because OEF already kills the wobble that motivated the
    /// previous 1.5-px floor — the gate is now only a duplicate filter,
    /// not a primary smoother.
    pub fn push(&mut self, sample: PenSample) {
        match self.smoothing.kind {
            SmoothingType::Adaptive => self.push_adaptive(sample),
            SmoothingType::None     => self.push_raw(sample),
            SmoothingType::Simple   => self.push_simple(sample),
            SmoothingType::Weighted => self.push_weighted(sample),
            SmoothingType::Stabilizer => {
                if let Some(out) = self.push_stabilizer(sample, false) {
                    self.accept(out);
                }
            }
        }
    }

    /// Drain remaining stabilizer queue on pen-up so the rope catches
    /// up to the cursor. No-op for every other mode.
    pub fn finish(&mut self) {
        if !matches!(self.smoothing.kind, SmoothingType::Stabilizer) {
            return;
        }
        let opts = self.smoothing;
        let Some(ModeState::Stabilizer(state)) = self.mode_state.as_mut() else { return; };
        let extras = smoothing::stabilizer_finish(state, &opts);
        for s in extras {
            self.accept(s);
        }
    }

    /// Existing pipeline: in-stroke One Euro on positions + min-dist
    /// gate. Preserves byte-for-byte legacy behaviour.
    fn push_adaptive(&mut self, mut sample: PenSample) {
        let filter = self.filter.get_or_insert_with(OneEuroFilter::new);
        sample.pos = filter.filter(sample.pos);

        if let Some(last) = self.samples.last() {
            let dx = sample.pos[0] - last.pos[0];
            let dy = sample.pos[1] - last.pos[1];
            let d2 = dx * dx + dy * dy;
            let min_dist = (self.base_width * 0.15).clamp(1.0, 4.0);
            if d2 < min_dist * min_dist {
                return;
            }
        }
        self.samples.push(sample);
        self.cache = None;
    }

    /// Krita NO_SMOOTHING: raw sample, only the tiny duplicate gate so
    /// idle digitiser ticks don't bloat the polyline.
    fn push_raw(&mut self, sample: PenSample) {
        if let Some(last) = self.samples.last() {
            let dx = sample.pos[0] - last.pos[0];
            let dy = sample.pos[1] - last.pos[1];
            if dx * dx + dy * dy < 0.25 { return; }
        }
        self.accept(sample);
    }

    /// Krita SIMPLE_SMOOTHING: average against the previous raw sample.
    fn push_simple(&mut self, sample: PenSample) {
        let state = self.mode_state
            .get_or_insert_with(|| ModeState::Simple { prev: None });
        let out = if let ModeState::Simple { prev } = state {
            smoothing::simple_step(prev, sample)
        } else {
            sample
        };
        self.accept(out);
    }

    /// Krita WEIGHTED_SMOOTHING: Gaussian-weighted history accumulation.
    fn push_weighted(&mut self, sample: PenSample) {
        let opts = self.smoothing;
        let state = self.mode_state
            .get_or_insert_with(|| ModeState::Weighted(WeightedState::new()));
        let out = if let ModeState::Weighted(w) = state {
            smoothing::weighted_step(w, sample, &opts)
        } else {
            sample
        };
        self.accept(out);
    }

    /// Krita STABILIZER: queue + delay-distance gate. Returns the
    /// blended output (or `None` when the gate suppresses the sample).
    fn push_stabilizer(&mut self, sample: PenSample, force: bool) -> Option<PenSample> {
        let opts = self.smoothing;
        let state = self.mode_state.get_or_insert_with(|| {
            ModeState::Stabilizer(StabilizerState::new(
                sample,
                opts.smoothness_distance.max(3.0) as usize,
            ))
        });
        if let ModeState::Stabilizer(s) = state {
            smoothing::stabilizer_step(s, sample, &opts, force)
        } else {
            Some(sample)
        }
    }

    /// Shared "accept this output sample" helper. Honours the same
    /// min-distance dedup gate every explicit Krita mode wants — keeps
    /// the polyline from accumulating near-coincident points.
    fn accept(&mut self, sample: PenSample) {
        if let Some(last) = self.samples.last() {
            let dx = sample.pos[0] - last.pos[0];
            let dy = sample.pos[1] - last.pos[1];
            if dx * dx + dy * dy < 0.25 { return; }
        }
        self.samples.push(sample);
        self.cache = None;
    }

    /// Build the cache with the *default* smoothing budget (3 position
    /// passes). Use `build_cache_with` to override.
    pub fn build_cache(&mut self) {
        self.build_cache_with(3);
    }

    /// Build the Catmull-Rom subdivision cache for this stroke. Cheap if
    /// already present (no-op). Producing the cache costs O(samples ×
    /// SUBSTEPS); paying it once saves the cost on every subsequent
    /// paint. `position_passes` controls how many symmetric MA passes
    /// run over the polyline positions — the smoothing-level UI maps to
    /// this directly (Low=1, Medium=3, High=6).
    pub fn build_cache_with(&mut self, position_passes: usize) {
        // Explicit Krita modes have already done the heavy smoothing
        // per-sample; the cache builder defers to the mode's own
        // budget instead of stacking another EMA on top.
        let position_passes = self.smoothing.cache_passes(position_passes);
        if self.cache.is_some() { return; }
        // Geometry is the shared ribbon-spine builder (centripetal
        // Catmull-Rom + adaptive arc-length / turn-angle decimation). The
        // live-preview path (`paint_stroke_live`) calls the same
        // `build_spine`, so an in-progress stroke matches its committed
        // form exactly.
        self.cache = Some(build_spine(&self.samples, self.base_width, position_passes));
    }

    // -- pressure curve global --------------------------------------------
    // see `pressure_curve` / `set_pressure_curve` below

    /// Bounding box (axis-aligned) of all samples. Used for hit-testing
    /// (eraser) and for clipping render work to dirty regions.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let mut iter = self.samples.iter();
        let first = iter.next()?;
        let mut min = first.pos;
        let mut max = first.pos;
        for s in iter {
            min[0] = min[0].min(s.pos[0]);
            min[1] = min[1].min(s.pos[1]);
            max[0] = max[0].max(s.pos[0]);
            max[1] = max[1].max(s.pos[1]);
        }
        // Inflate by the base width so a thick stroke isn't clipped to its
        // sample-points alone.
        let pad = self.base_width.clamp(0.5, 80.0);
        Some([min[0] - pad, min[1] - pad, max[0] + pad, max[1] + pad])
    }
}

// ---- Pressure-curve global --------------------------------------------
//
// The user can reshape how raw pen pressure (0..=1) maps to stroke
// width via a single exponent: `effective = raw.powf(curve)`. Stored
// as an atomic f32 (bits) so every cache builder reads the current
// value without threading a parameter through every call site. App
// invokes `set_pressure_curve` whenever the slider moves, then calls
// `CanvasManager::rebuild_all_caches` so existing strokes pick up
// the change.
//
// Range: [0.5, 2.0]. 1.0 = linear (no reshape). 0.5 = lighter touch
// (amplifies low-pressure pixels). 2.0 = heavier touch (low pressure
// stays thin).

use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

static PRESSURE_CURVE_BITS: AtomicU32 = AtomicU32::new(0x3F800000); // 1.0_f32 bits

/// Current pressure-curve exponent.
pub fn pressure_curve() -> f32 {
    f32::from_bits(PRESSURE_CURVE_BITS.load(AtomicOrdering::Relaxed))
}

/// Update the pressure-curve exponent. Call `rebuild_all_caches`
/// after to refresh existing strokes.
pub fn set_pressure_curve(c: f32) {
    let clamped = c.clamp(0.5, 2.0);
    PRESSURE_CURVE_BITS.store(clamped.to_bits(), AtomicOrdering::Relaxed);
}

/// Apply the curve to a raw pressure reading.
pub fn apply_curve(raw: f32, curve: f32) -> f32 {
    raw.clamp(0.0, 1.0).powf(curve)
}

// ---- Ribbon spine builder ---------------------------------------------
//
// Shared geometry core for both the committed cache (`build_cache_with`)
// and the live in-progress preview (`paint_stroke_live`). Ported from the
// "ribbon" stroke engine's input model, adapted to this app's vector
// renderer: we emit a `StrokeCache { points, widths }` that the existing
// mesh renderer consumes unchanged — no pixel buffer, no coverage buffer.
//
// The curve is a **centripetal Catmull-Rom** spline through the (already
// smoothed) control points, evaluated densely and then greedily
// **decimated** into spine nodes: a node is emitted when the arc length
// since the last node passes the node spacing, OR when the curve tangent
// has turned past ~10°. Arc-length spacing keeps node count low on straight
// runs; the turn-angle trigger keeps tight curls faceting-free with large
// brushes. Because the vector renderer rebuilds the whole spine every
// frame, the raster engine's incremental one-sample-lag / overlay
// machinery is unnecessary here.

/// Emit a spine node when the tangent has turned more than this since the
/// last node, regardless of arc length. `cos(10°)`.
const ANGLE_COS: f32 = 0.984_807_75;

/// Build the ribbon spine for a stroke: centripetal Catmull-Rom through the
/// pre-smoothed sample positions, adaptively decimated into
/// `StrokeCache { points, widths }`. `position_passes` is how many causal
/// MA passes pre-smooth the control points (already mapped through the
/// active Krita mode by the caller). Widths are `base_width` modulated by
/// the pressure curve; Marker (constant-width) overrides them at paint time.
pub fn build_spine(samples: &[PenSample], base_width: f32, position_passes: usize) -> StrokeCache {
    const MAX_WIDTH: f32 = 80.0;
    let base_width = base_width.clamp(0.5, MAX_WIDTH);
    let curve = pressure_curve();

    let n = samples.len();
    if n == 0 {
        return StrokeCache { points: Vec::new(), widths: Vec::new() };
    }
    // Width at a node: base_width scaled by (curved) pressure, floored so a
    // near-zero-pressure sample still paints a hairline rather than nothing.
    let width_at = |pr: f32| (base_width * pr.clamp(0.05, 1.0)).max(0.8);
    if n == 1 {
        let s = samples[0];
        let w = (base_width * apply_curve(s.pressure, curve).max(0.1)).max(0.8);
        return StrokeCache { points: vec![s.pos], widths: vec![w] };
    }

    // Pre-smooth raw control positions in sample-space (matches the old
    // path). Index 0 is left untouched by the causal EMA, so the spine
    // still lands its first node exactly on the pen-down point.
    let mut ctrl: Vec<[f32; 2]> = samples.iter().map(|s| s.pos).collect();
    for _ in 0..position_passes {
        smooth_positions_in_place(&mut ctrl);
    }
    // Curved pressures, one per control point.
    let pr: Vec<f32> = samples.iter().map(|s| apply_curve(s.pressure, curve)).collect();

    let radius = base_width * 0.5;
    // Node spacing: a fraction of the radius, never below 1px.
    let spacing = (radius * 0.3).max(1.0);
    // Dense-eval step size along each segment: keep sub-node samples well
    // under the radius so arc length / turn angle are measured finely.
    let eval_gap = (radius * 0.25).max(0.5);

    let mut points: Vec<[f32; 2]> = Vec::with_capacity(n * 2);
    let mut widths: Vec<f32> = Vec::with_capacity(n * 2);

    // First node = pen-down point (instant dot, zero lag).
    points.push(ctrl[0]);
    widths.push(width_at(pr[0]));

    let mut arc_since = 0.0_f32;
    let mut last_dir: Option<[f32; 2]> = None;
    let mut last_pt = ctrl[0];

    // Walk each Catmull-Rom segment ctrl[i-1] -> ctrl[i], with ctrl[i-2]
    // and ctrl[i+1] as the outer control points (clamped at the ends).
    for i in 1..n {
        let p0 = ctrl[i.saturating_sub(2)];
        let p1 = ctrl[i - 1];
        let p2 = ctrl[i];
        let p3 = ctrl[(i + 1).min(n - 1)];
        let pr1 = pr[i - 1];
        let pr2 = pr[i];

        let dx = p2[0] - p1[0];
        let dy = p2[1] - p1[1];
        let chord = (dx * dx + dy * dy).sqrt();
        if chord <= 0.0 {
            continue;
        }
        let steps = ((chord / eval_gap).ceil() as usize).max(2);

        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let pos = catmull_rom(p0, p1, p2, p3, t);
            // Pressure varies linearly across the segment (already
            // low-frequency after smoothing / curve).
            let pressure = pr1 + (pr2 - pr1) * t;

            let ex = pos[0] - last_pt[0];
            let ey = pos[1] - last_pt[1];
            let ds = (ex * ex + ey * ey).sqrt();
            if ds <= 1e-6 {
                continue;
            }
            arc_since += ds;
            let dir = [ex / ds, ey / ds];
            let turned = match last_dir {
                Some(l) => l[0] * dir[0] + l[1] * dir[1] < ANGLE_COS,
                None => false,
            };
            if arc_since >= spacing || turned {
                points.push(pos);
                widths.push(width_at(pressure));
                arc_since = 0.0;
                last_dir = Some(dir);
            }
            last_pt = pos;
        }
    }

    // Pin the exact end point so the stroke lands where the pen lifted,
    // even if the last decimated node fell short of it.
    let end = ctrl[n - 1];
    let need_end = points
        .last()
        .map(|p| (p[0] - end[0]).abs() > 0.01 || (p[1] - end[1]).abs() > 0.01)
        .unwrap_or(true);
    if need_end {
        points.push(end);
        widths.push(width_at(pr[n - 1]));
    }

    // Causal width smoothing only — positions are already smooth by
    // construction (centripetal spline + pre-smoothed controls), so no
    // extra Gaussian pass is applied to the sparse spine (it would round
    // deliberate corners across the few decimated nodes).
    smooth_widths_causal(&mut widths);

    StrokeCache { points, widths }
}

#[cfg(test)]
mod spine_tests {
    use super::*;

    fn sample(x: f32, y: f32, p: f32) -> PenSample {
        PenSample { pos: [x, y], pressure: p, tilt: [0.0, 0.0] }
    }

    #[test]
    fn empty_and_single_sample() {
        let c = build_spine(&[], 6.0, 3);
        assert!(c.points.is_empty() && c.widths.is_empty());

        let c = build_spine(&[sample(10.0, 10.0, 1.0)], 6.0, 3);
        assert_eq!(c.points.len(), 1);
        assert_eq!(c.points[0], [10.0, 10.0]);
        assert!(c.widths[0] > 0.0);
    }

    #[test]
    fn straight_line_spacing_and_endpoints() {
        // Straight horizontal drag: nodes should be spaced ~max(radius*0.3,1)
        // apart, monotone in x, and land on the endpoints.
        let samples: Vec<PenSample> = (0..=20).map(|i| sample(i as f32 * 5.0, 0.0, 1.0)).collect();
        let base_width = 6.0;
        let c = build_spine(&samples, base_width, 0);
        assert!(c.points.len() >= 2);
        // First and last cache points land on the drag ends.
        assert!((c.points[0][0] - 0.0).abs() < 0.5);
        assert!((c.points.last().unwrap()[0] - 100.0).abs() < 0.5);
        // Monotone non-decreasing x (a straight rightward drag never backs up).
        for w in c.points.windows(2) {
            assert!(w[1][0] >= w[0][0] - 0.01, "x went backwards: {:?}", w);
        }
        // Interior spacing is bounded below by the node spacing (minus slack
        // for the dense-eval granularity).
        let spacing = (base_width * 0.5 * 0.3).max(1.0);
        let mut gaps = 0;
        for w in c.points.windows(2) {
            let d = ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt();
            if d > 0.01 {
                assert!(d <= spacing + base_width, "gap too large: {d}");
                gaps += 1;
            }
        }
        assert!(gaps > 0);
    }

    #[test]
    fn deterministic() {
        let samples: Vec<PenSample> =
            (0..30).map(|i| sample(i as f32 * 3.0, (i as f32 * 0.5).sin() * 20.0, 1.0)).collect();
        let a = build_spine(&samples, 8.0, 3);
        let b = build_spine(&samples, 8.0, 3);
        assert_eq!(a.points, b.points);
        assert_eq!(a.widths, b.widths);
    }

    #[test]
    fn sharp_corner_emits_extra_node() {
        // An L-shape with a hard 90° turn must place a node near the corner
        // (turn-angle trigger), so a straight-line-only decimation can't skip
        // it. Compare node count against a straight line of the same length.
        let mut corner = Vec::new();
        for i in 0..=10 {
            corner.push(sample(i as f32 * 10.0, 0.0, 1.0));
        }
        for i in 1..=10 {
            corner.push(sample(100.0, i as f32 * 10.0, 1.0));
        }
        let straight: Vec<PenSample> = (0..=20).map(|i| sample(i as f32 * 10.0, 0.0, 1.0)).collect();
        let c = build_spine(&corner, 6.0, 0);
        let s = build_spine(&straight, 6.0, 0);
        // The corner path bends 90°, so it needs more nodes than a straight
        // path of equal arc length.
        assert!(
            c.points.len() > s.points.len(),
            "corner {} should exceed straight {}",
            c.points.len(),
            s.points.len()
        );
    }
}
