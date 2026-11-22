//! `copy-ignores.ron` in the data folder: what was ignored in the list of
//! duplicate pictures (find's "every duplicate picture"), so it is not
//! listed again — a whole set ("ignore set"), or one picture that is not
//! a copy of the rest of its set ("not a copy"). Each comes down to pairs
//! of pictures kept apart (`similar::copies_apart`): a picture new to
//! them still joins them, so the set comes back with it. Pictures are
//! known by a key that outlives the run (`app::similar::picture_id`).
//! Written whole on each change, atomically; small.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

const FILE: &str = "copy-ignores.ron";

/// One thing ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ignore {
    /// These pictures are not copies of each other. The same key twice:
    /// that picture shown twice, which is meant.
    Set(Vec<String>),
    /// `picture` is not a copy of these.
    Picture { picture: String, of: Vec<String> },
}

impl Ignore {
    /// The pairs it keeps apart, each `(smaller, larger)`.
    fn pairs(&self) -> Vec<(String, String)> {
        let pair = |a: &String, b: &String| if a <= b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) };
        match self {
            Ignore::Set(all) => all.iter().enumerate().flat_map(|(i, a)| all[i + 1..].iter().map(move |b| pair(a, b))).collect(),
            Ignore::Picture { picture, of } => of.iter().map(|b| pair(picture, b)).collect(),
        }
    }

    /// In a set kept, its pictures in order; nothing to keep for none.
    fn tidy(self) -> Option<Self> {
        match self {
            Ignore::Set(mut all) => {
                all.sort();
                (all.len() >= 2).then_some(Ignore::Set(all))
            }
            Ignore::Picture { picture, mut of } => {
                of.sort();
                of.dedup();
                (!of.is_empty()).then_some(Ignore::Picture { picture, of })
            }
        }
    }
}

#[derive(Default)]
pub struct CopyIgnores {
    /// Oldest first.
    list: Vec<Ignore>,
    /// Every pair kept apart, each `(smaller, larger)`.
    pairs: HashSet<(String, String)>,
    path: Option<PathBuf>,
    /// Counts the changes: the copies are gathered again after each.
    generation: u64,
}

impl CopyIgnores {
    /// What earlier runs kept, from the data folder.
    pub fn load() -> Self {
        let path = match super::paths::data_dir() {
            Ok(d) => d.join(FILE),
            Err(e) => {
                log::warn!("no data folder for ignored copies: {e}");
                return Self::default();
            }
        };
        Self::load_from(path)
    }

    fn load_from(path: PathBuf) -> Self {
        let list = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str::<Vec<Ignore>>(&text).unwrap_or_else(|e| {
                log::warn!("{} unreadable, starting it over: {e}", path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        let mut me = CopyIgnores { list: Vec::new(), pairs: HashSet::new(), path: Some(path), generation: 0 };
        me.list = list.into_iter().filter_map(Ignore::tidy).collect();
        me.repair();
        me
    }

    /// What is ignored, oldest first.
    pub fn list(&self) -> &[Ignore] {
        &self.list
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every pair kept apart, each `(smaller, larger)`.
    pub fn pairs(&self) -> &HashSet<(String, String)> {
        &self.pairs
    }

    /// Ignore `i`, and keep that. False when there was nothing new.
    pub fn add(&mut self, i: Ignore) -> bool {
        let Some(i) = i.tidy() else { return false };
        if self.list.contains(&i) {
            return false;
        }
        self.list.push(i);
        self.changed();
        true
    }

    /// List `i` again (its pictures in any order), and keep that. False
    /// when it was not ignored.
    pub fn remove(&mut self, i: &Ignore) -> bool {
        let Some(i) = i.clone().tidy() else { return false };
        let Some(at) = self.list.iter().position(|x| *x == i) else { return false };
        self.list.remove(at);
        self.changed();
        true
    }

    fn changed(&mut self) {
        self.repair();
        self.generation += 1;
        self.save();
    }

    /// The pairs again, from the list.
    fn repair(&mut self) {
        self.pairs = self.list.iter().flat_map(Ignore::pairs).collect();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text = match ron::ser::to_string_pretty(&self.list, ron::ser::PrettyConfig::default()) {
            Ok(t) => t,
            Err(e) => return log::warn!("could not write ignored copies: {e}"),
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)) {
            log::warn!("could not keep ignored copies: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ignores_are_kept_as_pairs_and_come_back_from_the_file() {
        let dir = std::env::temp_dir().join(format!("osn-copy-ignores-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        let mut c = CopyIgnores::load_from(path.clone());
        assert!(c.list().is_empty());
        // A set, in any order; the same set again is nothing new.
        assert!(c.add(Ignore::Set(keys(&["c", "a", "b"]))));
        assert!(!c.add(Ignore::Set(keys(&["b", "c", "a"]))));
        // A picture shown twice, ignored: the pair of it with itself.
        assert!(c.add(Ignore::Set(keys(&["d", "d"]))));
        assert!(c.add(Ignore::Picture { picture: "z".into(), of: keys(&["y", "x", "y"]) }));
        // Nothing to keep apart: not added.
        assert!(!c.add(Ignore::Set(keys(&["a"]))));
        assert!(!c.add(Ignore::Picture { picture: "z".into(), of: Vec::new() }));
        let want: HashSet<(String, String)> =
            [("a", "b"), ("a", "c"), ("b", "c"), ("d", "d"), ("x", "z"), ("y", "z")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        assert_eq!(c.pairs(), &want);
        assert_eq!(c.generation(), 3);
        let back = CopyIgnores::load_from(path.clone());
        assert_eq!(back.list(), c.list());
        assert_eq!(back.pairs(), &want);
        // Listed again: its pairs go, the others' stay.
        assert!(c.remove(&Ignore::Set(keys(&["b", "a", "c"]))));
        assert!(!c.remove(&Ignore::Set(keys(&["a", "b", "c"]))));
        assert_eq!(c.pairs().len(), 3);
        assert_eq!(CopyIgnores::load_from(path).list().len(), 2);
    }
}
