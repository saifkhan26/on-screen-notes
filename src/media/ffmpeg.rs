//! Finding, running and installing FFmpeg.
//!
//! Video decoding and MP4 recording both run `ffmpeg` as a child
//! process, so nothing is linked and the app builds without FFmpeg
//! present. This module finds the binary, in order:
//!
//!   1. the path set in config (`ffmpeg_path`: the exe or its folder),
//!   2. the folder the app's own exe sits in (a portable bundle),
//!   3. WinGet's link folders, per-user and machine-wide,
//!   4. WinGet's package folder for the `Gyan.FFmpeg*` packages,
//!   5. `PATH`.
//!
//! Steps 3 and 4 matter right after a one-click install: WinGet puts
//! the new folder on the *user's* PATH, but this already-running
//! process keeps the PATH it started with.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The `ffmpeg` / `ffprobe` pair in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// `(major, minor)` from `ffmpeg -version`; `None` for builds that
    /// do not print one (git snapshots), which are taken to be new.
    pub version: Option<(u32, u32)>,
}

impl Tools {
    /// `-fps_mode` replaced `-vsync` in FFmpeg 5.1.
    pub fn has_fps_mode(&self) -> bool {
        self.version.map_or(true, |v| v >= (5, 1))
    }

    /// A `Command` for `ffmpeg` that opens no console window.
    pub fn ffmpeg_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.ffmpeg);
        hide_console(&mut cmd);
        cmd
    }

    /// A `Command` for `ffprobe` that opens no console window.
    pub fn ffprobe_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.ffprobe);
        hide_console(&mut cmd);
        cmd
    }
}

