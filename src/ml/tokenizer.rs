//! The text model's tokenizer: Gemma's SentencePiece BPE, 256,000
//! pieces, read from `tokenizer.model` (a protobuf `ModelProto`). It
//! follows SentencePiece's own `bpe_model.cc`, and gives the ids Hugging
//! Face's tokenizer gives (checked by `the_ids_match_hugging_face`):
//!
//! * spaces become `▁`; nothing else is changed (the model's normaliser
//!   is "identity", with no dummy prefix and spaces kept as typed);
//! * the text is cut into characters, except that a user-defined or
//!   special piece (`<mask>`, `<unused5>`, `<eos>`, …) is taken whole and
//!   never merged — Hugging Face reads typed special pieces as such;
//! * the neighbouring pair whose joined piece scores highest is merged,
//!   leftmost first on a tie, until no joined pair is a piece;
//! * a piece not in the vocabulary is spelled out as its UTF-8 bytes
//!   (`<0x41>`…).
//!
//! The model reads 64 ids: the pieces, `<eos>`, then `<pad>` to the end.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Ids the text model reads.
pub const LEN: usize = 64;

/// SentencePiece piece types (`sentencepiece_model.proto`).
const NORMAL: u64 = 1;
const UNKNOWN: u64 = 2;
const CONTROL: u64 = 3;
const USER_DEFINED: u64 = 4;
const UNUSED: u64 = 5;
const BYTE: u64 = 6;

pub struct Tokenizer {
    /// Pieces that merges can make, and user-defined ones: id and score.
    pieces: HashMap<Box<str>, (u32, f32)>,
    /// User-defined and special pieces, longest first: taken whole.
    user: Vec<Box<str>>,
    /// Special pieces (`<eos>`, `<unk>`, …): id.
    specials: HashMap<Box<str>, u32>,
    /// The `<0xNN>` piece of each byte.
    bytes: [u32; 256],
    eos: u32,
    pad: u32,
}

impl Tokenizer {
    /// Read a `tokenizer.model`.
    pub fn parse(model: &[u8]) -> Result<Tokenizer, String> {
        let mut pieces = HashMap::new();
        let mut reserved = HashMap::new();
        let mut user = Vec::new();
        let mut bytes = [u32::MAX; 256];
        let mut id = 0u32;
        for field in Fields(model) {
            let (1, Wire::Bytes(p)) = field? else { continue };
            let (mut text, mut score, mut kind) = (String::new(), 0.0f32, NORMAL);
            for f in Fields(p) {
                match f? {
                    (1, Wire::Bytes(b)) => text = std::str::from_utf8(b).map_err(|_| "a piece is not UTF-8")?.to_string(),
                    (2, Wire::Fixed32(b)) => score = f32::from_le_bytes(b),
                    (3, Wire::Varint(v)) => kind = v,
                    _ => {}
                }
            }
            match kind {
                NORMAL | USER_DEFINED | UNUSED => {
                    if kind == USER_DEFINED {
                        user.push(text.clone().into_boxed_str());
                    }
                    pieces.insert(text.into_boxed_str(), (id, score));
                }
                BYTE => {
                    let hex = text.strip_prefix("<0x").and_then(|t| t.strip_suffix('>')).ok_or("a byte piece is not <0xNN>")?;
                    let b = u8::from_str_radix(hex, 16).map_err(|_| "a byte piece is not <0xNN>")?;
                    bytes[b as usize] = id;
                }
                UNKNOWN | CONTROL => {
                    user.push(text.clone().into_boxed_str());
                    reserved.insert(text.into_boxed_str(), id);
                }
                _ => {}
            }
            id += 1;
        }
        if bytes.contains(&u32::MAX) {
            return Err("the model lacks byte pieces".into());
        }
        let find = |name: &str| reserved.get(name).copied().ok_or_else(|| format!("the model has no {name}"));
        user.sort_by_key(|u| std::cmp::Reverse(u.len()));
        let (eos, pad) = (find("<eos>")?, find("<pad>")?);
        find("<unk>")?;
        Ok(Tokenizer { pieces, user, specials: reserved, bytes, eos, pad })
    }

