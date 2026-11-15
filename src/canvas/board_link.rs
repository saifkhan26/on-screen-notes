//! A slide's link back to the PureRef board it was opened from, so it can
//! be brought up to date with the board later ("resync from PureRef").
//!
//! The slide keeps the board's path, how board positions map onto the
//! slide, and for every board item it has had: the link id its shape
//! carries (`Shape::link`), the item's id on the board, and the item as
//! the board had it at the last sync (`ItemState`). That last part is
//! what makes a resync a three-way merge: whatever the board changed
//! since then is brought over, and whatever it did not change keeps the
//! slide's own edits.
//!
//! The board is only ever read. Saving the path does not let deleting
//! the slide touch the board: cleanup only removes the app's own files
//! in the media folder.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A slide's link to its board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardLink {
    /// The `.pur` file.
    pub path: PathBuf,
    /// Board positions onto the slide: `slide = board · scale + offset`.
    pub scale: f64,
    pub offset: [f64; 2],
    /// Every board item the slide has had, in no particular order. One
    /// whose shape the slide no longer has was deleted on the slide and
    /// stays deleted.
    #[serde(default)]
    pub items: Vec<LinkedItem>,
    /// Board group (its outermost item's id) → the slide group its
    /// members are put in, so an item PureRef moves into a group joins
    /// the slide group the others are in.
    #[serde(default)]
    pub groups: Vec<(i64, u64)>,
}

/// One board item the slide has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedItem {
    /// What its shape carries as `Shape::link`. Random, so a shape copied
    /// to another slide never matches that slide's items.
    pub link: u64,
    /// Its id on the board.
    pub key: i64,
    /// How the board had it at the last sync.
    pub base: ItemState,
    /// It is no longer on the board. Its shape stays on the slide; the
    /// media filter can highlight it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub gone: bool,
}

/// What a resync compares of a board item, in board units. Each part is
/// brought over on its own: a crop changed in PureRef does not undo a
/// move made on the slide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemState {
    /// Where it is: a picture's centre, a note's top-left.
    pub at: [f64; 2],
    /// A picture's half width and height; a note's font size, then 0.
    pub size: [f64; 2],
    /// Radians, clockwise on screen.
    pub rotation: f64,
    /// What it shows: the pixels of a picture (file, crop, mirror) or a
    /// note's text, hashed.
    pub content: u64,
    /// A note's colours, hashed; 0 for a picture.
    #[serde(default)]
    pub style: u64,
    /// The board group it moves with (`BoardLink::groups`): its
    /// outermost item's id, which can be 0. `None` when it is in none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<i64>,
    /// Names of the named PureRef groups it is in, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ItemState {
    /// Same place, size and turn, give or take float noise.
    pub fn same_place(&self, other: &ItemState) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0);
        near(self.at[0], other.at[0])
            && near(self.at[1], other.at[1])
            && near(self.size[0], other.size[0])
            && near(self.size[1], other.size[1])
            && near(self.rotation, other.rotation)
    }
}

impl BoardLink {
    /// Board point → slide point.
    pub fn to_slide(&self, p: [f64; 2]) -> [f32; 2] {
        [(p[0] * self.scale + self.offset[0]) as f32, (p[1] * self.scale + self.offset[1]) as f32]
    }

    /// The item whose shape carries `link`.
    pub fn item(&self, link: u64) -> Option<&LinkedItem> {
        if link == 0 {
            return None;
        }
        self.items.iter().find(|i| i.link == link)
    }

    /// Whether the shape with this `link` is gone from the board.
    #[cfg(test)]
    pub fn is_gone(&self, link: u64) -> bool {
        self.item(link).is_some_and(|i| i.gone)
    }

    /// The slide group board group `root` is put in, made on first use;
    /// 0 (no group) for none.
    pub fn group_for(&mut self, root: Option<i64>) -> u64 {
        let Some(root) = root else { return 0 };
        if let Some((_, g)) = self.groups.iter().find(|(r, _)| *r == root) {
            return *g;
        }
        let g = crate::canvas::shape::fresh_group_id();
        self.groups.push((root, g));
        g
    }
}

/// A fresh link id: never 0, never the same twice in a run, and spread
/// over all 64 bits (a clock-seeded counter, scrambled) so ids made in
/// different runs do not meet.
pub fn fresh_link() -> u64 {
    static NEXT: std::sync::OnceLock<std::sync::atomic::AtomicU64> = std::sync::OnceLock::new();
    let next = NEXT.get_or_init(|| {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        std::sync::atomic::AtomicU64::new(seed)
    });
    loop {
        // splitmix64: one-to-one, so distinct counts give distinct ids.
        let mut z = next.fetch_add(0x9e37_79b9_7f4a_7c15, std::sync::atomic::Ordering::Relaxed);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        if z != 0 {
            return z;
        }
    }
}

/// FNV-1a, 64 bit: a hash that stays the same from run to run and build
/// to build, so it can be saved. The media folder names stored files by
/// it too.
#[derive(Debug, Clone, Copy)]
pub struct Fnv(pub u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn of(bytes: &[u8]) -> u64 {
        let mut h = Fnv::default();
        h.write(bytes);
        h.0
    }

    pub fn write(&mut self, bytes: &[u8]) -> &mut Self {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.write(&v.to_le_bytes())
    }

    pub fn f64(&mut self, v: f64) -> &mut Self {
        // -0.0 and 0.0 are the same place.
        self.u64(if v == 0.0 { 0 } else { v.to_bits() })
    }

    pub fn str(&mut self, s: &str) -> &mut Self {
        self.u64(s.len() as u64).write(s.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_matches_the_media_folder_names() {
        // FNV-1a of "a" (published test vector).
        assert_eq!(Fnv::of(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(Fnv::of(b""), Fnv::default().0);
    }

    #[test]
    fn a_link_round_trips_through_ron() {
        let link = BoardLink {
            path: PathBuf::from("D:/refs/board.pur"),
            scale: 0.5,
            offset: [10.0, -3.25],
            items: vec![LinkedItem {
                link: 7,
                key: 42,
                base: ItemState { at: [1.0, 2.0], size: [3.0, 4.0], rotation: 0.1, content: 99, style: 0, group: Some(3), tags: vec!["walk".into()] },
                gone: true,
            }],
            groups: vec![(3, 11)],
        };
        let text = ron::to_string(&link).unwrap();
        let back: BoardLink = ron::from_str(&text).unwrap();
        assert_eq!(back, link);
        assert!(back.is_gone(7) && !back.is_gone(8) && back.item(0).is_none());
    }
}