/// Outer `None`: not looked for yet. Inner `None`: looked, not found.
static TOOLS: Mutex<Option<Option<Tools>>> = Mutex::new(None);
/// `AppConfig::ffmpeg_path`, pushed in at startup and by "Locate…".
static CONFIGURED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Use `path` (an `ffmpeg` exe or a folder holding one) before any
/// other location. Forgets the cached result so the next `tools()`
/// looks again.
pub fn set_configured(path: Option<PathBuf>) {
    *CONFIGURED.lock().unwrap_or_else(|e| e.into_inner()) = path;
    *TOOLS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The FFmpeg tools, found on first call and cached. Spawns
/// `ffmpeg -version` per candidate, so the first call costs a few
/// tens of milliseconds.
pub fn tools() -> Option<Tools> {
    let mut cached = TOOLS.lock().unwrap_or_else(|e| e.into_inner());
    if cached.is_none() {
        let configured = CONFIGURED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        *cached = Some(detect(configured.as_deref()));
    }
    cached.clone().flatten()
}

/// Forget the cached result and look again (after an install).
pub fn redetect() -> Option<Tools> {
    *TOOLS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    tools()
}

fn exe(name: &str) -> String {
    if cfg!(target_os = "windows") { format!("{name}.exe") } else { name.to_string() }
}

/// Folders to look in, in priority order (see the module docs).
fn candidate_dirs(configured: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = configured {
        if p.is_file() {
            if let Some(d) = p.parent() { dirs.push(d.to_path_buf()); }
        } else {
            dirs.push(p.to_path_buf());
        }
    }
    if let Some(d) = std::env::current_exe().ok().as_deref().and_then(Path::parent) {
        dirs.push(d.to_path_buf());
    }
    if cfg!(target_os = "windows") {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            let winget = local.join("Microsoft").join("WinGet");
            dirs.push(winget.join("Links"));
            dirs.extend(winget_package_bins(&winget.join("Packages")));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
            dirs.push(pf.join("WinGet").join("Links"));
            dirs.extend(winget_package_bins(&pf.join("WinGet").join("Packages")));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

/// `<packages>/Gyan.FFmpeg*/<build folder>/bin` for every installed
/// Gyan build, newest-looking first.
fn winget_package_bins(packages: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(packages) else { return out };
    for pkg in entries.flatten() {
        if !pkg.file_name().to_string_lossy().starts_with("Gyan.FFmpeg") { continue; }
        let Ok(builds) = std::fs::read_dir(pkg.path()) else { continue };
        let mut bins: Vec<PathBuf> = builds.flatten().map(|b| b.path().join("bin")).filter(|b| b.is_dir()).collect();
        bins.sort();
        bins.reverse();
        out.extend(bins);
    }
    out
}

fn detect(configured: Option<&Path>) -> Option<Tools> {
    // A configured exe may have any name; everywhere else it is `ffmpeg`.
    if let Some(p) = configured.filter(|p| p.is_file()) {
        if let Some(t) = try_tools(p) { return Some(t); }
    }
    for dir in candidate_dirs(configured) {
        let ffmpeg = dir.join(exe("ffmpeg"));
        if ffmpeg.is_file() {
            if let Some(t) = try_tools(&ffmpeg) {
                log::info!("using ffmpeg at {}", t.ffmpeg.display());
                return Some(t);
            }
        }
    }
    log::info!("ffmpeg not found");
    None
}

/// `Some` when `ffmpeg -version` runs and an `ffprobe` sits next to it.
fn try_tools(ffmpeg: &Path) -> Option<Tools> {
    let ffprobe = ffmpeg.parent()?.join(exe("ffprobe"));
    if !ffprobe.is_file() {
        log::warn!("{} has no ffprobe beside it", ffmpeg.display());
        return None;
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let out = hide_console(&mut cmd).output().ok()?;
    if !out.status.success() { return None; }
    let first = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string();
    Some(Tools { ffmpeg: ffmpeg.to_path_buf(), ffprobe, version: parse_version(&first) })
}

/// `(major, minor)` from the first line of `ffmpeg -version`, e.g.
/// `ffmpeg version 7.1-essentials_build-www.gyan.dev …` or
/// `ffmpeg version n6.0 …`.
fn parse_version(line: &str) -> Option<(u32, u32)> {
    let v = line.split_whitespace().skip_while(|w| *w != "version").nth(1)?;
    let v = v.trim_start_matches('n');
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// `CREATE_NO_WINDOW`: keeps a GUI app's console children from
/// flashing a console window.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Apply `CREATE_NO_WINDOW` on Windows; no-op elsewhere.
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Tie `child` to this process's lifetime: on Windows it joins a job
/// object that the OS closes — killing every member — when the app
/// exits for any reason, crash included. Elsewhere a decoder dies on
/// its own when its output pipe breaks.
pub fn kill_with_app(child: &Child) {
    #[cfg(target_os = "windows")]
    win_job::assign(child);
    #[cfg(not(target_os = "windows"))]
    let _ = child;
}

#[cfg(target_os = "windows")]
mod win_job {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// The job handle, created once and never closed: the OS closes it
    /// when the process ends, which is the point.
    static JOB: OnceLock<Option<isize>> = OnceLock::new();

    fn job() -> Option<HANDLE> {
        let raw = JOB.get_or_init(|| unsafe {
            let job = CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            Some(job.0)
        });
        raw.map(HANDLE)
    }

    pub fn assign(child: &std::process::Child) {
        let Some(job) = job() else { return };
        let process = HANDLE(child.as_raw_handle() as isize);
        if let Err(e) = unsafe { AssignProcessToJobObject(job, process) } {
            log::debug!("could not tie ffmpeg to the app's lifetime: {e}");
        }
    }
}

/// What the "Install FFmpeg" button can do on this platform.
pub fn can_install() -> bool {
    cfg!(target_os = "windows")
}

/// One run of `winget install`, watched from a background thread.
pub struct Installer {
    /// `Some(found)` once winget has exited and the app has looked
    /// for ffmpeg again; `Err` when winget could not be started.
    done: Arc<Mutex<Option<Result<bool, String>>>>,
}

impl Installer {
    /// Start `winget install Gyan.FFmpeg.Essentials` in its own visible
    /// console, so the user sees progress and can answer winget's
    /// agreement prompt there.
    pub fn start() -> Self {
        let done = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&done);
        let spawned = std::thread::Builder::new().name("osn-ffmpeg-install".into()).spawn(move || {
            let result = run_winget();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        });
        if let Err(e) = spawned {
            *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(e.to_string()));
        }
        Self { done }
    }

    /// The outcome, once: `Some(Ok(true))` found ffmpeg afterwards,
    /// `Some(Ok(false))` winget ran but ffmpeg is still missing.
    pub fn take_result(&self) -> Option<Result<bool, String>> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

fn run_winget() -> Result<bool, String> {
    let mut cmd = Command::new("winget");
    cmd.args(["install", "--id", "Gyan.FFmpeg.Essentials", "-e", "--source", "winget"]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    let status = cmd.status().map_err(|e| format!("could not start winget: {e}"))?;
    // The exit code is not trusted: "already installed" is non-zero
    // too. Whether ffmpeg can now be found is what counts.
    log::info!("winget exited with {status}");
    Ok(redetect().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_from_common_builds() {
        assert_eq!(parse_version("ffmpeg version 7.1-essentials_build-www.gyan.dev Copyright"), Some((7, 1)));
        assert_eq!(parse_version("ffmpeg version n6.0 Copyright (c) 2000-2023"), Some((6, 0)));
        assert_eq!(parse_version("ffmpeg version 4.4.2-0ubuntu0.22.04.1"), Some((4, 4)));
        assert_eq!(parse_version("ffmpeg version N-112345-gabcdef"), None);
    }

    #[test]
    fn fps_mode_needs_5_1() {
        let t = |version| Tools { ffmpeg: "f".into(), ffprobe: "p".into(), version };
        assert!(!t(Some((5, 0))).has_fps_mode());
        assert!(t(Some((5, 1))).has_fps_mode());
        assert!(t(None).has_fps_mode());
    }

    #[test]
    fn configured_location_is_searched_first() {
        let dir = std::env::temp_dir().join("osn-ffmpeg-test-configured");
        let dirs = candidate_dirs(Some(&dir));
        assert_eq!(dirs.first(), Some(&dir));
    }

    #[test]
    fn winget_package_bins_are_found() {
        let root = std::env::temp_dir().join(format!("osn-ffmpeg-test-{}", std::process::id()));
        let bin = root.join("Gyan.FFmpeg.Essentials_Microsoft.Winget.Source_8wekyb3d8bbwe").join("ffmpeg-7.1-essentials_build").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(root.join("Other.Package").join("x").join("bin")).unwrap();
        assert_eq!(winget_package_bins(&root), vec![bin]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
