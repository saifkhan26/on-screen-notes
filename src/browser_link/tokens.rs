//! `browser-link.ron` in the data folder: the browser extensions the user
//! said yes to, each by the token it was given then. One token per
//! extension install: one that connects again (its token lost, or
//! "Connect" pressed again) replaces its old one. Written whole on each
//! change, atomically; small.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE: &str = "browser-link.ron";

/// One extension let in.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Paired {
    pub token: String,
    /// What it called itself when it asked ("Firefox", "Chrome").
    pub client: String,
    /// When, in seconds since 1970.
    pub added: u64,
    /// The install it was given to (the extension's own id for itself);
    /// empty for one let in before installs said so.
    #[serde(default)]
    pub install: String,
}

impl Paired {
    /// Whether this is the same install as one calling itself `client`
    /// with id `install`: by its id, or (from before ids) by its name.
    fn same(&self, client: &str, install: &str) -> bool {
        if install.is_empty() {
            self.install.is_empty() && self.client.eq_ignore_ascii_case(client)
        } else {
            self.install == install
        }
    }
}

#[derive(Default)]
pub struct Tokens {
    list: Vec<Paired>,
    path: Option<PathBuf>,
}

impl Tokens {
    /// What earlier runs let in, from the data folder.
    pub fn load() -> Self {
        match crate::persistence::paths::data_dir() {
            Ok(d) => Self::load_from(d.join(FILE)),
            Err(e) => {
                log::warn!("no data folder for the browser link: {e}");
                Self::default()
            }
        }
    }

    pub fn load_from(path: PathBuf) -> Self {
        let list = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str::<Vec<Paired>>(&text).unwrap_or_else(|e| {
                log::warn!("{} unreadable, starting it over: {e}", path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Tokens { list, path: Some(path) }
    }

    /// Where `token` is in the list, if an extension was let in with it.
    /// Every token is compared in full, so how long this takes says
    /// nothing about how close a guess came.
    fn find(&self, token: &str) -> Option<usize> {
        let t = token.as_bytes();
        let mut found = None;
        for (at, p) in self.list.iter().enumerate() {
            let k = p.token.as_bytes();
            let mut diff = u8::from(k.len() != t.len());
            for (i, b) in k.iter().enumerate() {
                diff |= b ^ t.get(i).copied().unwrap_or(0);
            }
            if diff == 0 && !t.is_empty() {
                found = Some(at);
            }
        }
        found
    }

    /// Whether `token` belongs to an extension that was let in.
    #[cfg(test)]
    pub fn knows(&self, token: &str) -> bool {
        self.find(token).is_some()
    }

    /// `knows`, for a request that carries `token`: an install's older
    /// tokens (left from before installs said who they were) go once it
    /// is seen using this one.
    pub fn use_token(&mut self, token: &str) -> bool {
        let Some(at) = self.find(token) else { return false };
        let me = self.list[at].clone();
        let before = self.list.len();
        self.list.retain(|p| p.token == me.token || !p.same(&me.client, &me.install));
        if self.list.len() != before {
            self.save();
        }
        true
    }

    /// Let an extension in: a fresh token for it, kept, in place of any
    /// its install had.
    pub fn add(&mut self, client: &str, install: &str) -> String {
        let token = fresh_token();
        let added = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.list.retain(|p| !p.same(client, install));
        self.list.push(Paired { token: token.clone(), client: client.to_string(), added, install: install.to_string() });
        self.save();
        token
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.list.len()
    }

    /// The browsers let in, each named once ("Firefox", "Chrome"), as
    /// they called themselves.
    pub fn browsers(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in &self.list {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&p.client)) {
                out.push(p.client.clone());
            }
        }
        out
    }

    /// Every extension let in is out again: each must ask once more.
    pub fn forget_all(&mut self) {
        if !self.list.is_empty() {
            self.list.clear();
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text = match ron::ser::to_string_pretty(&self.list, ron::ser::PrettyConfig::default()) {
            Ok(t) => t,
            Err(e) => return log::warn!("could not write the browser link's list: {e}"),
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)) {
            log::warn!("could not keep the browser link's list: {e}");
        }
    }
}

/// 128 bits no other program can guess: SipHash keyed by `RandomState`,
/// whose keys come from the operating system's randomness.
fn fresh_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    (0..2u8)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(now);
            h.write_u32(std::process::id());
            h.write_u8(i);
            format!("{:016x}", h.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_let_their_extension_in_and_are_kept() {
        let dir = std::env::temp_dir().join(format!("osn-browser-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        let mut t = Tokens::load_from(path.clone());
        assert!(!t.knows(""));
        let a = t.add("Firefox", "");
        let b = t.add("Chrome", "");
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(t.knows(&a) && t.knows(&b));
        assert!(!t.knows(&a[..31]));
        assert!(!t.knows(&format!("{a}0")));
        let t2 = Tokens::load_from(path.clone());
        assert_eq!(t2.count(), 2);
        assert!(t2.knows(&a));
        let mut t3 = t2;
        t3.forget_all();
        assert!(!Tokens::load_from(path).knows(&a));
    }

    /// One Firefox that connected six times shows as one: an install that
    /// connects again replaces its token, and old tokens without an
    /// install go once the one in use is seen.
    #[test]
    fn an_install_is_let_in_once() {
        let dir = std::env::temp_dir().join(format!("osn-browser-link-once-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        let mut t = Tokens::load_from(path.clone());
        // Before installs said who they were: six for one Firefox.
        let old: Vec<String> = (0..6).map(|_| t.add_legacy("Firefox")).collect();
        let chrome = t.add_legacy("Chrome");
        assert_eq!(t.count(), 7);
        assert!(t.use_token(&old[3]));
        assert_eq!(t.count(), 2, "Firefox's other five went; Chrome stays");
        assert!(t.knows(&old[3]) && t.knows(&chrome) && !t.knows(&old[0]));
        // An install connecting again replaces its own token only.
        let a = t.add("Firefox", "inst-a");
        let b = t.add("Firefox", "inst-b");
        let a2 = t.add("Firefox", "inst-a");
        assert!(!t.knows(&a) && t.knows(&a2) && t.knows(&b));
        assert_eq!(Tokens::load_from(path.clone()).count(), 4);
        assert!(!t.use_token("nope"));
        let _ = std::fs::remove_file(&path);
    }

    impl Tokens {
        /// A token as one was given before installs said who they were.
        fn add_legacy(&mut self, client: &str) -> String {
            let token = fresh_token();
            self.list.push(Paired { token: token.clone(), client: client.to_string(), added: 0, install: String::new() });
            self.save();
            token
        }
    }
}
