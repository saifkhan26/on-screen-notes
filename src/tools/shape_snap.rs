//! Shape snap: hold the pen still at the end of a stroke and it turns
//! into the shape it was meant to be — a straight line, a triangle, a
//! rectangle (turned or not) or another polygon of up to six sides, or an
//! ellipse or circle.
//!
//! Pure geometry over the stroke's points; the pen tool runs it after the
//! hold and morphs the stroke onto the result (`ActiveTool::tick_snap`).
//! The result stays a stroke, drawn with the same brush.
//!
//! How a shape is told:
//!   * open strokes can only become lines: the stroke must keep close to
//!     the segment between its ends;
//!   * a closed stroke (ends near each other) is fitted twice — as an
//!     ellipse, from its spread along its main axes, and as a polygon,
//!     from its corners (Douglas–Peucker) — and the closer fit wins, if
//!     it is close enough. Shapes in between (a blob, handwriting) snap
//!     to nothing.

/// What a stroke snapped to.
#[derive(Debug, Clone, PartialEq)]
pub enum Snapped {
    Line([f32; 2], [f32; 2]),
    /// A closed polygon, corners in drawing order.
    Polygon(Vec<[f32; 2]>),
    /// Centre, radii along its own axes, turn (radians).
    Ellipse { center: [f32; 2], radii: [f32; 2], angle: f32 },
}

/// Mean distance from the stroke to the fitted shape, as a fraction of
/// the stroke's size, above which nothing snaps.
const MAX_ERROR: f32 = 0.045;
/// Ends closer than this fraction of the length make a closed stroke.
const CLOSED_GAP: f32 = 0.17;
/// Corners turning less than this (degrees) are not corners.
const MIN_CORNER: f32 = 28.0;
/// Within this many degrees of square, a quadrilateral is a rectangle,
/// and within this of level, a rectangle or ellipse is set level.
const SQUARE_TOL: f32 = 18.0;
const LEVEL_TOL: f32 = 6.0;
/// Axes closer than this ratio make an ellipse a circle.
const ROUND: f32 = 0.86;

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn len(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    len(sub(a, b))
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = sub(b, a);
    let l2 = ab[0] * ab[0] + ab[1] * ab[1];
    if l2 <= f32::EPSILON {
        return dist(p, a);
    }
    let t = (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0);
    dist(p, [a[0] + ab[0] * t, a[1] + ab[1] * t])
}

fn path_length(pts: &[[f32; 2]]) -> f32 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// `pts` resampled to `n` points evenly spaced along it.
pub fn resample(pts: &[[f32; 2]], n: usize) -> Vec<[f32; 2]> {
    if pts.is_empty() || n == 0 {
        return Vec::new();
    }
    let total = path_length(pts);
    if n == 1 || total <= f32::EPSILON {
        return vec![pts[0]; n];
    }
    let step = total / (n - 1) as f32;
    let mut out = Vec::with_capacity(n);
    out.push(pts[0]);
    // One walk along the path: `seg` is the segment holding arc length
    // `want`, which starts `walked` along.
    let (mut seg, mut walked) = (0usize, 0.0f32);
    for k in 1..n - 1 {
        let want = step * k as f32;
        while seg + 2 < pts.len() && walked + dist(pts[seg], pts[seg + 1]) < want {
            walked += dist(pts[seg], pts[seg + 1]);
            seg += 1;
        }
        let (a, b) = (pts[seg], pts[seg + 1]);
        let l = dist(a, b);
        let into = if l > 0.0 { ((want - walked) / l).clamp(0.0, 1.0) } else { 0.0 };
        out.push([a[0] + (b[0] - a[0]) * into, a[1] + (b[1] - a[1]) * into]);
    }
    out.push(pts[pts.len() - 1]);
    out
}

