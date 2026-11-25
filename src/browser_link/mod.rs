//! The browser extension's way in (`browser-extension/` in the repo): a
//! small HTTP server on 127.0.0.1 that takes pictures, GIFs and videos
//! sent from the browser, with where each came from.
//!
//! Who may talk to it: the extension asks to connect once (`POST
//! /v1/pair`), the app asks the user, and a yes gives the extension a
//! token it sends from then on (`tokens`). Web pages cannot get that
//! far: every request must carry an `X-OSN` header, which makes a page's
//! browser ask the server first (a CORS preflight), and the server never
//! says yes to one; an `Origin` of a web page is turned away as well.
//! Extensions with access to the address skip that check. The `Host`
//! must be this machine, so a web address pointed at 127.0.0.1 is
//! turned away too.
//!
//! Where a picture came from (`Source`) is kept beside the slides, not
//! in them (`persistence::sources`), keyed by the picture's file
//! (`shape_key`), so the slide files do not change.

pub mod http;
pub mod queue;
mod server;
pub mod tokens;

pub use server::{Event, Link};

use crate::canvas::shape::Shape;

/// The port the app listens on unless the config says otherwise.
pub const DEFAULT_PORT: u16 = 47823;

/// Where a picture sent from the browser came from, as the extension
/// read it off the page. Empty for what it could not tell.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    /// The page it was on.
    #[serde(default)]
    pub page: String,
    /// The post it belongs to (that post's own address), when found.
    #[serde(default)]
    pub post: String,
    /// Who posted it: their handle, without `@`.
    #[serde(default)]
    pub author: String,
    /// The site, for people: "X", "Instagram".
    #[serde(default)]
    pub site: String,
    /// The file's own address.
    #[serde(default)]
    pub media: String,
}

/// What the user asked for along with a file, beyond where it came from:
/// tags to put on it (typed in the extension, or the artist they picked
/// on Lens), and a slide to put it on other than the one open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extra {
    pub tags: Vec<String>,
    /// The slide's index and name as the extension saw them (the name
    /// finds it again if slides moved since).
    pub slide: Option<(usize, String)>,
}

/// A handle as a tag: no `@`, only what handles are made of, at most 64
/// characters. Empty when nothing is left.
pub fn clean_handle(s: &str) -> String {
    s.trim().trim_start_matches('@').chars().filter(|c| c.is_alphanumeric() || "._-".contains(*c)).take(64).collect()
}

/// What a picture is known by in `persistence::sources`: the file it is
/// stored as in the media folder, or for one still kept in its slide,
/// the name it will be stored under (`picture_store` names files by
/// their bytes), so the entry keeps working once it is moved out.
pub fn shape_key(shape: &Shape) -> Option<String> {
    let Shape::Raster { png, media, picture, .. } = shape else { return None };
    Some(match (media, picture) {
        (Some(m), _) => m.file.clone(),
        (None, Some(p)) => p.file.clone(),
        (None, None) => bytes_key(png),
    })
}

/// `shape_key` for a picture, GIF or video just imported, before it is
/// a shape.
pub fn imported_key(im: &crate::media::import::Imported) -> String {
    match &im.media {
        Some(m) => m.file.clone(),
        None => bytes_key(&im.png),
    }
}

/// The name `picture_store` gives a picture with these bytes.
pub fn bytes_key(bytes: &[u8]) -> String {
    use image::ImageFormat as F;
    let ext = match image::guess_format(bytes) {
        Ok(F::Jpeg) => "jpg",
        Ok(F::WebP) => "webp",
        Ok(F::Bmp) => "bmp",
        _ => "png",
    };
    crate::media::import::media_name(crate::canvas::board_link::Fnv::of(bytes), ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_lose_the_at_and_anything_odd() {
        assert_eq!(clean_handle(" @Antin_Illust "), "Antin_Illust");
        assert_eq!(clean_handle("kaizin.art"), "kaizin.art");
        assert_eq!(clean_handle("a b/c<d>"), "abcd");
        assert_eq!(clean_handle("@"), "");
    }

    #[test]
    fn a_picture_kept_in_its_slide_is_known_by_its_future_file_name() {
        let mut png = Vec::new();
        image::RgbaImage::new(3, 2).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let key = bytes_key(&png);
        assert!(key.ends_with(".png") && key.len() == 20, "{key}");
        let dir = std::env::temp_dir().join(format!("osn-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (stored, _) = crate::media::import::store_bytes_into(&dir, &png, "png").unwrap();
        assert_eq!(stored, key);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
