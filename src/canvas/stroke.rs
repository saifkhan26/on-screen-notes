//! `PenSample` and `Stroke` — the raw building blocks of an ink stroke.
//!
//! A `PenSample` is one tick of pen telemetry: position, pressure, optional
//! tilt. A `Stroke` is the ordered list of samples that make up one drag
//! from pen-down to pen-up, plus the visual properties (color, base width).
//!
//! Why pressure is `f32` 0..=1: every backend normalises into this range so
//! we never have to worry about device-specific maximum levels (1024, 4096, …).

use serde::{Deserialize, Serialize};

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
    pub fn new(color: [u8; 4], base_width: f32) -> Self {
        Self::with_style(color, base_width, StrokeStyle::Default)
    }

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
    pub fn push(&mut self, mut sample: PenSample) {
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
        if self.cache.is_some() { return; }
        // Hard ceiling: the slider + scroll handler already constrain
        // `base_width` to ≤ 80 px, but a stroke loaded from a corrupt /
        // older RON file could carry an arbitrary value. Clamp here so the
        // renderer never has to paint a 10 000-px-wide quad.
        const MAX_WIDTH: f32 = 80.0;
        let base_width = self.base_width.clamp(0.5, MAX_WIDTH);
        let curve = pressure_curve();

        let n = self.samples.len();
        if n == 0 {
            self.cache = Some(StrokeCache { points: Vec::new(), widths: Vec::new() });
            return;
        }
        if n == 1 {
            // A single tap: cache contains just that one point.
            let s = self.samples[0];
            let w = (base_width * apply_curve(s.pressure, curve).max(0.1)).max(0.8);
            self.cache = Some(StrokeCache { points: vec![s.pos], widths: vec![w] });
            return;
        }

        // ---- Pre-smooth raw sample positions ----------------------
        // We run the symmetric MA passes here, on the raw control
        // points, *before* Catmull-Rom subdivision. Smoothing strength
        // (effective kernel σ ≈ √passes) is then expressed in
        // sample-space — independent of the adaptive substep count
        // below — so a "High" smoothing level actually feels heavier
        // than "Low", which was the bug the user reported.
        let mut ctrl: Vec<[f32; 2]> = self.samples.iter().map(|s| s.pos).collect();
        for _ in 0..position_passes {
            smooth_positions_in_place(&mut ctrl);
        }

        let mut points: Vec<[f32; 2]> = Vec::with_capacity(n * 8);
        let mut widths: Vec<f32>      = Vec::with_capacity(n * 8);

        // ---- Quadratic Bezier through midpoints --------------------
        // For each sample p_i, draw a quadratic Bezier from
        // m(p_{i-1}, p_i) to m(p_i, p_{i+1}) using p_i as the control
        // point. Endpoints clamp m(-1, 0) := p_0 and m(n-1, n) := p_{n-1}.
        //
        // Why not Catmull-Rom: the spline passes through every sample
        // *and* overshoots at sharp curvature peaks, producing the
        // small bumps the user sees at hand-drawn turning points. The
        // midpoint quadratic instead anchors at midpoints (which sit
        // inside the convex hull of consecutive samples) and curves
        // toward the actual sample as control. No overshoot. This is
        // the technique Windows Sticky Notes, Krita freehand, and
        // Adobe sketch apps use.
        //
        // Trade-off: rendered curve does *not* exactly pass through
        // samples — it can sit up to one inter-sample gap inside the
        // turn. At our Wintab sample rate (≈200 Hz) this is sub-pixel
        // for slow motion and perceptually invisible for fast motion.
        let midpoint = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
            [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
        };

        for i in 0..n {
            let p_pos = ctrl[i];
            let p_pr  = apply_curve(self.samples[i].pressure, curve);
            let m_start = if i == 0 { p_pos } else { midpoint(ctrl[i - 1], p_pos) };
            let m_end   = if i + 1 == n { p_pos } else { midpoint(p_pos, ctrl[i + 1]) };
            let pr_start = if i == 0 {
                p_pr
            } else {
                0.5 * (apply_curve(self.samples[i - 1].pressure, curve) + p_pr)
            };
            let pr_end = if i + 1 == n {
                p_pr
            } else {
                0.5 * (p_pr + apply_curve(self.samples[i + 1].pressure, curve))
            };

            // Adaptive substep count based on chord length of this
            // curve segment. Polyline gap stays well below half a
            // stroke width so the ribbon mesh has no visible scallop
            // at fast pen speeds.
            let dx = m_end[0] - m_start[0];
            let dy = m_end[1] - m_start[1];
            let chord = (dx * dx + dy * dy).sqrt();
            let avg_w = base_width * 0.5 * (pr_start + pr_end).max(0.1);
            let target_gap = (avg_w * 0.4).max(2.0);
            let substeps = ((chord / target_gap).ceil() as usize).clamp(2, 24);

            // Push step 0 only for the very first segment so adjacent
            // segments don't duplicate their shared midpoint.
            let start = if i == 0 { 0 } else { 1 };
            for step in start..=substeps {
                let t = step as f32 / substeps as f32;
                let u = 1.0 - t;
                let pos = [
                    u * u * m_start[0] + 2.0 * u * t * p_pos[0] + t * t * m_end[0],
                    u * u * m_start[1] + 2.0 * u * t * p_pos[1] + t * t * m_end[1],
                ];
                let pressure = u * u * pr_start + 2.0 * u * t * p_pr + t * t * pr_end;
                let w = (base_width * pressure.clamp(0.05, 1.0)).max(0.8);
                points.push(pos);
                widths.push(w);
            }
        }

        // Position passes already ran on raw samples above. Widths
        // get a single causal pass here so a pressure rise thickens
        // the stroke *where it happens* rather than ramping toward it.
        smooth_widths_causal(&mut widths);

        self.cache = Some(StrokeCache { points, widths });
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