/// Douglas–Peucker: the points of `pts` kept when every dropped one lies
/// within `eps` of the line through the kept ones around it.
fn simplify(pts: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (mut far, mut at) = (0.0f32, 0usize);
        for i in a + 1..b {
            let d = to_segment(pts[i], pts[a], pts[b]);
            if d > far {
                far = d;
                at = i;
            }
        }
        if far > eps {
            keep[at] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// Turn at corner `b` between `a`→`b` and `b`→`c`, degrees (0 = straight on).
fn turn(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    let (u, v) = (sub(b, a), sub(c, b));
    let d = (u[0] * v[0] + u[1] * v[1]) / (len(u) * len(v)).max(f32::EPSILON);
    d.clamp(-1.0, 1.0).acos().to_degrees()
}

/// The corners of a closed loop: Douglas–Peucker, then corners that
/// hardly turn dropped, until every corner left turns by `MIN_CORNER`.
fn corners(loop_pts: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    // Split the loop at its point farthest from the start, so the start
    // (an arbitrary point, maybe mid-side) can be dropped like any other.
    let far = (0..loop_pts.len()).max_by(|&i, &j| dist(loop_pts[i], loop_pts[0]).total_cmp(&dist(loop_pts[j], loop_pts[0]))).unwrap_or(0);
    let mut a = simplify(&loop_pts[..=far], eps);
    let b = simplify(&loop_pts[far..], eps);
    a.pop();
    a.extend(b);
    a.pop(); // the end is the start again
    let mut c = a;
    loop {
        let n = c.len();
        if n < 3 {
            return c;
        }
        let weakest = (0..n).map(|i| (i, turn(c[(i + n - 1) % n], c[i], c[(i + 1) % n]))).min_by(|x, y| x.1.total_cmp(&y.1));
        match weakest {
            Some((i, t)) if t < MIN_CORNER => {
                c.remove(i);
            }
            _ => return c,
        }
    }
}

/// Mean distance from `pts` to the closed polygon `poly`.
fn polygon_error(pts: &[[f32; 2]], poly: &[[f32; 2]]) -> f32 {
    let n = poly.len();
    pts.iter()
        .map(|&p| (0..n).map(|i| to_segment(p, poly[i], poly[(i + 1) % n])).fold(f32::INFINITY, f32::min))
        .sum::<f32>()
        / pts.len().max(1) as f32
}

/// Fit an ellipse to a closed loop of evenly spaced points: centre at
/// their mean, axes along their spread, radii from it. Returns it with
/// the mean distance of the points to it.
fn fit_ellipse(pts: &[[f32; 2]]) -> (Snapped, f32) {
    let n = pts.len().max(1) as f32;
    let c = pts.iter().fold([0.0, 0.0], |s, p| [s[0] + p[0] / n, s[1] + p[1] / n]);
    let (mut sxx, mut syy, mut sxy) = (0.0f32, 0.0f32, 0.0f32);
    for p in pts {
        let (x, y) = (p[0] - c[0], p[1] - c[1]);
        sxx += x * x / n;
        syy += y * y / n;
        sxy += x * y / n;
    }
    let mut angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (s, co) = angle.sin_cos();
    let local = |p: [f32; 2]| {
        let (x, y) = (p[0] - c[0], p[1] - c[1]);
        [co * x + s * y, -s * x + co * y]
    };
    let (mut uu, mut vv) = (0.0f32, 0.0f32);
    for p in pts {
        let q = local(*p);
        uu += q[0] * q[0] / n;
        vv += q[1] * q[1] / n;
    }
    // Evenly spread round an ellipse, the mean square along an axis is
    // half the radius squared.
    let (mut a, mut b) = ((2.0 * uu).sqrt().max(1e-3), (2.0 * vv).sqrt().max(1e-3));
    let err = pts
        .iter()
        .map(|p| {
            let q = local(*p);
            let r = ((q[0] / a).powi(2) + (q[1] / b).powi(2)).sqrt().max(1e-6);
            // Along the ray from the centre: |q| · |1 − 1/r|.
            len(q) * (1.0 - 1.0 / r).abs()
        })
        .sum::<f32>()
        / n;
    if b / a > ROUND && a / b > ROUND {
        let r = (a + b) * 0.5;
        (a, b, angle) = (r, r, 0.0);
    } else {
        angle = level(angle);
    }
    (Snapped::Ellipse { center: c, radii: [a, b], angle }, err)
}

/// `angle` set to the nearest quarter turn when within `LEVEL_TOL` of one.
fn level(angle: f32) -> f32 {
    let q = std::f32::consts::FRAC_PI_2;
    let near = (angle / q).round() * q;
    if (angle - near).abs().to_degrees() < LEVEL_TOL { near } else { angle }
}

/// A quadrilateral with corners near square, as the rectangle that fits
/// it: turned along its longest side, sized by its corners.
fn as_rectangle(quad: &[[f32; 2]]) -> Option<Vec<[f32; 2]>> {
    if quad.len() != 4 {
        return None;
    }
    let square = (0..4).all(|i| (turn(quad[(i + 3) % 4], quad[i], quad[(i + 1) % 4]) - 90.0).abs() < SQUARE_TOL);
    if !square {
        return None;
    }
    let longest = (0..4).max_by(|&i, &j| dist(quad[i], quad[(i + 1) % 4]).total_cmp(&dist(quad[j], quad[(j + 1) % 4])))?;
    let d = sub(quad[(longest + 1) % 4], quad[longest]);
    let angle = level(d[1].atan2(d[0]));
    let (s, c) = angle.sin_cos();
    let c0 = quad.iter().fold([0.0, 0.0], |a, p| [a[0] + p[0] * 0.25, a[1] + p[1] * 0.25]);
    let local = |p: [f32; 2]| {
        let (x, y) = (p[0] - c0[0], p[1] - c0[1]);
        [c * x + s * y, -s * x + c * y]
    };
    // Each side halfway between the two corners on it.
    let mut by_x: Vec<f32> = quad.iter().map(|p| local(*p)[0]).collect();
    let mut by_y: Vec<f32> = quad.iter().map(|p| local(*p)[1]).collect();
    by_x.sort_by(f32::total_cmp);
    by_y.sort_by(f32::total_cmp);
    let (x0, x1) = ((by_x[0] + by_x[1]) * 0.5, (by_x[2] + by_x[3]) * 0.5);
    let (y0, y1) = ((by_y[0] + by_y[1]) * 0.5, (by_y[2] + by_y[3]) * 0.5);
    let world = |q: [f32; 2]| [c0[0] + c * q[0] - s * q[1], c0[1] + s * q[0] + c * q[1]];
    // In the drawing's own order round, starting nearest its first corner.
    let mut rect = vec![world([x0, y0]), world([x1, y0]), world([x1, y1]), world([x0, y1])];
    if signed_area(&rect).signum() != signed_area(quad).signum() {
        rect.reverse();
    }
    let first = (0..4).min_by(|&i, &j| dist(rect[i], quad[0]).total_cmp(&dist(rect[j], quad[0])))?;
    rect.rotate_left(first);
    Some(rect)
}

fn signed_area(poly: &[[f32; 2]]) -> f32 {
    let n = poly.len();
    (0..n).map(|i| poly[i][0] * poly[(i + 1) % n][1] - poly[(i + 1) % n][0] * poly[i][1]).sum::<f32>() * 0.5
}

/// The shape `pts` (a stroke's points, in drawing order) was meant to be,
/// if it is clearly one; `min_len` is the shortest stroke that snaps.
pub fn recognise(pts: &[[f32; 2]], min_len: f32) -> Option<Snapped> {
    if pts.len() < 2 {
        return None;
    }
    let length = path_length(pts);
    if length < min_len {
        return None;
    }
    let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
    for p in pts {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size = dist(lo, hi).max(1e-3);
    let even = resample(pts, 96);
    let (first, last) = (pts[0], pts[pts.len() - 1]);

    if dist(first, last) > CLOSED_GAP * length {
        // Open: a straight line, or nothing.
        let err = even.iter().map(|p| to_segment(*p, first, last)).sum::<f32>() / even.len() as f32;
        let straight = dist(first, last) > 0.9 * length;
        return (straight && err / size < MAX_ERROR).then_some(Snapped::Line(first, last));
    }

    // Closed: the better of an ellipse and a polygon.
    let mut loop_pts = even.clone();
    loop_pts.push(even[0]);
    let (ellipse, e_err) = fit_ellipse(&even);
    let poly = corners(&loop_pts, 0.06 * size);
    let polygon = (3..=6).contains(&poly.len()).then(|| {
        let shape = as_rectangle(&poly).unwrap_or_else(|| poly.clone());
        let err = polygon_error(&even, &shape);
        (Snapped::Polygon(shape), err)
    });
    let best = match polygon {
        // A polygon has to fit clearly better to beat the ellipse: a
        // round shape drawn a little lumpy is still round.
        Some((p, p_err)) if p_err < e_err * 0.85 => (p, p_err),
        _ => (ellipse, e_err),
    };
    (best.1 / size < MAX_ERROR).then_some(best.0)
}

/// Points along `shape`, about `spacing` apart, in the drawing's
/// direction round and starting nearest `start` (for a closed shape;
/// coming back to it at the end).
pub fn outline(shape: &Snapped, start: [f32; 2], clockwise: bool, spacing: f32) -> Vec<[f32; 2]> {
    let spacing = spacing.max(0.25);
    let edge = |a: [f32; 2], b: [f32; 2], out: &mut Vec<[f32; 2]>| {
        let n = ((dist(a, b) / spacing).ceil() as usize).max(1);
        for k in 0..n {
            let t = k as f32 / n as f32;
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    };
    match shape {
        Snapped::Line(a, b) => {
            let mut out = Vec::new();
            edge(*a, *b, &mut out);
            out.push(*b);
            out
        }
        Snapped::Polygon(corners) => {
            let mut c = corners.clone();
            if (signed_area(&c) > 0.0) != clockwise {
                c.reverse();
            }
            // Start on the side nearest `start`, at the point on it
            // nearest, so the morph moves the stroke's start the least.
            let n = c.len();
            let side = (0..n)
                .min_by(|&i, &j| to_segment(start, c[i], c[(i + 1) % n]).total_cmp(&to_segment(start, c[j], c[(j + 1) % n])))
                .unwrap_or(0);
            let (a, b) = (c[side], c[(side + 1) % n]);
            let ab = sub(b, a);
            let l2 = (ab[0] * ab[0] + ab[1] * ab[1]).max(f32::EPSILON);
            let t = (((start[0] - a[0]) * ab[0] + (start[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0);
            let s0 = [a[0] + ab[0] * t, a[1] + ab[1] * t];
            let mut out = Vec::new();
            edge(s0, b, &mut out);
            for k in 1..n {
                edge(c[(side + k) % n], c[(side + k + 1) % n], &mut out);
            }
            edge(a, s0, &mut out);
            out.push(s0);
            out
        }
        Snapped::Ellipse { center, radii, angle } => {
            let (s, co) = angle.sin_cos();
            let at = |t: f32| {
                let (x, y) = (radii[0] * t.cos(), radii[1] * t.sin());
                [center[0] + co * x - s * y, center[1] + s * x + co * y]
            };
            // The parameter nearest `start`.
            let t0 = (0..360)
                .map(|k| (k as f32).to_radians())
                .min_by(|&p, &q| dist(at(p), start).total_cmp(&dist(at(q), start)))
                .unwrap_or(0.0);
            // Screen y grows down, so increasing t runs clockwise.
            let dir = if clockwise { 1.0 } else { -1.0 };
            let perimeter = std::f32::consts::PI * (3.0 * (radii[0] + radii[1]) - ((3.0 * radii[0] + radii[1]) * (radii[0] + 3.0 * radii[1])).max(0.0).sqrt());
            let n = ((perimeter / spacing).ceil() as usize).max(12);
            (0..=n).map(|k| at(t0 + dir * std::f32::consts::TAU * k as f32 / n as f32)).collect()
        }
    }
}

/// Which way round a closed stroke was drawn: clockwise on screen (y
/// down) when its signed area is positive.
pub fn drawn_clockwise(pts: &[[f32; 2]]) -> bool {
    signed_area(pts) > 0.0
}

/// Overshooting ease: from 0 to 1, past it by about a tenth, and back.
pub fn spring(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (c1, c3) = (1.70158, 2.70158);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wobbly closed path through `corners` (with a little noise), or a
    /// wobbly circle.
    fn wobble(p: [f32; 2], k: usize, amp: f32) -> [f32; 2] {
        let w = ((k as f32) * 1.7).sin() * amp;
        [p[0] + w, p[1] - w * 0.6]
    }

    fn drawn_polygon(corners: &[[f32; 2]], amp: f32) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        let n = corners.len();
        for i in 0..=n {
            let (a, b) = (corners[i % n], corners[(i + 1) % n]);
            for s in 0..20 {
                let t = s as f32 / 20.0;
                out.push(wobble([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], out.len(), amp));
            }
            if i == n {
                break;
            }
        }
        out.truncate(n * 20 + 2);
        out
    }

    fn drawn_ellipse(c: [f32; 2], r: [f32; 2], amp: f32) -> Vec<[f32; 2]> {
        (0..=100)
            .map(|k| {
                let t = std::f32::consts::TAU * k as f32 / 100.0 * 1.02;
                wobble([c[0] + r[0] * t.cos(), c[1] + r[1] * t.sin()], k, amp)
            })
            .collect()
    }

    #[test]
    fn a_wobbly_rectangle_snaps_to_a_level_rectangle() {
        let pts = drawn_polygon(&[[0.0, 0.0], [200.0, 3.0], [198.0, 120.0], [-2.0, 118.0]], 2.0);
        match recognise(&pts, 20.0) {
            Some(Snapped::Polygon(c)) => {
                assert_eq!(c.len(), 4);
                // Level: two corners share each y, two each x.
                assert!((c[0][1] - c[1][1]).abs() < 0.5 && (c[2][1] - c[3][1]).abs() < 0.5, "{c:?}");
                assert!((c[0][0] - c[3][0]).abs() < 0.5 && (c[1][0] - c[2][0]).abs() < 0.5, "{c:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_triangle_and_a_turned_square_keep_their_corners() {
        let tri = drawn_polygon(&[[0.0, 100.0], [60.0, 0.0], [120.0, 100.0]], 1.5);
        assert!(matches!(recognise(&tri, 20.0), Some(Snapped::Polygon(c)) if c.len() == 3));
        let turned = drawn_polygon(&[[50.0, 0.0], [100.0, 50.0], [50.0, 100.0], [0.0, 50.0]], 1.5);
        match recognise(&turned, 20.0) {
            Some(Snapped::Polygon(c)) => {
                assert_eq!(c.len(), 4);
                let d = sub(c[1], c[0]);
                assert!((d[1].atan2(d[0]).to_degrees().abs() % 90.0 - 45.0).abs() < 8.0, "still turned 45°: {c:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn round_things_snap_to_circles_and_ellipses() {
        match recognise(&drawn_ellipse([100.0, 100.0], [80.0, 76.0], 2.0), 20.0) {
            Some(Snapped::Ellipse { radii, .. }) => assert_eq!(radii[0], radii[1], "near round becomes a circle"),
            other => panic!("{other:?}"),
        }
        match recognise(&drawn_ellipse([0.0, 0.0], [120.0, 50.0], 2.0), 20.0) {
            Some(Snapped::Ellipse { radii, angle, .. }) => {
                assert!((radii[0] - 120.0).abs() < 12.0 && (radii[1] - 50.0).abs() < 8.0, "{radii:?}");
                assert_eq!(angle, 0.0, "nearly level is level");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_straight_stroke_is_a_line_and_scribbles_are_nothing() {
        let line: Vec<[f32; 2]> = (0..50).map(|k| wobble([k as f32 * 4.0, k as f32 * 2.0], k, 1.0)).collect();
        assert!(matches!(recognise(&line, 20.0), Some(Snapped::Line(..))));
        // An S curve, a zigzag, a short tick.
        let s: Vec<[f32; 2]> = (0..80).map(|k| { let t = k as f32 / 79.0 * 6.0; [t * 30.0, (t).sin() * 40.0] }).collect();
        assert_eq!(recognise(&s, 20.0), None);
        let zig: Vec<[f32; 2]> = (0..9).map(|k| [k as f32 * 20.0, if k % 2 == 0 { 0.0 } else { 30.0 }]).collect();
        assert_eq!(recognise(&resample(&zig, 80), 20.0), None);
        assert_eq!(recognise(&[[0.0, 0.0], [5.0, 1.0]], 20.0), None);
        // A blob: neither round nor cornered enough.
        let blob: Vec<[f32; 2]> = (0..=100)
            .map(|k| { let t = std::f32::consts::TAU * k as f32 / 100.0; let r = 60.0 + 25.0 * (3.0 * t).sin(); [r * t.cos(), r * t.sin()] })
            .collect();
        assert_eq!(recognise(&blob, 20.0), None);
    }

    #[test]
    fn the_outline_starts_where_the_stroke_did_and_closes() {
        let rect = Snapped::Polygon(vec![[0.0, 0.0], [100.0, 0.0], [100.0, 50.0], [0.0, 50.0]]);
        let out = outline(&rect, [40.0, -2.0], true, 5.0);
        assert!(dist(out[0], [40.0, 0.0]) < 1e-3 && dist(*out.last().unwrap(), out[0]) < 1e-3);
        assert!(out.windows(2).all(|w| dist(w[0], w[1]) <= 5.01));
        // Clockwise on screen: from the top side, it heads right.
        assert!(out[1][0] > out[0][0]);
        let anti = outline(&rect, [40.0, -2.0], false, 5.0);
        assert!(anti[1][0] < anti[0][0]);
        let circle = Snapped::Ellipse { center: [0.0, 0.0], radii: [50.0, 50.0], angle: 0.0 };
        let out = outline(&circle, [0.0, 52.0], true, 4.0);
        assert!(dist(out[0], [0.0, 50.0]) < 1.0 && dist(*out.last().unwrap(), out[0]) < 1e-3);
    }

    #[test]
    fn the_spring_starts_at_0_overshoots_and_settles_at_1() {
        assert!(spring(0.0).abs() < 1e-6);
        assert!((spring(1.0) - 1.0).abs() < 1e-6);
        let peak = (0..=100).map(|k| spring(k as f32 / 100.0)).fold(0.0, f32::max);
        assert!(peak > 1.05 && peak < 1.15, "{peak}");
    }
}
