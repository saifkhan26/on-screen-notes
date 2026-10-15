//! Bringing a picture, video or GIF file into the app.
//!
//! Runs on a background thread, since a large video takes a while to
//! copy: the file is copied into the media folder under a name hashed
//! from its contents (inserting the same file twice stores it once),
//! then a poster frame and the stream facts are read from the copy.
//! A still picture is not copied: like a pasted image, it is stored in
//! the slide itself as PNG.

use super::{ffmpeg, gif_player, video_player};
use crate::canvas::shape::{fresh_media_id, MediaKind, MediaMeta, MediaRef};
use crate::error::{anyhow, Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Video file extensions offered in the Open dialog and accepted on drop.
pub const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv"];

/// Still-picture extensions accepted on drop and in the Open dialog.
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];

/// Poster frames are at most this many pixels on their long side.
const POSTER_SIDE: u32 = 480;

/// Pictures are scaled down to at most this many pixels on their long
/// side. They are stored in the slide file, and a full-size camera photo
/// would add tens of megabytes to it; 4K still looks sharp full-screen.
const IMAGE_SIDE: u32 = 3840;

/// What a file would be imported as, from its extension.
pub fn media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext == "gif" {
        Some(MediaKind::Gif)
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Whether a file is a still picture, from its extension.
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Whether a file can be inserted at all.
pub fn is_supported(path: &Path) -> bool {
    is_image(path) || media_kind(path).is_some()
}

/// A finished import, ready to become a shape.
pub struct Imported {
    /// `None` for a still picture, including a GIF with a single frame.
    pub media: Option<MediaRef>,
    /// PNG for the shape's `png`: the poster, or the whole picture.
    pub png: Vec<u8>,
    pub png_size: [u32; 2],
    /// Natural display size in pixels.
    pub size: [u32; 2],
}

pub enum ImportError {
    /// A video, and FFmpeg is not installed.
    NeedsFfmpeg,
    Failed(String),
}

/// One import in flight.
pub struct ImportJob {
    pub path: PathBuf,
    /// Where it was dropped, in screen points; `None` = view centre.
    pub at: Option<egui::Pos2>,
    result: Arc<Mutex<Option<std::result::Result<Imported, ImportError>>>>,
}

impl ImportJob {
    pub fn start(path: PathBuf, at: Option<egui::Pos2>) -> Self {
        let result = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&result);
        let p = path.clone();
        let spawned = std::thread::Builder::new().name("osn-media-import".into()).spawn(move || {
            let r = run(&p);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        });
        if let Err(e) = spawned {
            *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(ImportError::Failed(e.to_string())));
        }
        Self { path, at, result }
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The outcome, once it is in.
    pub fn take(&self) -> Option<std::result::Result<Imported, ImportError>> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

fn run(path: &Path) -> std::result::Result<Imported, ImportError> {
    let failed = |e: crate::error::Error| ImportError::Failed(format!("{e:#}"));
    if is_image(path) {
        return import_image(path).map_err(failed);
    }
    let kind = media_kind(path).ok_or_else(|| ImportError::Failed("not a picture, video or GIF".into()))?;
    let source_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match kind {
        MediaKind::Gif => {
            let scan = gif_player::scan(path).map_err(failed)?;
            let first = gif_player::first_frame(path).map_err(failed)?;
            if scan.frames <= 1 {
                return Ok(Imported { media: None, png: encode_png(&first).map_err(failed)?, png_size: [first.width(), first.height()], size: scan.size });
            }
            let file = copy_hashed(path).map_err(failed)?;
            let poster = shrink(first, POSTER_SIDE);
            Ok(Imported {
                png: encode_png(&poster).map_err(failed)?,
                png_size: [poster.width(), poster.height()],
                size: scan.size,
                media: Some(MediaRef {
                    file,
                    kind,
                    source_name,
                    meta: Some(MediaMeta { frame_rate: [0, 1], frames: scan.frames, duration: scan.duration, size: scan.size }),
                    id: fresh_media_id(),
                }),
            })
        }
        MediaKind::Video => {
            let tools = ffmpeg::tools().ok_or(ImportError::NeedsFfmpeg)?;
            // Probe the original first, so an unplayable file is turned
            // away before it is copied.
            let info = video_player::probe(&tools, path).map_err(failed)?;
            let file = copy_hashed(path).map_err(failed)?;
            let stored = crate::persistence::paths::media_dir().map_err(failed)?.join(&file);
            let poster = video_player::grab_first_frame(&tools, &stored, &info, fit_side(info.size, POSTER_SIDE)).map_err(failed)?;
            Ok(Imported {
                png: encode_png(&poster).map_err(failed)?,
                png_size: [poster.width(), poster.height()],
                size: info.size,
                media: Some(MediaRef {
                    file,
                    kind,
                    source_name,
                    meta: Some(MediaMeta {
                        frame_rate: info.fps,
                        frames: info.frame_estimate().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
                        duration: info.duration.unwrap_or(0.0),
                        size: info.size,
                    }),
                    id: fresh_media_id(),
                }),
            })
        }
    }
}

/// A still picture, turned upright as its EXIF data asks (phone photos
/// are stored sideways) and scaled to `IMAGE_SIDE`.
fn import_image(path: &Path) -> Result<Imported> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .with_guessed_format()
        .context("reading the picture")?
        .into_decoder()
        .context("reading the picture")?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).context("decoding the picture")?;
    img.apply_orientation(orientation);
    let rgba = shrink(img.to_rgba8(), IMAGE_SIDE);
    let size = [rgba.width(), rgba.height()];
    Ok(Imported { media: None, png: encode_png(&rgba)?, png_size: size, size })
}

