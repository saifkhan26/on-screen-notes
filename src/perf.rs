//! Opt-in timing log for performance and stress testing. Set the
//! `OSN_PERF_LOG` environment variable to a file path and the app
//! appends lines to it, each stamped with seconds since start:
//!   * once a second, the frames drawn, the CPU time spent in the app's
//!     `update`, and the gaps between frames (which also cover egui's
//!     tessellation and handing the frame to the GPU), each as average /
//!     95th percentile / worst, plus what the active slide holds;
//!   * how long loading the slides took at startup;
//!   * each autosave: the time taken on the UI thread to copy the slides
//!     out, and per slide the time to write it and its size.
//!
//! Unset, nothing is measured or written.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Log {
    file: Mutex<std::fs::File>,
    start: Instant,
}

static LOG: OnceLock<Option<Log>> = OnceLock::new();

fn log() -> Option<&'static Log> {
    LOG.get_or_init(|| {
        let path = std::env::var_os("OSN_PERF_LOG").filter(|p| !p.is_empty())?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Log { file: Mutex::new(file), start: Instant::now() })
    })
    .as_ref()
}

pub fn enabled() -> bool {
    log().is_some()
}

/// Append one line to the log, if it is on.
pub fn line(text: impl std::fmt::Display) {
    if let Some(l) = log() {
        let mut f = l.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(f, "{:9.3} {text}", l.start.elapsed().as_secs_f64());
    }
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[derive(Default)]
struct Frames {
    window: Option<Instant>,
    last: Option<Instant>,
    update: Vec<f64>,
    gap: Vec<f64>,
    context: String,
}

static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// Times one `update` from its creation to its drop, so early returns
/// are covered.
pub struct FrameTimer(Option<Instant>);

/// Start timing this frame. `context` describes the scene for the next
/// summary line and is only built when the log is on.
pub fn frame(context: impl FnOnce() -> String) -> FrameTimer {
    if !enabled() {
        return FrameTimer(None);
    }
    let now = Instant::now();
    let mut guard = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    let f = guard.get_or_insert_with(Frames::default);
    if let Some(last) = f.last {
        f.gap.push(ms(now - last));
    }
    f.last = Some(now);
    f.window.get_or_insert(now);
    f.context = context();
    FrameTimer(Some(now))
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        let Some(t0) = self.0 else { return };
        let mut guard = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
        let Some(f) = guard.as_mut() else { return };
        f.update.push(ms(t0.elapsed()));
        if f.window.is_some_and(|w| w.elapsed() >= Duration::from_secs(1)) {
            let text = format!("frames {:4} | update {} | gap {} | {}", f.update.len(), stats(&mut f.update), stats(&mut f.gap), f.context);
            f.update.clear();
            f.gap.clear();
            f.window = Some(Instant::now());
            drop(guard);
            line(text);
        }
    }
}

