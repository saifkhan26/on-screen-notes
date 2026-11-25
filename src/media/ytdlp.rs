//! yt-dlp: downloads the videos web pages only stream (Instagram, X and
//! most players play `blob:` streams the browser extension cannot fetch),
//! from the post's own address. Run as a child process, like FFmpeg;
//! found, in order, beside the app's exe, in WinGet's link and package
//! folders (`winget install yt-dlp.yt-dlp`, which `Installer` runs), and
//! on `PATH`. Sites change and yt-dlp follows them: an old one fails
//! where a new one works (`winget upgrade yt-dlp.yt-dlp`).

use super::ffmpeg::{hide_console, kill_with_app};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A miss is looked for again after this long: yt-dlp may have been
/// installed meanwhile.
const MISS_KEPT: Duration = Duration::from_secs(30);
/// A download still running after this long is stopped.
const LONGEST: Duration = Duration::from_secs(15 * 60);
/// The most videos taken from one post (a carousel).
const MOST_ITEMS: u32 = 6;

/// What was found, and when it was looked for.
static FOUND: Mutex<Option<(Option<PathBuf>, Instant)>> = Mutex::new(None);
/// yt-dlp was updated (or tried) this run.
static UPDATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The one update a run gets: `true` for the first caller only.
pub fn claim_update() -> bool {
    !UPDATED.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// `yt-dlp -U`: `Ok(true)` when it updated itself, `Ok(false)` when it
/// was up to date already.
pub fn update(exe: &Path) -> Result<bool, String> {
    let mut cmd = Command::new(exe);
    cmd.arg("-U").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = hide_console(&mut cmd).output().map_err(|e| format!("could not start yt-dlp: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    log::info!("yt-dlp -U: {}", text.trim());
    if text.contains("Updated yt-dlp to") {
        return Ok(true);
    }
    if text.contains("is up to date") {
        return Ok(false);
    }
    Err(error_from(&text))
}

/// yt-dlp's exe, if there is one. The first look runs `yt-dlp
/// --version` (a second or so); the answer is kept.
pub fn find() -> Option<PathBuf> {
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    match &*found {
        Some((Some(p), _)) => return Some(p.clone()),
        Some((None, at)) if at.elapsed() < MISS_KEPT => return None,
        _ => {}
    }
    let exe = detect();
    *found = Some((exe.clone(), Instant::now()));
    exe
}

/// Look again now (after an install).
pub fn redetect() -> Option<PathBuf> {
    *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
    find()
}

fn exe_name() -> &'static str {
    if cfg!(target_os = "windows") { "yt-dlp.exe" } else { "yt-dlp" }
}

fn candidates() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::current_exe().ok().as_deref().and_then(Path::parent) {
        dirs.push(d.to_path_buf());
    }
    if cfg!(target_os = "windows") {
        let roots = [
            std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Microsoft").join("WinGet")),
            std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join("WinGet")),
        ];
        for root in roots.into_iter().flatten() {
            dirs.push(root.join("Links"));
            if let Ok(entries) = std::fs::read_dir(root.join("Packages")) {
                dirs.extend(entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("yt-dlp.yt-dlp")).map(|e| e.path()));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join(exe_name())).collect()
}

fn detect() -> Option<PathBuf> {
    for exe in candidates() {
        if !exe.is_file() {
            continue;
        }
        let mut cmd = Command::new(&exe);
        cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        match hide_console(&mut cmd).output() {
            Ok(out) if out.status.success() => {
                log::info!("using yt-dlp {} at {}", String::from_utf8_lossy(&out.stdout).trim(), exe.display());
                return Some(exe);
            }
            _ => log::warn!("{} would not run", exe.display()),
        }
    }
    None
}

/// The arguments for downloading `url`'s videos into `dir`: the best
/// video up to 1080p, H.264 and MP4 preferred (quick for FFmpeg to
/// play back), each file's path printed once it is in place.
pub fn args(url: &str, dir: &Path, cookies: Option<&Path>, ffmpeg: Option<&Path>) -> Vec<OsString> {
    let mut a: Vec<OsString> = [
        "--ignore-config",
        "--no-warnings",
        "--no-progress",
        "--no-mtime",
        "--no-part",
        "--restrict-filenames",
        "-f",
        "bv*/b",
        "-S",
        "res:1080,vcodec:h264,ext:mp4:m4a",
        "--playlist-items",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    a.push(format!("1:{MOST_ITEMS}").into());
    a.extend(["--max-filesize", "2000M", "--socket-timeout", "30", "--print", "after_move:filepath", "-o"].map(OsString::from));
    a.push(dir.join("%(id).60B-%(autonumber)d.%(ext)s").into_os_string());
    if let Some(c) = cookies {
        a.push("--cookies".into());
        a.push(c.as_os_str().to_owned());
    }
    if let Some(f) = ffmpeg {
        a.push("--ffmpeg-location".into());
        a.push(f.as_os_str().to_owned());
    }
    a.push("--".into());
    a.push(url.into());
    a
}

/// How a site that can't be reached is told apart: then it's the
/// computer or network (a DNS filter, a firewall, no connection), and
/// updating yt-dlp can't help.
pub const UNREACHABLE: &str = "couldn't reach";

/// The host yt-dlp couldn't connect to or look up, if that is what
/// went wrong (curl's "Failed to connect to host:443", "Could not resolve
/// host: …", Python's "getaddrinfo failed" / "Name or service not known").
fn unreachable_host(stderr: &str) -> Option<String> {
    for line in stderr.lines().rev() {
        for marker in ["Failed to connect to ", "Could not resolve host: ", "Could not resolve host "] {
            if let Some(at) = line.find(marker) {
                let rest = &line[at + marker.len()..];
                let host: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || ".-".contains(*c)).collect();
                if !host.is_empty() {
                    return Some(host);
                }
            }
        }
        if line.contains("getaddrinfo failed") || line.contains("Name or service not known") || line.contains("Errno 11001") {
            return Some("the site".to_string());
        }
    }
    None
}

/// What went wrong, for people: yt-dlp's last `ERROR:` line, cut short;
/// a site that can't be reached is said plainly.
pub fn error_from(stderr: &str) -> String {
    if let Some(host) = unreachable_host(stderr) {
        return format!("{UNREACHABLE} {host} — something on this computer or network blocks it (a DNS filter, firewall or VPN), or the connection is down");
    }
    let line = stderr.lines().rev().find(|l| l.starts_with("ERROR:")).or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("yt-dlp stopped");
    let line = line.trim_start_matches("ERROR:").trim();
    // "[Instagram] Abc: Requested content is not available…" → the part people read.
    let line = match line.split_once("] ") {
        Some((site, rest)) if site.starts_with('[') => rest.split_once(": ").map_or(rest, |(_, r)| r),
        _ => line,
    };
    let short: String = line.chars().take(160).collect();
    if short.len() < line.len() { format!("{short}…") } else { short }
}

/// Download `url`'s videos into `dir` with `exe`; the files' paths. Blocks
/// until yt-dlp is done (at most `LONGEST`).
pub fn download(exe: &Path, url: &str, dir: &Path, cookies: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let ffmpeg = super::ffmpeg::tools().map(|t| t.ffmpeg);
    let mut cmd = Command::new(exe);
    cmd.args(args(url, dir, cookies, ffmpeg.as_deref())).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = hide_console(&mut cmd).spawn().map_err(|e| format!("could not start yt-dlp: {e}"))?;
    kill_with_app(&child);
    // Both pipes read on their own threads, so neither fills and stalls it.
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = pipe(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() > LONGEST => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(format!("yt-dlp: {e}")),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let files: Vec<PathBuf> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).filter(|p| p.is_file()).collect();
    match status {
        None => Err("it took too long".into()),
        Some(_) if !files.is_empty() => Ok(files),
        Some(_) => Err(error_from(&stderr)),
    }
}

/// One run of `winget install yt-dlp.yt-dlp`, watched from a background
/// thread, in its own console so the user sees it.
pub struct Installer {
    done: Arc<Mutex<Option<Result<bool, String>>>>,
}

impl Installer {
    pub fn start() -> Self {
        let done = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&done);
        let spawned = std::thread::Builder::new().name("osn-ytdlp-install".into()).spawn(move || {
            let result = run_winget();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        });
        if let Err(e) = spawned {
            *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(e.to_string()));
        }
        Self { done }
    }

    /// `Some(Ok(true))` once yt-dlp can be found after the install.
    pub fn take_result(&self) -> Option<Result<bool, String>> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

