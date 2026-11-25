//! The files picture search needs, fetched the first time it is turned
//! on: ONNX Runtime with DirectML (Microsoft's `onnxruntime-directml`
//! package from PyPI, 25 MB, of which two DLLs are kept), and the vision
//! model's picture half (Google's SigLIP 2, Apache-2.0, as exported to
//! ONNX by the `onnx-community` on Hugging Face) at half size, 189 MB,
//! which the GPU and the CPU both run. Searching by typed words fetches
//! the model's text half (565 MB) and its tokenizer (4 MB) the first
//! time it is used.
//!
//! Each comes from a fixed version, and is checked against the size and
//! SHA-256 it was published with before it is used. Fetching uses the
//! `curl.exe` and `tar.exe` that come with Windows; elsewhere the files
//! have to be put in the folder by hand.
//!
//! The folder is the app's cache folder, not the data folder: these are
//! not the user's work, and a scratch data folder (`OSN_DATA_DIR`) should
//! not fetch them again. `OSN_MODEL_DIR` points elsewhere.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A file fetched once.
#[derive(Debug, Clone, Copy)]
pub struct Download {
    /// Kept under this name in the folder.
    pub file: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// For the settings, while it comes in: "the runtime".
    pub what: &'static str,
}

/// ONNX Runtime 1.24.4 with DirectML, for 64-bit Windows.
pub const RUNTIME: Download = Download {
    file: "onnxruntime_directml-1.24.4-cp312-cp312-win_amd64.whl",
    url: "https://files.pythonhosted.org/packages/88/ea/33814eb0ec96775eda4c1d30b0d86e91d7d2cd0d84c66d3915aef0e06fa3/onnxruntime_directml-1.24.4-cp312-cp312-win_amd64.whl",
    size: 25_111_930,
    sha256: "f2ecb68b7b7b259d2ef3112ae760149f9b5a1e7c0fbb73d539da6250a648a614",
    what: "the runtime",
};

/// The model's picture half, at half size.
pub const VISION_MODEL: Download = Download {
    file: "siglip2-base-patch32-256-vision-fp16.onnx",
    url: "https://huggingface.co/onnx-community/siglip2-base-patch32-256-ONNX/resolve/7efccb86b5b6601bfe9e14326e1c11b40b68d44c/onnx/vision_model_fp16.onnx",
    size: 189_374_263,
    sha256: "44fcd984cf4ac4baba5b4776192c0b141a120c6981df49732c681d4fe8ddc718",
    what: "the model",
};

/// The model's text half, at half size: typed words to a vector.
pub const TEXT_MODEL: Download = Download {
    file: "siglip2-base-patch32-256-text-fp16.onnx",
    url: "https://huggingface.co/onnx-community/siglip2-base-patch32-256-ONNX/resolve/7efccb86b5b6601bfe9e14326e1c11b40b68d44c/onnx/text_model_fp16.onnx",
    size: 564_862_230,
    sha256: "3f8149272095410332433ca84f2c99611ed4679429eff43fbf3983ea15664ef3",
    what: "the text model",
};

/// The text half's SentencePiece tokenizer (`ml::tokenizer`).
pub const TOKENIZER: Download = Download {
    file: "siglip2-tokenizer.model",
    url: "https://huggingface.co/google/siglip2-base-patch32-256/resolve/94dffa8cb1179de3e03f091dbc3917e5d5a9ae84/tokenizer.model",
    size: 4_241_003,
    sha256: "61a7b147390c64585d6c3543dd6fc636906c9af3865a5548f27f31aee1d4c8e2",
    what: "the tokenizer",
};

/// The libraries kept out of `RUNTIME`.
#[cfg(target_os = "windows")]
const RUNTIME_FILES: [&str; 2] = ["onnxruntime.dll", "DirectML.dll"];

/// Which model these vectors come from, written with them
/// (`persistence::vectors`): vectors from another model do not compare.
pub const MODEL_ID: &str = "siglip2-base-patch32-256";