/// `size` scaled down (never up) so its long side is at most `side`.
fn fit_side(size: [u32; 2], side: u32) -> [u32; 2] {
    let long = size[0].max(size[1]).max(1);
    if long <= side { return [size[0].max(1), size[1].max(1)]; }
    let s = side as f64 / long as f64;
    [((size[0] as f64 * s).round() as u32).max(1), ((size[1] as f64 * s).round() as u32).max(1)]
}

fn shrink(img: image::RgbaImage, side: u32) -> image::RgbaImage {
    let [w, h] = fit_side([img.width(), img.height()], side);
    if [w, h] == [img.width(), img.height()] { return img; }
    image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
}

fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).context("encoding poster")?;
    Ok(png)
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// File name for content hashing to `hash`, keeping the extension.
fn hashed_name(hash: u64, src: &Path) -> String {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_ascii_lowercase();
    format!("{hash:016x}.{ext}")
}

/// Copy `src` into the media folder, hashing it on the way, and return
/// the stored file's name. A file already there under that name is the
/// same content, so the fresh copy is discarded.
fn copy_hashed(src: &Path) -> Result<String> {
    let dir = crate::persistence::paths::media_dir()?;
    copy_hashed_into(src, &dir)
}

fn copy_hashed_into(src: &Path, dir: &Path) -> Result<String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".import-{}-{n}.tmp", std::process::id()));
    let copied = (|| -> Result<u64> {
        let mut input = File::open(src).with_context(|| format!("opening {}", src.display()))?;
        let mut out = File::create(&tmp).context("creating the media copy")?;
        let mut buf = vec![0u8; 1 << 20];
        let mut hash = FNV_OFFSET;
        loop {
            let read = input.read(&mut buf).context("reading the file")?;
            if read == 0 { break; }
            for b in &buf[..read] {
                hash ^= u64::from(*b);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
            out.write_all(&buf[..read]).context("writing the media copy")?;
        }
        out.sync_all().context("writing the media copy")?;
        Ok(hash)
    })();
    let hash = match copied {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let name = hashed_name(hash, src);
    let dest = dir.join(&name);
    if dest.is_file() {
        let _ = std::fs::remove_file(&tmp);
    } else if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("storing the media copy: {e}"));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_the_extension() {
        assert_eq!(media_kind(Path::new("a/b.GIF")), Some(MediaKind::Gif));
        assert_eq!(media_kind(Path::new("clip.Mp4")), Some(MediaKind::Video));
        assert_eq!(media_kind(Path::new("photo.png")), None);
        assert_eq!(media_kind(Path::new("noext")), None);
        assert!(is_image(Path::new("photo.JPG")) && is_image(Path::new("a.webp")));
        assert!(!is_image(Path::new("anim.gif")), "GIFs go through the GIF path");
        assert!(is_supported(Path::new("x.jpeg")) && is_supported(Path::new("x.mov")));
        assert!(!is_supported(Path::new("notes.txt")));
    }

    #[test]
    fn pictures_import_as_plain_images_and_big_ones_shrink() {
        let dir = std::env::temp_dir().join(format!("osn-import-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.png");
        image::RgbaImage::from_pixel(50, 30, image::Rgba([10, 20, 30, 255])).save(&small).unwrap();
        // A JPEG under a .png name: the content decides, not the name.
        let big = dir.join("big.png");
        image::RgbImage::from_pixel(5000, 20, image::Rgb([200, 0, 0]))
            .save_with_format(&big, image::ImageFormat::Jpeg)
            .unwrap();

        let Ok(s) = run(&small) else { panic!("small picture failed") };
        assert!(s.media.is_none());
        assert_eq!((s.size, s.png_size), ([50, 30], [50, 30]));
        let back = image::load_from_memory(&s.png).unwrap();
        assert_eq!((back.width(), back.height()), (50, 30));

        let Ok(b) = run(&big) else { panic!("big picture failed") };
        assert_eq!(b.size, [IMAGE_SIDE, 15]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_content_is_stored_once() {
        let root = std::env::temp_dir().join(format!("osn-import-test-{}", std::process::id()));
        let media = root.join("media");
        std::fs::create_dir_all(&media).unwrap();
        let a = root.join("a.MP4");
        let b = root.join("b.mp4");
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        let na = copy_hashed_into(&a, &media).unwrap();
        let nb = copy_hashed_into(&b, &media).unwrap();
        assert_eq!(na, nb);
        assert!(na.ends_with(".mp4") && na.len() == 16 + 4, "{na}");
        let files: Vec<_> = std::fs::read_dir(&media).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "no leftover temp files");
        assert_eq!(std::fs::read(media.join(&na)).unwrap(), b"same bytes");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Needs ffmpeg, and `OSN_DATA_DIR` pointing at a scratch folder (the
    /// copies land in its `media/`). Imports a generated GIF and MP4
    /// through the real pipeline.
    #[test]
    #[ignore]
    fn imports_generated_gif_and_video() {
        assert!(std::env::var_os("OSN_DATA_DIR").is_some(), "set OSN_DATA_DIR to a scratch folder");
        let tools = ffmpeg::tools().expect("ffmpeg");
        let dir = std::env::temp_dir().join(format!("osn-import-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gif = dir.join("anim.gif");
        let mp4 = dir.join("clip.mp4");
        let make = |args: &[&str], out: &Path| {
            let ok = tools.ffmpeg_cmd().args(["-y", "-v", "error"]).args(args).arg(out).status().unwrap().success();
            assert!(ok, "ffmpeg failed making {}", out.display());
        };
        make(&["-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=3"], &gif);
        make(&["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=6", "-c:v", "libx264", "-pix_fmt", "yuv420p"], &mp4);

        let Ok(g) = run(&gif) else { panic!("GIF import failed") };
        let m = g.media.expect("animated GIF keeps its media");
        assert_eq!((m.kind, g.size), (MediaKind::Gif, [320, 240]));
        let meta = m.meta.unwrap();
        assert_eq!(meta.frames, 30);
        assert!((meta.duration - 3.0).abs() < 0.05, "{}", meta.duration);
        assert!(g.png_size[0] <= POSTER_SIDE && !g.png.is_empty());

        let v = match run(&mp4) {
            Ok(v) => v,
            Err(ImportError::Failed(e)) => panic!("video import failed: {e}"),
            Err(ImportError::NeedsFfmpeg) => panic!("ffmpeg vanished"),
        };
        let m = v.media.expect("video media");
        assert_eq!((m.kind, v.size, v.png_size), (MediaKind::Video, [1280, 720], [480, 270]));
        assert_eq!(m.meta.unwrap().frames, 180);
        let stored = crate::persistence::paths::media_dir().unwrap().join(&m.file);
        assert_eq!(std::fs::metadata(&stored).unwrap().len(), std::fs::metadata(&mp4).unwrap().len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn posters_shrink_but_never_grow() {
        assert_eq!(fit_side([1920, 1080], 480), [480, 270]);
        assert_eq!(fit_side([300, 200], 480), [300, 200]);
        assert_eq!(fit_side([1080, 1920], 480), [270, 480]);
    }
}