    /// The ids of `text`'s pieces.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let text = text.replace(' ', "\u{2581}");
        // Cut into characters, or user-defined pieces taken whole.
        let mut syms: Vec<Sym> = Vec::new();
        let mut at = 0;
        while at < text.len() {
            let rest = &text[at..];
            let (len, frozen) = match self.user.iter().find(|u| rest.starts_with(&***u)) {
                Some(u) => (u.len(), true),
                None => (rest.chars().next().map_or(1, char::len_utf8), false),
            };
            let i = syms.len();
            syms.push(Sym { start: at, end: at + len, prev: i.checked_sub(1), next: None, frozen });
            if i > 0 {
                syms[i - 1].next = Some(i);
            }
            at += len;
        }
        let mut agenda = BinaryHeap::new();
        let pair = |syms: &[Sym], agenda: &mut BinaryHeap<Pair>, left: Option<usize>, right: Option<usize>| {
            let (Some(l), Some(r)) = (left, right) else { return };
            if syms[l].frozen || syms[r].frozen {
                return;
            }
            if let Some(&(_, score)) = self.pieces.get(&text[syms[l].start..syms[r].end]) {
                agenda.push(Pair { score, left: l, right: r, size: syms[r].end - syms[l].start });
            }
        };
        for i in 1..syms.len() {
            pair(&syms, &mut agenda, Some(i - 1), Some(i));
        }
        while let Some(p) = agenda.pop() {
            let (l, r) = (p.left, p.right);
            // Gone stale: one side has been merged into something else.
            if syms[l].start == syms[l].end || syms[r].start == syms[r].end || syms[r].end - syms[l].start != p.size || syms[l].next != Some(r) {
                continue;
            }
            syms[l].end = syms[r].end;
            syms[l].next = syms[r].next;
            if let Some(n) = syms[r].next {
                syms[n].prev = Some(l);
            }
            syms[r].end = syms[r].start;
            pair(&syms, &mut agenda, syms[l].prev, Some(l));
            pair(&syms, &mut agenda, Some(l), syms[l].next);
        }
        let mut ids = Vec::new();
        let mut at = if syms.is_empty() { None } else { Some(0) };
        while let Some(i) = at {
            let piece = &text[syms[i].start..syms[i].end];
            match self.pieces.get(piece).map(|p| p.0).or_else(|| self.specials.get(piece).copied()) {
                Some(id) => ids.push(id),
                None => ids.extend(piece.bytes().map(|b| self.bytes[b as usize])),
            }
            at = syms[i].next;
        }
        ids
    }

    /// What the text model reads for `text`: its pieces (at most `LEN`
    /// less one), `<eos>`, then `<pad>` up to `LEN`.
    pub fn model_ids(&self, text: &str) -> Vec<i64> {
        let mut ids: Vec<i64> = self.encode(text).into_iter().take(LEN - 1).map(i64::from).collect();
        ids.push(self.eos.into());
        ids.resize(LEN, self.pad.into());
        ids
    }
}

struct Sym {
    /// Where it is in the text, in bytes; empty once merged away.
    start: usize,
    end: usize,
    prev: Option<usize>,
    next: Option<usize>,
    /// A user-defined piece: never merged.
    frozen: bool,
}

/// A merge that may be made.
struct Pair {
    score: f32,
    left: usize,
    right: usize,
    /// Bytes the merged piece spans, to tell a stale pair.
    size: usize,
}

// Highest score first; on a tie the leftmost.
impl Ord for Pair {
    fn cmp(&self, o: &Self) -> Ordering {
        self.score.total_cmp(&o.score).then(o.left.cmp(&self.left))
    }
}
impl PartialOrd for Pair {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl PartialEq for Pair {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Pair {}

/// A protobuf field's value.
enum Wire<'a> {
    Varint(u64),
    Fixed64,
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

/// The fields of a protobuf message, as `(number, value)`.
struct Fields<'a>(&'a [u8]);

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Wire<'a>), String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        Some(self.field())
    }
}