/// The folder they live in, made if need be.
pub fn dir() -> Result<PathBuf, String> {
    let dir = match std::env::var_os("OSN_MODEL_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => directories::ProjectDirs::from("dev", "OnScreenNotes", "on-screen-notes")
            .ok_or("could not find the app's cache folder")?
            .cache_dir()
            .join("models"),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The runtime is in `dir`.
pub fn runtime_present(dir: &Path) -> bool {
    dir.join(super::runtime::RUNTIME_LIB).is_file()
}

/// Fetch `d` into `dir` unless it is there; its path. `got` follows the
/// bytes fetched; `stop` gives up.
pub fn fetch(d: &Download, dir: &Path, got: &AtomicU64, stop: &AtomicBool) -> Result<PathBuf, String> {
    let path = dir.join(d.file);
    // Only a file that passed the check is ever given this name.
    if path.is_file() {
        return Ok(path);
    }
    fetch_checked(d, &path, got, stop)?;
    Ok(path)
}

/// Fetch the runtime into `dir`, keeping its libraries.
pub fn fetch_runtime(dir: &Path, got: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let wheel = fetch(&RUNTIME, dir, got, stop)?;
        // A wheel is a zip; Windows' tar reads zips.
        let unpack = dir.join("runtime-unpack");
        let _ = std::fs::remove_dir_all(&unpack);
        std::fs::create_dir_all(&unpack).map_err(|e| e.to_string())?;
        let inside: Vec<String> = RUNTIME_FILES.iter().map(|f| format!("onnxruntime/capi/{f}")).collect();
        let mut cmd = std::process::Command::new(system_tool("tar.exe"));
        cmd.arg("-xf").arg(&wheel).arg("-C").arg(&unpack).args(&inside);
        crate::media::ffmpeg::hide_console(&mut cmd);
        let out = cmd.output().map_err(|e| format!("could not run tar: {e}"))?;
        if !out.status.success() {
            return Err(format!("could not unpack the runtime: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        for (f, i) in RUNTIME_FILES.iter().zip(&inside) {
            std::fs::rename(unpack.join(i), dir.join(f)).map_err(|e| format!("could not keep {f}: {e}"))?;
        }
        let _ = std::fs::remove_dir_all(&unpack);
        let _ = std::fs::remove_file(&wheel);
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (got, stop);
        Err(format!("put {} in {}", super::runtime::RUNTIME_LIB, dir.display()))
    }
}

/// A tool that comes with Windows, by its full path, so no other program
/// of the same name is run instead.
#[cfg(target_os = "windows")]
fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join(name)
}

/// Fetch `d` to `path`: into `<path>.part`, checked, then renamed.
#[cfg(target_os = "windows")]
fn fetch_checked(d: &Download, path: &Path, got: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    let part = path.with_extension("part");
    let _ = std::fs::remove_file(&part);
    got.store(0, Ordering::Relaxed);
    let mut cmd = std::process::Command::new(system_tool("curl.exe"));
    cmd.args(["--location", "--fail", "--silent", "--show-error", "--retry", "2", "--output"]).arg(&part).arg(d.url);
    cmd.stderr(std::process::Stdio::piped());
    crate::media::ffmpeg::hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("could not run curl: {e}"))?;
    crate::media::ffmpeg::kill_with_app(&child);
    let status = loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Err("stopped".into());
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                got.store(std::fs::metadata(&part).map_or(0, |m| m.len()), Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(format!("curl: {e}")),
        }
    };
    if !status.success() {
        let mut msg = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut msg);
        }
        let _ = std::fs::remove_file(&part);
        return Err(format!("could not download {}: {}", d.what, msg.trim()));
    }
    let size = std::fs::metadata(&part).map_or(0, |m| m.len());
    got.store(size, Ordering::Relaxed);
    let hash = sha256(&part)?;
    if size != d.size || !hash.eq_ignore_ascii_case(d.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{} did not download whole (got {size} bytes, SHA-256 {hash})", d.what));
    }
    std::fs::rename(&part, path).map_err(|e| format!("could not keep {}: {e}", d.what))
}

#[cfg(not(target_os = "windows"))]
fn fetch_checked(d: &Download, path: &Path, _got: &AtomicU64, _stop: &AtomicBool) -> Result<(), String> {
    Err(format!("put {} in {}", d.file, path.parent().map_or_else(String::new, |p| p.display().to_string())))
}

/// A file's SHA-256, in hex, from Windows' `certutil`.
#[cfg(target_os = "windows")]
fn sha256(path: &Path) -> Result<String, String> {
    let mut cmd = std::process::Command::new(system_tool("certutil.exe"));
    cmd.arg("-hashfile").arg(path).arg("SHA256");
    crate::media::ffmpeg::hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("could not run certutil: {e}"))?;
    parse_certutil(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "could not check the download".to_string())
}

/// The hash in `certutil -hashfile` output: the line of 64 hex digits
/// (older Windows puts spaces between the bytes).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_certutil(out: &str) -> Option<String> {
    out.lines().map(|l| l.split_whitespace().collect::<String>().to_ascii_lowercase()).find(|l| l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_hash_certutil_prints() {
        let new = "SHA256 hash of x.whl:\r\nf2ecb68b7b7b259d2ef3112ae760149f9b5a1e7c0fbb73d539da6250a648a614\r\nCertUtil: -hashfile command completed successfully.\r\n";
        assert_eq!(parse_certutil(new).as_deref(), Some(RUNTIME.sha256));
        let old = "SHA256 hash of file x:\nf2 ec b6 8b 7b 7b 25 9d 2e f3 11 2a e7 60 14 9f 9b 5a 1e 7c 0f bb 73 d5 39 da 62 50 a6 48 a6 14\nCertUtil: -hashfile command completed successfully.\n";
        assert_eq!(parse_certutil(old).as_deref(), Some(RUNTIME.sha256));
        assert_eq!(parse_certutil("CertUtil: error"), None);
    }

    #[test]
    fn every_download_is_pinned() {
        for d in [RUNTIME, VISION_MODEL, TEXT_MODEL, TOKENIZER] {
            assert_eq!(d.sha256.len(), 64, "{}", d.file);
            assert!(d.url.starts_with("https://") && d.size > 0, "{}", d.file);
        }
    }
}