fn run_winget() -> Result<bool, String> {
    let mut cmd = Command::new("winget");
    cmd.args(["install", "--id", "yt-dlp.yt-dlp", "-e", "--source", "winget"]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    let status = cmd.status().map_err(|e| format!("could not start winget: {e}"))?;
    log::info!("winget exited with {status}");
    Ok(redetect().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_that_cant_be_reached_is_said_plainly() {
        let err = "ERROR: [Instagram] C1: Video info extraction failed: Failed to perform, curl: (7) Failed to connect to www.instagram.com:443 after 2181 ms: Could not connect to server. See https://curl.se/libcurl/c/libcurl-errors.html first for more details.";
        let e = error_from(err);
        assert!(e.starts_with("couldn't reach www.instagram.com — "), "{e}");
        assert!(error_from("ERROR: Unable to download webpage: curl: (6) Could not resolve host: x.com").starts_with("couldn't reach x.com"));
        assert!(error_from("ERROR: <urlopen error [Errno 11001] getaddrinfo failed>").starts_with("couldn't reach the site"));
        assert!(!error_from("ERROR: [Instagram] C1: login required").starts_with(UNREACHABLE));
    }

    #[test]
    fn errors_are_cut_to_what_people_read() {
        let err = "WARNING: x\nERROR: [Instagram] C1x2: Requested content is not available, rate-limit reached or login required. Use --cookies\n";
        assert_eq!(error_from(err), "Requested content is not available, rate-limit reached or login required. Use --cookies");
        assert_eq!(error_from("ERROR: Unsupported URL: https://x.com/home"), "Unsupported URL: https://x.com/home");
        assert_eq!(error_from(""), "yt-dlp stopped");
        assert!(error_from(&format!("ERROR: {}", "a".repeat(300))).ends_with('…'));
    }

    #[test]
    fn the_address_comes_last_after_a_double_dash() {
        let a = args("https://x.com/a/status/1", Path::new("C:/in/3"), Some(Path::new("C:/in/3/cookies.txt")), None);
        let s: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert_eq!(s[s.len() - 2..], ["--".to_string(), "https://x.com/a/status/1".to_string()]);
        let at = s.iter().position(|x| x == "--cookies").unwrap();
        assert_eq!(s[at + 1], "C:/in/3/cookies.txt");
        assert!(s.contains(&"after_move:filepath".to_string()));
        assert!(!s.contains(&"--ffmpeg-location".to_string()));
    }
}