/// "avg / p95 / max" of a set of milliseconds.
fn stats(v: &mut [f64]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    v.sort_by(f64::total_cmp);
    let avg = v.iter().sum::<f64>() / v.len() as f64;
    let p95 = v[((v.len() - 1) as f64 * 0.95).round() as usize];
    format!("avg {avg:6.2} p95 {p95:6.2} max {:7.2} ms", v[v.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic random numbers for fixtures.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }

        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next()
        }
    }

    const COLORS: [[u8; 4]; 5] = [[255, 80, 80, 255], [80, 200, 255, 255], [255, 220, 0, 255], [120, 255, 120, 255], [255, 255, 255, 255]];

    fn ink(rng: &mut Rng, strokes: usize) -> Vec<crate::canvas::stroke::Stroke> {
        use crate::canvas::stroke::{PenSample, Stroke, StrokeStyle};
        (0..strokes)
            .map(|i| {
                let mut s = Stroke::with_style(COLORS[i % COLORS.len()], rng.range(2.0, 8.0), StrokeStyle::Default);
                let (mut x, mut y, mut a) = (rng.range(0.0, 3000.0), rng.range(0.0, 2000.0), rng.range(0.0, 6.3));
                for k in 0..120 {
                    a += rng.range(-0.3, 0.3);
                    x += a.cos() * 4.0;
                    y += a.sin() * 4.0;
                    let pressure = 0.3 + 0.7 * (k as f32 * 0.05).sin().abs();
                    s.samples.push(PenSample { pos: [x, y], pressure, tilt: [0.0, 0.0] });
                }
                s
            })
            .collect()
    }

    fn shapes(rng: &mut Rng, n: usize) -> Vec<crate::canvas::shape::Shape> {
        use crate::canvas::shape::{BorderStyle, Shape};
        (0..n)
            .map(|i| {
                let a = [rng.range(0.0, 3000.0), rng.range(0.0, 2000.0)];
                let b = [a[0] + rng.range(20.0, 300.0), a[1] + rng.range(20.0, 200.0)];
                let (color, stroke_width, border) = (COLORS[i % COLORS.len()], rng.range(1.0, 6.0), BorderStyle::Solid);
                let rotation = if i % 2 == 0 { 0.0 } else { rng.range(-1.0, 1.0) };
                match i % 5 {
                    0 => Shape::Rect { a, b, color, stroke_width, border, rotation },
                    1 => Shape::Ellipse { a, b, color, stroke_width, border, rotation },
                    2 => Shape::Line { a, b, color, stroke_width, border },
                    3 => Shape::Arrow { a, b, color, stroke_width, border },
                    _ => Shape::Text { pos: a, content: format!("label {i}\nsecond line"), font_size: rng.range(14.0, 48.0), color, rotation },
                }
            })
            .collect()
    }

    /// `n` inline pictures (1600 × 900, a gradient with a little noise,
    /// like a pasted screenshot), in a grid.
    fn pictures(rng: &mut Rng, n: usize) -> Vec<crate::canvas::shape::Shape> {
        use crate::canvas::shape::{RasterKind, Shape};
        (0..n)
            .map(|i| {
                let tint = [rng.range(0.0, 255.0), rng.range(0.0, 255.0), rng.range(0.0, 255.0)];
                let img = image::RgbaImage::from_fn(1600, 900, |x, y| {
                    let g = (x as f32 / 1600.0 + y as f32 / 900.0) * 0.5;
                    let n = rng.range(-3.0, 3.0);
                    image::Rgba([(tint[0] * g + n) as u8, (tint[1] * (1.0 - g) + n) as u8, (tint[2] * 0.5 + n) as u8, 255])
                });
                let mut png = Vec::new();
                img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
                let (col, row) = ((i % 6) as f32, (i / 6) as f32);
                Shape::Raster {
                    pos: [col * 680.0, row * 400.0],
                    size: [640.0, 360.0],
                    png: std::sync::Arc::new(png),
                    px_size: [1600, 900],
                    color: [0; 4],
                    rotation: 0.0,
                    kind: RasterKind::Image,
                    media: None,
                    picture: None,
                }
            })
            .collect()
    }

    /// Writes stress-test slides into `OSN_DATA_DIR` (a scratch folder,
    /// never the real one), after any slides already there. `OSN_STRESS`
    /// lists what to make, comma-separated: `ink:N` (a slide of N
    /// strokes), `shapes:N`, `pictures:N` (N inline pictures),
    /// `slides:N:S` (N slides of S strokes each).
    #[test]
    #[ignore]
    fn write_stress_slides() {
        assert!(std::env::var_os("OSN_DATA_DIR").is_some(), "point OSN_DATA_DIR at a scratch folder");
        let spec = std::env::var("OSN_STRESS").expect("set OSN_STRESS, e.g. ink:5000,shapes:3000");
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut next = crate::persistence::max_canvas_index().map_or(0, |m| m + 1);
        let mut save = |strokes, shapes| {
            let mut c = crate::canvas::canvas::Canvas::default();
            c.layers[0].strokes = strokes;
            c.layers[0].shapes = shapes;
            crate::persistence::save_canvas(next, &c).unwrap();
            println!("wrote slide {}", next + 1);
            next += 1;
        };
        for item in spec.split(',') {
            let parts: Vec<&str> = item.trim().split(':').collect();
            let num = |i: usize| parts.get(i).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1000);
            match parts[0] {
                "ink" => save(ink(&mut rng, num(1)), Vec::new()),
                "shapes" => save(Vec::new(), shapes(&mut rng, num(1))),
                "pictures" => save(Vec::new(), pictures(&mut rng, num(1))),
                "slides" => {
                    for _ in 0..num(1) {
                        save(ink(&mut rng, num(2)), Vec::new());
                    }
                }
                other => panic!("unknown stress item {other:?}"),
            }
        }
    }

    #[test]
    fn stats_read_average_percentile_and_worst() {
        let mut v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(stats(&mut v), "avg  50.50 p95  95.00 max  100.00 ms");
        assert_eq!(stats(&mut []), "-");
    }
}
