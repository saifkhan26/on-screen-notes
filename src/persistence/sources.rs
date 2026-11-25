//! `picture-sources.ron` in the data folder: where pictures sent from the
//! browser came from (`browser_link::Source`), by what each picture is
//! known by (`browser_link::shape_key`: its file in the media folder).
//! Kept beside the slides so their files do not change; every copy of a
//! picture shares its entry. Written whole on each change, atomically.

use crate::browser_link::Source;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

const FILE: &str = "picture-sources.ron";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    pub source: Source,
    /// The user said the poster is not who made it: the poster is not
    /// offered as its tag any more.
    #[serde(default)]
    pub not_artist: bool,
    /// When it came in, in seconds since 1970.
    #[serde(default)]
    pub added: u64,
}

#[derive(Default)]
pub struct Sources {
    map: HashMap<String, Entry>,
    path: Option<PathBuf>,
    /// Goes up on each change, so what was worked out from it can tell
    /// it is out of date.
    pub generation: u64,
}

impl Sources {
    /// What earlier runs kept, from the data folder.
    pub fn load() -> Self {
        match super::paths::data_dir() {
            Ok(d) => Self::load_from(d.join(FILE)),
            Err(e) => {
                log::warn!("no data folder for picture sources: {e}");
                Self::default()
            }
        }
    }

    pub fn load_from(path: PathBuf) -> Self {
        let map = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str::<Vec<(String, Entry)>>(&text).map(|v| v.into_iter().collect()).unwrap_or_else(|e| {
                log::warn!("{} unreadable, starting it over: {e}", path.display());
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        Sources { map, path: Some(path), generation: 0 }
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.map.get(key)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Where the picture known by `key` came from. A picture sent again
    /// keeps an earlier "not the artist" if the poster is the same.
    pub fn put(&mut self, key: String, source: Source) {
        let added = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let not_artist = self.map.get(&key).is_some_and(|e| e.not_artist && crate::canvas::units::same_tag(&e.source.author, &source.author));
        self.map.insert(key, Entry { source, not_artist, added });
        self.changed();
    }

    /// Their poster is not who made the pictures known by `keys`.
    pub fn mark_not_artist(&mut self, keys: &[String]) {
        let mut any = false;
        for k in keys {
            if let Some(e) = self.map.get_mut(k) {
                any |= !e.not_artist;
                e.not_artist = true;
            }
        }
        if any {
            self.changed();
        }
    }

    fn changed(&mut self) {
        self.generation += 1;
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut all: Vec<(&String, &Entry)> = self.map.iter().collect();
        all.sort_by(|a, b| a.0.cmp(b.0));
        let text = match ron::ser::to_string_pretty(&all, ron::ser::PrettyConfig::default()) {
            Ok(t) => t,
            Err(e) => return log::warn!("could not write picture sources: {e}"),
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)) {
            log::warn!("could not keep picture sources: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(author: &str) -> Source {
        Source { author: author.into(), site: "X".into(), post: format!("https://x.com/{author}/status/1"), ..Default::default() }
    }

    #[test]
    fn sources_are_kept_and_not_the_artist_sticks() {
        let dir = std::env::temp_dir().join(format!("osn-sources-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        let mut s = Sources::load_from(path.clone());
        assert!(s.is_empty());
        s.put("a.png".into(), from("Aria"));
        s.put("b.mp4".into(), from("Bo"));
        s.mark_not_artist(&["a.png".into()]);
        let g = s.generation;
        s.mark_not_artist(&["a.png".into()]);
        assert_eq!(s.generation, g, "nothing changed");
        let mut s = Sources::load_from(path.clone());
        assert_eq!(s.get("b.mp4").unwrap().source.author, "Bo");
        assert!(s.get("a.png").unwrap().not_artist);
        // Sent again by the same poster: still not the artist. Another
        // poster: offered.
        s.put("a.png".into(), from("aria"));
        assert!(s.get("a.png").unwrap().not_artist);
        s.put("a.png".into(), from("Cy"));
        assert!(!s.get("a.png").unwrap().not_artist);
        let _ = std::fs::remove_file(&path);
    }
}