impl<'a> Fields<'a> {
    fn field(&mut self) -> Result<(u64, Wire<'a>), String> {
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Wire::Fixed64
            }
            2 => {
                let n = self.varint()? as usize;
                Wire::Bytes(self.take(n)?)
            }
            5 => Wire::Fixed32(self.take(4)?.try_into().expect("4 bytes")),
            w => return Err(format!("protobuf wire type {w} is not read here")),
        };
        Ok((key >> 3, value))
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = self.0.split_first().ok_or("protobuf cut short")?;
            self.0 = rest;
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err("protobuf varint too long".into())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.0.len() {
            return Err("protobuf cut short".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ModelProto` with these pieces: `(text, score, type)`.
    fn model(pieces: &[(&str, f32, u64)]) -> Vec<u8> {
        let varint = |out: &mut Vec<u8>, mut v: u64| loop {
            if v < 0x80 {
                out.push(v as u8);
                break;
            }
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        };
        let mut out = Vec::new();
        for (text, score, kind) in pieces {
            let mut p = Vec::new();
            p.push(0x0a);
            varint(&mut p, text.len() as u64);
            p.extend_from_slice(text.as_bytes());
            p.push(0x15);
            p.extend_from_slice(&score.to_le_bytes());
            p.push(0x18);
            varint(&mut p, *kind);
            out.push(0x0a);
            varint(&mut out, p.len() as u64);
            out.extend(p);
        }
        out
    }

    fn tiny() -> Tokenizer {
        let mut pieces: Vec<(String, f32, u64)> = vec![
            ("<pad>".into(), 0.0, CONTROL),
            ("<eos>".into(), 0.0, CONTROL),
            ("<unk>".into(), 0.0, UNKNOWN),
            ("<mask>".into(), 0.0, USER_DEFINED),
        ];
        pieces.extend((0..=255u8).map(|b| (format!("<0x{b:02X}>"), 0.0, BYTE)));
        // 260…: characters, then merges by score.
        for (t, s) in [("a", -1.0), ("b", -1.0), ("c", -1.0), ("\u{2581}", -1.0), ("ab", -2.0), ("bc", -3.0), ("abc", -4.0), ("\u{2581}a", -5.0)] {
            pieces.push((t.into(), s, NORMAL));
        }
        let refs: Vec<(&str, f32, u64)> = pieces.iter().map(|(t, s, k)| (t.as_str(), *s, *k)).collect();
        Tokenizer::parse(&model(&refs)).unwrap()
    }

    #[test]
    fn merges_by_score_and_falls_back_to_bytes() {
        let t = tiny();
        // a b c: "ab" scores higher than "bc", then "abc".
        assert_eq!(t.encode("abc"), [266]);
        // "ab" outscores "▁a", and nothing joins "▁" to "ab".
        assert_eq!(t.encode(" ab"), [263, 264]);
        assert_eq!(t.encode(" a"), [267]);
        // "z" is no piece: its byte.
        assert_eq!(t.encode("z"), [4 + b'z' as u32]);
        // A user-defined or special piece is taken whole and not merged.
        assert_eq!(t.encode("a<mask>b"), [260, 3, 261]);
        assert_eq!(t.encode("<eos>a"), [1, 260]);
        assert!(t.encode("").is_empty());
    }

    #[test]
    fn the_model_reads_pieces_then_eos_then_padding() {
        let t = tiny();
        let ids = t.model_ids("abc");
        assert_eq!(ids.len(), LEN);
        assert_eq!(&ids[..3], &[266, 1, 0]);
        assert!(ids[2..].iter().all(|&i| i == 0));
        let long = t.model_ids(&"z".repeat(200));
        assert_eq!((long.len(), long[LEN - 1]), (LEN, 1), "cut to fit, eos last");
    }

    /// Needs `tokenizer.model` in `OSN_MODEL_DIR` (picture search fetches it).
    #[test]
    #[ignore = "needs tokenizer.model in OSN_MODEL_DIR"]
    fn the_ids_match_hugging_face() {
        let dir = std::path::PathBuf::from(std::env::var_os("OSN_MODEL_DIR").expect("OSN_MODEL_DIR"));
        let t = Tokenizer::parse(&std::fs::read(dir.join(super::super::models::TOKENIZER.file)).unwrap()).unwrap();
        for (text, want) in super::super::tokenizer_cases::CASES {
            assert_eq!(t.encode(text), *want, "{text:?}");
        }
    }
}
