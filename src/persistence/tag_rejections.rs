//! `tag-rejections.ron` in the data folder: the pictures turned down for
//! a tag ("not this" on a suggestion, `tools::tag_suggest`), so they are
//! not suggested for it again. Each is a tag (as `units::tag_key` folds
//! it) and a picture's key (`canvas::picture_vectors`: its file and crop,
//! or its bytes). Written whole on each change, atomically; small.

use crate::canvas::units::tag_key;
use std::collections::HashSet;
use std::path::PathBuf;

const FILE: &str = "tag-rejections.ron";

#[derive(Default)]
pub struct Rejections {
    set: HashSet<(String, String)>,
    path: Option<PathBuf>,
}

impl Rejections {
    /// What earlier runs kept, from the data folder.
    pub fn load() -> Self {
        let path = match super::paths::data_dir() {
            Ok(d) => d.join(FILE),
            Err(e) => {
                log::warn!("no data folder for tag rejections: {e}");
                return Self::default();
            }
        };
        Self::load_from(path)
    }

    fn load_from(path: PathBuf) -> Self {
        let set = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str::<Vec<(String, String)>>(&text).unwrap_or_else(|e| {
                log::warn!("{} unreadable, starting it over: {e}", path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Rejections { set: set.into_iter().collect(), path: Some(path) }
    }

    /// The picture with `key` was turned down for `tag`.
    pub fn contains(&self, tag: &str, key: &str) -> bool {
        self.set.contains(&(tag_key(tag), key.to_string()))
    }

    /// Turn the picture with `key` down for `tag`, and keep that.
    pub fn add(&mut self, tag: &str, key: &str) {
        if self.set.insert((tag_key(tag), key.to_string())) {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut all: Vec<&(String, String)> = self.set.iter().collect();
        all.sort();
        let text = match ron::ser::to_string_pretty(&all, ron::ser::PrettyConfig::default()) {
            Ok(t) => t,
            Err(e) => return log::warn!("could not write tag rejections: {e}"),
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)) {
            log::warn!("could not keep tag rejections: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejections_are_kept_case_aside() {
        let dir = std::env::temp_dir().join(format!("osn-rejections-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        let mut r = Rejections::load_from(path.clone());
        assert!(!r.contains("Aria", "file:a.png:None"));
        r.add("Aria", "file:a.png:None");
        r.add("aria", "file:a.png:None");
        assert_eq!(r.set.len(), 1);
        let r = Rejections::load_from(path);
        assert!(r.contains("ARIA", "file:a.png:None"));
        assert!(!r.contains("aria", "file:b.png:None"));
    }
}
