//! `picture-vectors.bin` in the data folder: the vector of every picture
//! read so far (`canvas::picture_vectors`), so each is read once ever.
//!
//! Binary, added to as vectors come in, never rewritten in place:
//!   * `OSNVEC01`, then the model's name (u16 length + UTF-8) and how many
//!     numbers a vector has (u32);
//!   * then one record per picture: its key (u16 length + UTF-8) and its
//!     vector as 16-bit floats (half the room, and closer than the model
//!     tells pictures apart).
//!
//! Little-endian throughout. A record cut short (the app stopped while
//! writing it) is dropped, and the file cut back to the records before it.
//! A file from another model, or not this format, is started over.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"OSNVEC01";

/// The file, open for adding to.
pub struct VectorCache {
    path: PathBuf,
    model: String,
    dim: usize,
    /// Opened on the first add.
    file: Option<File>,
    /// What to do to the file before the first add.
    start: Start,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Start {
    /// Add after what is there.
    Append,
    /// Cut it back to this many bytes (a torn record), then add.
    Cut(u64),
    /// Write it new: none, another model's, or not ours.
    Fresh,
}

impl VectorCache {
    /// The vectors kept for `model` at `path`, in the order written, and
    /// the file ready to add to.
    pub fn open(path: &Path, model: &str, dim: usize) -> (VectorCache, Vec<(String, Vec<f32>)>) {
        let bytes = std::fs::read(path).unwrap_or_default();
        let (start, kept) = read(&bytes, model, dim);
        let n = kept.len();
        if start == Start::Fresh && !bytes.is_empty() {
            log::info!("{} is for another model or unreadable: starting it over", path.display());
        }
        log::info!("{n} picture vectors read from {}", path.display());
        (VectorCache { path: path.to_path_buf(), model: model.to_string(), dim, file: None, start }, kept)
    }

    /// Keep `v` for `key`.
    pub fn add(&mut self, key: &str, v: &[f32]) -> std::io::Result<()> {
        if v.len() != self.dim || key.len() > u16::MAX as usize {
            return Err(std::io::Error::other("not a vector this file keeps"));
        }
        if self.file.is_none() {
            let mut f = match self.start {
                Start::Fresh => {
                    let mut f = File::create(&self.path)?;
                    f.write_all(&header(&self.model, self.dim))?;
                    f
                }
                Start::Cut(len) => {
                    let f = OpenOptions::new().write(true).open(&self.path)?;
                    f.set_len(len)?;
                    drop(f);
                    OpenOptions::new().append(true).open(&self.path)?
                }
                Start::Append => OpenOptions::new().append(true).open(&self.path)?,
            };
            f.flush()?;
            self.file = Some(f);
            self.start = Start::Append;
        }
        let mut rec = Vec::with_capacity(2 + key.len() + 2 * v.len());
        rec.extend_from_slice(&(key.len() as u16).to_le_bytes());
        rec.extend_from_slice(key.as_bytes());
        for x in v {
            rec.extend_from_slice(&half::f16::from_f32(*x).to_le_bytes());
        }
        self.file.as_mut().expect("opened above").write_all(&rec)
    }
}

fn header(model: &str, dim: usize) -> Vec<u8> {
    let mut h = MAGIC.to_vec();
    h.extend_from_slice(&(model.len() as u16).to_le_bytes());
    h.extend_from_slice(model.as_bytes());
    h.extend_from_slice(&(dim as u32).to_le_bytes());
    h
}

/// What `bytes` hold for `model`, and how to add to them.
fn read(bytes: &[u8], model: &str, dim: usize) -> (Start, Vec<(String, Vec<f32>)>) {
    let head = header(model, dim);
    if bytes.len() < head.len() || bytes[..head.len()] != head[..] {
        return (Start::Fresh, Vec::new());
    }
    let mut kept = Vec::new();
    let mut at = head.len();
    loop {
        if at == bytes.len() {
            return (Start::Append, kept);
        }
        let Some(rec) = record(&bytes[at..], dim) else {
            return (Start::Cut(at as u64), kept);
        };
        at += rec.0;
        kept.push(rec.1);
    }
}

/// One record at the start of `b`: its length in bytes, key and vector.
fn record(b: &[u8], dim: usize) -> Option<(usize, (String, Vec<f32>))> {
    let klen = u16::from_le_bytes(b.get(..2)?.try_into().ok()?) as usize;
    let key = std::str::from_utf8(b.get(2..2 + klen)?).ok()?.to_string();
    let body = b.get(2 + klen..2 + klen + 2 * dim)?;
    let v = body.chunks_exact(2).map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32()).collect();
    Some((2 + klen + 2 * dim, (key, v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("osn-vectors-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("picture-vectors.bin")
    }

    #[test]
    fn vectors_come_back_at_half_precision() {
        let p = temp("roundtrip");
        let _ = std::fs::remove_file(&p);
        let (mut c, kept) = VectorCache::open(&p, "m", 4);
        assert!(kept.is_empty());
        c.add("file:a.png:None", &[0.5, -0.25, 0.1, 0.0]).unwrap();
        c.add("bytes:00ff", &[1.0, 0.0, 0.0, 0.0]).unwrap();
        drop(c);
        let (mut c, kept) = VectorCache::open(&p, "m", 4);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].0, "file:a.png:None");
        assert!((kept[0].1[2] - 0.1).abs() < 1e-3);
        c.add("third", &[0.0; 4]).unwrap();
        drop(c);
        assert_eq!(VectorCache::open(&p, "m", 4).1.len(), 3, "added after, not over");
    }

    #[test]
    fn a_torn_record_is_dropped_and_cut_away() {
        let p = temp("torn");
        let _ = std::fs::remove_file(&p);
        let (mut c, _) = VectorCache::open(&p, "m", 4);
        c.add("whole", &[1.0; 4]).unwrap();
        c.add("torn", &[1.0; 4]).unwrap();
        drop(c);
        let len = std::fs::metadata(&p).unwrap().len();
        OpenOptions::new().write(true).open(&p).unwrap().set_len(len - 3).unwrap();
        let (mut c, kept) = VectorCache::open(&p, "m", 4);
        assert_eq!(kept.iter().map(|k| k.0.as_str()).collect::<Vec<_>>(), ["whole"]);
        c.add("next", &[0.5; 4]).unwrap();
        drop(c);
        let names: Vec<String> = VectorCache::open(&p, "m", 4).1.into_iter().map(|k| k.0).collect();
        assert_eq!(names, ["whole", "next"]);
    }

    #[test]
    fn another_models_file_is_started_over() {
        let p = temp("model");
        let _ = std::fs::remove_file(&p);
        let (mut c, _) = VectorCache::open(&p, "old", 4);
        c.add("a", &[1.0; 4]).unwrap();
        drop(c);
        let (mut c, kept) = VectorCache::open(&p, "new", 4);
        assert!(kept.is_empty());
        c.add("b", &[1.0; 4]).unwrap();
        drop(c);
        assert_eq!(VectorCache::open(&p, "new", 4).1.len(), 1);
        assert!(VectorCache::open(&p, "old", 4).1.is_empty());
    }
}
