//! `artist-tags.ron` in the data folder: which tags name an artist, so
//! "Find artist" can pass over pictures that have one already.
//!
//! It starts from every tag the slides hold the first time it is needed
//! (the user's tags were nearly all artists' handles, from PureRef group
//! names), then learns: a handle picked on Lens or taken from a post is an
//! artist; a tag given while sending from the browser ("pose hands") is
//! not. The tag list's switch says so for any tag. Tags are kept as
//! `units::tag_key` folds them. Written whole on each change, atomically.

use crate::canvas::units::tag_key;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

const FILE: &str = "artist-tags.ron";

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// Started from the slides' tags already.
    #[serde(default)]
    seeded: bool,
    #[serde(default)]
    artists: Vec<String>,
    /// Tags known not to be artists (left out when it starts).
    #[serde(default)]
    not_artists: Vec<String>,
}

#[derive(Default)]
pub struct ArtistTags {
    artists: HashSet<String>,
    not_artists: HashSet<String>,
    seeded: bool,
    path: Option<PathBuf>,
}

impl ArtistTags {
    /// What earlier runs kept, from the data folder.
    pub fn load() -> Self {
        match super::paths::data_dir() {
            Ok(d) => Self::load_from(d.join(FILE)),
            Err(e) => {
                log::warn!("no data folder for artist tags: {e}");
                Self::default()
            }
        }
    }

    pub fn load_from(path: PathBuf) -> Self {
        let saved = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str::<Saved>(&text).unwrap_or_else(|e| {
                log::warn!("{} unreadable, starting it over: {e}", path.display());
                Saved::default()
            }),
            Err(_) => Saved::default(),
        };
        ArtistTags { artists: saved.artists.into_iter().collect(), not_artists: saved.not_artists.into_iter().collect(), seeded: saved.seeded, path: Some(path) }
    }

    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Start from `tags` (every tag on the slides), once: each is an
    /// artist unless known not to be.
    pub fn seed<'a>(&mut self, tags: impl IntoIterator<Item = &'a str>) {
        if self.seeded {
            return;
        }
        for t in tags {
            let k = tag_key(t);
            if !self.not_artists.contains(&k) {
                self.artists.insert(k);
            }
        }
        self.seeded = true;
        self.save();
    }

    #[cfg(test)]
    pub fn is_artist(&self, tag: &str) -> bool {
        self.artists.contains(&tag_key(tag))
    }

    /// Say whether `tag` names an artist.
    pub fn set(&mut self, tag: &str, artist: bool) {
        let k = tag_key(tag);
        let changed = if artist {
            let a = self.artists.insert(k.clone());
            self.not_artists.remove(&k) || a
        } else {
            let a = self.artists.remove(&k);
            self.not_artists.insert(k) || a
        };
        if changed {
            self.save();
        }
    }

    /// `tag` was given while sending (a subject, not who made it): not an
    /// artist, unless it is known to be one.
    pub fn note_subject(&mut self, tag: &str) {
        let k = tag_key(tag);
        if !self.artists.contains(&k) && self.not_artists.insert(k) {
            self.save();
        }
    }

    /// Tag `from` became `to` everywhere: what was said of it carries over.
    pub fn rename(&mut self, from: &str, to: &[String]) {
        let k = tag_key(from);
        let (was_artist, was_not) = (self.artists.remove(&k), self.not_artists.remove(&k));
        for t in to {
            if was_artist {
                self.artists.insert(tag_key(t));
            } else if was_not && !self.artists.contains(&tag_key(t)) {
                self.not_artists.insert(tag_key(t));
            }
        }
        if was_artist || was_not {
            self.save();
        }
    }

    /// The artists, as `tag_key` folds them.
    pub fn artists(&self) -> &HashSet<String> {
        &self.artists
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut artists: Vec<String> = self.artists.iter().cloned().collect();
        artists.sort();
        let mut not_artists: Vec<String> = self.not_artists.iter().cloned().collect();
        not_artists.sort();
        let saved = Saved { seeded: self.seeded, artists, not_artists };
        let text = match ron::ser::to_string_pretty(&saved, ron::ser::PrettyConfig::default()) {
            Ok(t) => t,
            Err(e) => return log::warn!("could not write artist tags: {e}"),
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)) {
            log::warn!("could not keep artist tags: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artists_start_from_the_tags_then_learn() {
        let dir = std::env::temp_dir().join(format!("osn-artist-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        let mut a = ArtistTags::load_from(path.clone());
        assert!(!a.is_seeded());
        // A tag given while sending before it starts stays out.
        a.note_subject("pose");
        a.seed(["Antin_Illust", "kaizin.art", "pose", "AI"]);
        assert!(a.is_seeded());
        assert!(a.is_artist("antin_illust") && a.is_artist("KAIZIN.ART"));
        assert!(!a.is_artist("pose"));
        a.seed(["later"]);
        assert!(!a.is_artist("later"), "it starts once");
        a.set("AI", false);
        a.set("Punipaws", true);
        a.note_subject("Punipaws");
        assert!(a.is_artist("punipaws"), "a known artist stays one");
        a.rename("kaizin.art", &["kaizin".to_string()]);
        let a = ArtistTags::load_from(path.clone());
        assert!(a.is_seeded());
        assert!(!a.is_artist("ai") && !a.is_artist("kaizin.art"));
        assert!(a.is_artist("kaizin") && a.is_artist("punipaws") && a.is_artist("antin_illust"));
        let _ = std::fs::remove_file(&path);
    }
}
