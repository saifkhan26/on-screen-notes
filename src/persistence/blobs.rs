//! Fast reading of the picture byte lists in a slide file.
//!
//! A slide keeps each pasted picture as `png: [137, 80, 78, …]`, one
//! decimal number per byte, so a slide of pictures is almost nothing
//! but such lists: a 1.3 GB slide of a few hundred pictures is 99 %
//! numbers. ron reads them one element at a time through serde, which
//! took ~20 s for that slide. Here a plain digit loop reads them on
//! several threads, and ron reads only the rest of the file.
//!
//! Each list is cut out of the text and replaced by a short marker list
//! holding its number; ron reads that, and every picture whose bytes
//! are a marker gets its list back. Nothing is assumed about which
//! lists belong to pictures: unless every cut-out list comes back
//! exactly once as a picture's bytes (one inside a text label would
//! not), and every list is plain numbers, the result is thrown away and
//! ron reads the original text instead.

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::Shape;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// With fewer list bytes than this the plain parse is quick anyway.
const MIN_LIST_BYTES: usize = 4 << 20;
/// A marker list is these four bytes and its number (little-endian
/// `u32`). No PNG starts with 255.
const MAGIC: [u8; 4] = [255, b'O', b'S', b'N'];

/// Read a slide's text, or `None` when this path does not apply or does
/// not account for every list; the caller then parses `text` with ron.
/// `progress` counts the list bytes read so far.
pub(super) fn parse(text: &str, progress: Option<&AtomicU64>) -> Option<Canvas> {
    let lists = find_lists(text.as_bytes())?;
    let list_bytes: usize = lists.iter().map(Range::len).sum();
    if list_bytes < MIN_LIST_BYTES {
        return None;
    }
    let mut skeleton = String::with_capacity(text.len() - list_bytes + lists.len() * 40);
    let mut at = 0;
    for (i, r) in lists.iter().enumerate() {
        // Lists start after `[` and end before `]`, both ASCII, so these
        // are char boundaries.
        skeleton.push_str(&text[at..r.start]);
        let n = (i as u32).to_le_bytes();
        let marker: Vec<String> = MAGIC.iter().chain(&n).map(u8::to_string).collect();
        skeleton.push_str(&marker.join(","));
        at = r.end;
    }
    skeleton.push_str(&text[at..]);

    let mut decoded: Vec<Option<Vec<u8>>> = decode_all(text.as_bytes(), &lists, progress)?.into_iter().map(Some).collect();
    let mut canvas: Canvas = ron::from_str(&skeleton).ok()?;

    // Give every marker its list back, each exactly once.
    let mut restore = |png: &mut Vec<u8>| -> bool {
        let Some(i) = marker_index(png) else { return true };
        match decoded.get_mut(i).and_then(Option::take) {
            Some(bytes) => {
                *png = bytes;
                true
            }
            None => false,
        }
    };
    let mut ok = true;
    for layer in &mut canvas.layers {
        for shape in &mut layer.shapes {
            if let Shape::Raster { png, .. } = shape {
                ok &= restore(Arc::make_mut(png));
            }
        }
        if let Some(img) = &mut layer.image {
            ok &= restore(&mut img.png);
        }
    }
    // Old slides keep their shapes outside any layer.
    for shape in &mut canvas.legacy_shapes {
        if let Shape::Raster { png, .. } = shape {
            ok &= restore(Arc::make_mut(png));
        }
    }
    (ok && decoded.iter().all(Option::is_none)).then_some(canvas)
}

fn marker_index(png: &[u8]) -> Option<usize> {
    match png {
        [a, b, c, d, n @ ..] if [*a, *b, *c, *d] == MAGIC && n.len() == 4 => {
            Some(u32::from_le_bytes([n[0], n[1], n[2], n[3]]) as usize)
        }
        _ => None,
    }
}

/// Where the text of every `png: [ … ]` list is, between its brackets.
/// Strings are skipped so a label that reads like one is not taken for
/// one; `None` when the text ends inside a string or a list.
fn find_lists(b: &[u8]) -> Option<Vec<Range<usize>>> {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let skip_ws = |mut j: usize| {
        while b.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        j
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                loop {
                    match *b.get(i)? {
                        b'\\' => i += 2,
                        b'"' => break,
                        _ => i += 1,
                    }
                }
                i += 1;
            }
            b'p' if b[i..].starts_with(b"png") && (i == 0 || !ident(b[i - 1])) => {
                let colon = skip_ws(i + 3);
                let open = skip_ws(colon + 1);
                if b.get(colon) == Some(&b':') && b.get(open) == Some(&b'[') {
                    let start = open + 1;
                    let end = start + memchr::memchr(b']', &b[start..])?;
                    out.push(start..end);
                    i = end + 1;
                } else {
                    i += 3;
                }
            }
            _ => i += 1,
        }
    }
    Some(out)
}

/// Read every list, on several threads.
fn decode_all(b: &[u8], lists: &[Range<usize>], progress: Option<&AtomicU64>) -> Option<Vec<Vec<u8>>> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let per = lists.iter().map(Range::len).sum::<usize>() / threads + 1;
    // Runs of consecutive lists, about `per` bytes each.
    let mut runs = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, r) in lists.iter().enumerate() {
        size += r.len();
        if size >= per {
            runs.push(start..i + 1);
            (start, size) = (i + 1, 0);
        }
    }
    if start < lists.len() {
        runs.push(start..lists.len());
    }
    let done: Vec<Option<Vec<Vec<u8>>>> = std::thread::scope(|s| {
        let workers: Vec<_> = runs
            .into_iter()
            .map(|run| {
                let mine = &lists[run.clone()];
                (run, std::thread::Builder::new().spawn_scoped(s, move || decode_run(b, mine, progress)))
            })
            .collect();
        workers
            .into_iter()
            .map(|(run, worker)| match worker {
                Ok(w) => w.join().ok().flatten(),
                // No thread to be had: read this run here.
                Err(_) => decode_run(b, &lists[run], progress),
            })
            .collect()
    });
    let mut out = Vec::with_capacity(lists.len());
    for run in done {
        out.extend(run?);
    }
    Some(out)
}

fn decode_run(b: &[u8], lists: &[Range<usize>], progress: Option<&AtomicU64>) -> Option<Vec<Vec<u8>>> {
    lists
        .iter()
        .map(|r| {
            let bytes = decode(&b[r.clone()]);
            if let Some(p) = progress {
                p.fetch_add(r.len() as u64, Ordering::Relaxed);
            }
            bytes
        })
        .collect()
}

/// One list's text: numbers 0–255 apart by commas, with any spacing and
/// an optional trailing comma. Anything else (hex, signs, comments) is
/// `None`, left for ron.
fn decode(t: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(t.len() / 3);
    let mut i = 0;
    let skip_ws = |mut i: usize| {
        while i < t.len() && t[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    loop {
        i = skip_ws(i);
        if i == t.len() {
            return Some(out);
        }
        let first = i;
        let mut v: u32 = 0;
        while i < t.len() && t[i].is_ascii_digit() {
            v = v * 10 + u32::from(t[i] - b'0');
            if v > 255 {
                return None;
            }
            i += 1;
        }
        if i == first {
            return None;
        }
        out.push(v as u8);
        i = skip_ws(i);
        if i == t.len() {
            return Some(out);
        }
        if t[i] != b',' {
            return None;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::canvas::{Layer, LayerImage};

    fn picture(png: Vec<u8>) -> Shape {
        Shape::Raster {
            pos: [1.0, 2.0],
            size: [3.0, 4.0],
            png: Arc::new(png),
            px_size: [1, 1],
            color: [9, 9, 9, 255],
            rotation: 0.0,
            kind: Default::default(),
            media: None,
            picture: None,
            pin: 0,
        }
    }

    fn big(seed: u8) -> Vec<u8> {
        (0..(MIN_LIST_BYTES / 2)).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    fn slide() -> Canvas {
        let mut c = Canvas::default();
        let mut l = Layer::default();
        l.shapes.push(picture(big(1)));
        // A label that reads like a list must stay a label.
        l.shapes.push(Shape::Text { pos: [0.0, 0.0], content: "png: [1, 2, 3] and \"png: [4]\"".into(), font_size: 12.0, color: [1, 2, 3, 4], rotation: 0.0, pin: 0 });
        l.shapes.push(picture(vec![]));
        l.image = Some(LayerImage { png: big(2), size: [1, 1], logical_size: [1.0, 1.0], canvas_origin: [0.0, 0.0], capture_zoom: 1.0 });
        c.layers = vec![l, Layer::default()];
        c.layers[1].shapes.push(picture(big(3)));
        c
    }

    fn same(a: &Canvas, b: &Canvas) -> bool {
        crate::persistence::serialize_canvas(a).unwrap() == crate::persistence::serialize_canvas(b).unwrap()
    }

    #[test]
    fn reads_what_ron_reads() {
        let c = slide();
        let text = crate::persistence::serialize_canvas(&c).unwrap();
        let progress = AtomicU64::new(0);
        let fast = parse(&text, Some(&progress)).expect("the fast path applies");
        assert!(same(&fast, &ron::from_str(&text).unwrap()));
        assert!(same(&fast, &c));
        assert!(progress.load(Ordering::Relaxed) as usize > MIN_LIST_BYTES * 3);
        // Compact text too.
        let compact = ron::to_string(&c).unwrap();
        assert!(same(&parse(&compact, None).unwrap(), &c));
    }

    #[test]
    fn anything_unusual_is_left_to_ron() {
        let text = crate::persistence::serialize_canvas(&slide()).unwrap();
        // A list that is not plain bytes.
        let hex = text.replacen("png: [", "png: [0x1, ", 1);
        assert!(parse(&hex, None).is_none());
        let wide = text.replacen("png: [", "png: [256, ", 1);
        assert!(parse(&wide, None).is_none());
        // A cut-out list that is not a picture's bytes.
        let label = text.replacen("content: \"", "content: \"x\", png: [1, 2], z: \"", 1);
        assert!(parse(&label, None).is_none());
        // Small slides are not worth it.
        let mut small = Canvas::default();
        small.layers[0].shapes.push(picture(vec![1, 2, 3]));
        assert!(parse(&crate::persistence::serialize_canvas(&small).unwrap(), None).is_none());
        // Unterminated.
        assert!(parse(&text[..text.len() / 2], None).is_none());
    }

    #[test]
    fn list_text() {
        assert_eq!(decode(b""), Some(vec![]));
        assert_eq!(decode(b" 1, 22 ,255,\n 0 , "), Some(vec![1, 22, 255, 0]));
        assert_eq!(decode(b"1,,2"), None);
        assert_eq!(decode(b"1 2"), None);
        assert_eq!(decode(b"-1"), None);
        assert_eq!(decode(b"999"), None);
    }
}
