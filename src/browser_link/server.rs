//! The listening thread and what each request does. The UI thread hears
//! of what came in through `Link::try_event`, each frame.

use super::http::{self, Request};
use super::queue::{self, Queue};
use super::tokens::Tokens;
use super::{Extra, Source};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The biggest file taken: a long video, not a film.
const MAX_BODY: u64 = 2 << 30;
/// How long the extension's "may I connect?" waits for the user.
const PAIR_WAIT: Duration = Duration::from_secs(120);
/// A connection that sends nothing for this long is dropped.
const IDLE: Duration = Duration::from_secs(30);
/// A body turned away is still read, up to this much, so the answer
/// reaches the sender: one still writing would see the connection reset
/// instead, and could not tell "not connected" from "not running".
const DRAIN: u64 = 256 << 20;
/// The most a site's cookies, sent along for a download, may weigh.
const MAX_COOKIES: u64 = 1 << 20;
/// The most a picture sent to be checked or looked for may weigh.
const MAX_PICTURE: u64 = 64 << 20;
/// How long a check waits for the app's pictures to be read.
const CHECK_WAIT: Duration = Duration::from_secs(20);

/// What came in, for the UI thread.
pub enum Event {
    /// A picture, GIF or video, saved to `path` (in the `incoming`
    /// folder, under its own name), where it came from, and what the user
    /// asked for with it.
    Media { path: PathBuf, source: Source, extra: Extra },
    /// Which slides hold a copy of this picture (`Print::duplicate_of`):
    /// their indexes and names go back on `reply`.
    Check { print: crate::tools::similar::Print, reply: Sender<Vec<(usize, String)>> },
    /// Find pictures like the one saved at `path`, in the app's find.
    Find(PathBuf),
    /// An extension asks to connect, calling itself `client`; the user's
    /// answer goes back on `reply`.
    Pair { client: String, reply: Sender<bool> },
    /// Something for the user to read: a download started or failed.
    Note(String),
    /// A stream video was sent and yt-dlp is not installed: offer to
    /// install it.
    NeedYtdlp,
    /// The user picked who made the picture known by `key` on `slide`
    /// ("Find artist"): tag it so.
    Artist { slide: usize, key: String, source: Source },
}

/// "Find artist" under way: how many pictures are answered of how many,
/// and the one being looked up now (its slide, and what it is known by
/// there: `browser_link::shape_key`).
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub answered: usize,
    pub total: usize,
    pub slide: usize,
    pub key: String,
}

/// An answer: its status, type and bytes.
struct Reply {
    status: u16,
    ctype: &'static str,
    body: Vec<u8>,
}

impl From<(u16, String)> for Reply {
    fn from((status, json): (u16, String)) -> Reply {
        Reply { status, ctype: "application/json", body: json.into_bytes() }
    }
}

/// The page "Find artist" opens in the browser: the extension sees it
/// and takes the tab over. Nothing in it but what to do if it doesn't.
const QUEUE_PAGE: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>On-Screen Notes</title>
<style>body{font:15px/1.5 system-ui,sans-serif;background:#18181b;color:#f4f4f5;display:grid;place-items:center;height:100vh;margin:0}
main{max-width:460px;padding:16px}p.muted{color:#a1a1aa;font-size:13px}</style></head>
<body><main><p>Handing the pictures to the On-Screen Notes extension…</p>
<p class="muted" id="none" hidden>Nothing happened? The extension isn't in this browser, or this tab was open before it was added.
Open this address in the browser that has it, or reload this tab.</p></main>
<script>setTimeout(function(){if(!document.documentElement.dataset.osnTaken)document.getElementById("none").hidden=false},4000)</script>
</body></html>"#;

/// What the listening thread and its connections share.
struct Shared {
    port: u16,
    tokens: Mutex<Tokens>,
    events: Mutex<Sender<Event>>,
    ctx: egui::Context,
    incoming: PathBuf,
    count: AtomicU64,
    /// "Find artist"'s pictures.
    queue: Mutex<Queue>,
    /// The slides to offer the extension, as `/v1/slides` answers (the
    /// UI thread keeps it up to date).
    slides: Mutex<String>,
}

/// The server, while it runs. Dropping it stops it.
pub struct Link {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    events: Receiver<Event>,
    failed: Option<String>,
}

impl Link {
    /// Listen on 127.0.0.1:`port`. Files come in under `incoming`, which
    /// is emptied first: anything there is left from an earlier run.
    /// `ctx` is woken when something comes in.
    pub fn start(port: u16, ctx: egui::Context, incoming: PathBuf, tokens: Tokens) -> Link {
        let (tx, rx) = mpsc::channel();
        clear_dir(&incoming);
        let shared = Arc::new(Shared {
            port,
            tokens: Mutex::new(tokens),
            events: Mutex::new(tx),
            ctx,
            incoming,
            count: AtomicU64::new(0),
            queue: Mutex::new(Queue::default()),
            slides: Mutex::new("{\"active\":0,\"slides\":[]}".to_string()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let failed = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
                std::thread::Builder::new()
                    .name("osn-browser-link".into())
                    .spawn(move || listen(listener, &shared, &stop))
                    .err()
                    .map(|e| format!("couldn't start: {e}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Some(format!("port {port} is in use by another program")),
            Err(e) => Some(format!("can't listen on port {port}: {e}")),
        };
        if let Some(f) = &failed {
            log::warn!("browser link: {f}");
        }
        Link { shared, stop, events: rx, failed }
    }

    /// One line for the settings.
    pub fn status(&self) -> String {
        if let Some(f) = &self.failed {
            return format!("Not listening: {f}");
        }
        let names = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner()).browsers();
        let browsers = if names.is_empty() { "no browser connected yet".to_string() } else { format!("connected: {}", names.join(", ")) };
        format!("Listening on 127.0.0.1:{} · {browsers}", self.shared.port)
    }

    pub fn try_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Every connected extension is let go: each asks again to send.
    pub fn forget_browsers(&self) {
        self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner()).forget_all();
    }

    /// Whether it is listening.
    pub fn is_up(&self) -> bool {
        self.failed.is_none()
    }

    /// The address "Find artist" opens in the browser.
    pub fn queue_page(&self) -> String {
        format!("http://127.0.0.1:{}/v1/queue", self.shared.port)
    }

    /// Hand these pictures to the extension, in place of any waiting.
    pub fn find_artists(&self, list: Vec<(usize, String, queue::Picture)>) {
        self.queue().start(list);
    }

    /// How far "Find artist" is, while pictures wait for the extension.
    pub fn finding(&self) -> Option<Finding> {
        let q = self.queue();
        let (slide, key) = q.current()?;
        let (answered, total) = q.progress();
        Some(Finding { answered, total, slide, key })
    }

    pub fn stop_finding(&self) {
        self.queue().clear();
    }

    /// The picture known by `key` on `slide` got tag `tag` another way (a
    /// copy of it was tagged): it is not looked up.
    pub fn found_already(&self, slide: usize, key: &str, tag: &str) {
        self.queue().resolve_spot(slide, key, queue::State::Tagged(tag.to_string()));
    }

    /// The slides to offer: the open one, then the most recently visited,
    /// as `(index, name)`.
    pub fn set_slides(&self, active: usize, slides: &[(usize, String)]) {
        let list: Vec<String> = slides.iter().map(|(i, n)| format!("{{\"index\":{i},\"name\":{}}}", http::json_str(n))).collect();
        let json = format!("{{\"active\":{active},\"slides\":[{}]}}", list.join(","));
        let mut s = self.shared.slides.lock().unwrap_or_else(|e| e.into_inner());
        if *s != json {
            *s = json;
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn listen(listener: TcpListener, shared: &Arc<Shared>, stop: &AtomicBool) {
    // Not blocking, so the stop flag is seen within a tick.
    if let Err(e) = listener.set_nonblocking(true) {
        log::warn!("browser link: {e}");
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(shared);
                let spawned = std::thread::Builder::new().name("osn-browser-request".into()).spawn(move || serve(stream, shared));
                if let Err(e) = spawned {
                    log::warn!("browser link: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => {
                log::warn!("browser link: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// One request, answered.
fn serve(mut stream: TcpStream, shared: Arc<Shared>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(IDLE));
    let _ = stream.set_write_timeout(Some(IDLE));
    let reply = match http::read_head(&mut stream) {
        Ok(req) => {
            let mut taken = false;
            let answered = answer(&req, &mut stream, &shared, &mut taken);
            if !taken {
                drain(&req, &mut stream);
            }
            answered
        }
        Err(_) => (400, http::error_json("bad request")).into(),
    };
    let _ = http::respond_typed(&mut stream, reply.status, reply.ctype, &reply.body);
}

/// The answer to `req`; `taken` is set once its body is read.
fn answer(req: &Request, stream: &mut impl Read, shared: &Arc<Shared>, taken: &mut bool) -> Reply {
    // The page the browser opens for "Find artist": a plain visit, so it
    // has no extension header; it holds nothing.
    if req.method == "GET" && req.path == "/v1/queue" {
        if !http::host_ok(req.header("host"), shared.port) {
            return (403, http::error_json("host")).into();
        }
        return Reply { status: 200, ctype: "text/html; charset=utf-8", body: QUEUE_PAGE.as_bytes().to_vec() };
    }
    if let Err(refused) = admit(req, shared.port) {
        return refused.into();
    }
    let token_ok = || shared.tokens.lock().unwrap_or_else(|e| e.into_inner()).use_token(req.header("x-osn-token").unwrap_or(""));
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/status") => {
            let paired = token_ok();
            let ytdlp = paired && crate::media::ytdlp::find().is_some();
            let version = http::json_str(env!("CARGO_PKG_VERSION"));
            (200, format!("{{\"app\":\"on-screen-notes\",\"version\":{version},\"paired\":{paired},\"ytdlp\":{ytdlp}}}")).into()
        }
        ("POST", "/v1/pair") => pair(req, shared).into(),
        (_, path) if path != "/v1/status" && !token_ok() => (401, http::error_json("pair")).into(),
        ("POST", "/v1/media") => receive(req, stream, shared, taken).into(),
        ("POST", "/v1/link") => link(req, stream, shared, taken).into(),
        ("GET", "/v1/slides") => (200, shared.slides.lock().unwrap_or_else(|e| e.into_inner()).clone()).into(),
        ("POST", "/v1/check") => check(req, stream, shared, taken).into(),
        ("POST", "/v1/find") => find(req, stream, shared, taken).into(),
        (method, path) if path.starts_with("/v1/queue/") => finding(method, &path["/v1/queue/".len()..], req, shared),
        _ => (404, http::error_json("not found")).into(),
    }
}

/// "Find artist"'s requests, under `/v1/queue/`: `next`, `stop`,
/// `<id>/picture`, `<id>/artist`, `<id>/skip`.
fn finding(method: &str, rest: &str, req: &Request, shared: &Shared) -> Reply {
    let q = || shared.queue.lock().unwrap_or_else(|e| e.into_inner());
    match (method, rest) {
        ("GET", "next") => {
            let json = match q().next() {
                queue::Next::Item { id, index, total } => format!("{{\"done\":false,\"id\":{id},\"index\":{index},\"total\":{total}}}"),
                queue::Next::Done { tagged, skipped, total } => format!("{{\"done\":true,\"tagged\":{tagged},\"skipped\":{skipped},\"total\":{total}}}"),
            };
            (200, json).into()
        }
        ("POST", "stop") => {
            let had = q().is_waiting();
            q().clear();
            if had {
                tell(shared, Event::Note("Stopped finding artists".into()));
            }
            (200, "{\"ok\":true}".to_string()).into()
        }
        _ => {
            let Some((id, what)) = rest.split_once('/').and_then(|(id, what)| Some((id.parse::<u64>().ok()?, what))) else {
                return (404, http::error_json("not found")).into();
            };
            match (method, what) {
                ("GET", "picture") => {
                    let Some(picture) = q().picture(id) else { return (404, http::error_json("no such picture")).into() };
                    match queue::jpeg(&picture) {
                        Ok(bytes) => Reply { status: 200, ctype: "image/jpeg", body: bytes },
                        Err(e) => (500, http::error_json(&e)).into(),
                    }
                }
                ("POST", "artist") => {
                    let source = source_of(req);
                    if source.author.is_empty() {
                        return (400, http::error_json("no name")).into();
                    }
                    let Some((slide, key)) = q().resolve(id, queue::State::Tagged(source.author.clone())) else {
                        return (404, http::error_json("not waiting")).into();
                    };
                    tell(shared, Event::Artist { slide, key, source });
                    (200, "{\"ok\":true}".to_string()).into()
                }
                ("POST", "skip") => match q().resolve(id, queue::State::Skipped) {
                    Some(_) => {
                        shared.ctx.request_repaint();
                        (200, "{\"ok\":true}".to_string()).into()
                    }
                    None => (404, http::error_json("not waiting")).into(),
                },
                _ => (404, http::error_json("not found")).into(),
            }
        }
    }
}

/// Turn away whatever is not the extension: another host name, a page's
/// preflight, a request without the extension's header, or one a web
/// page sent. `Err` holds the answer.
fn admit(req: &Request, port: u16) -> Result<(), (u16, String)> {
    if !http::host_ok(req.header("host"), port) {
        return Err((403, http::error_json("host")));
    }
    // A page must ask before it sends `X-OSN` (CORS); the answer is no.
    if req.method == "OPTIONS" {
        return Err((403, http::error_json("no")));
    }
    if req.header("x-osn").is_none() {
        return Err((403, http::error_json("not the extension")));
    }
    if req.header("origin").is_some_and(|o| o.starts_with("http:") || o.starts_with("https:")) {
        return Err((403, http::error_json("not the extension")));
    }
    Ok(())
}

fn pair(req: &Request, shared: &Shared) -> (u16, String) {
    let mut client: String = req.text("x-osn-client").chars().filter(|c| !c.is_control()).take(80).collect();
    if client.trim().is_empty() {
        client = "A browser extension".into();
    }
    let (tx, rx) = mpsc::channel();
    if shared.events.lock().unwrap_or_else(|e| e.into_inner()).send(Event::Pair { client: client.clone(), reply: tx }).is_err() {
        return (503, http::error_json("app closing"));
    }
    shared.ctx.request_repaint();
    match rx.recv_timeout(PAIR_WAIT) {
        Ok(true) => {
            // Its own id for itself: connecting again replaces its old token.
            let install: String = req.text("x-osn-install").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
            let token = shared.tokens.lock().unwrap_or_else(|e| e.into_inner()).add(&client, &install);
            (200, format!("{{\"token\":{}}}", http::json_str(&token)))
        }
        Ok(false) => (403, http::error_json("denied")),
        Err(_) => (408, http::error_json("no answer")),
    }
}

/// Take the file in, then tell the UI.
fn receive(req: &Request, stream: &mut impl Read, shared: &Shared, taken: &mut bool) -> (u16, String) {
    let Some(len) = req.content_length() else { return (411, http::error_json("length")) };
    if len > MAX_BODY {
        return (413, http::error_json("too big"));
    }
    let name = req.text("x-osn-name");
    let Some(ext) = ext_for(req.header("content-type").unwrap_or(""), &name) else {
        return (415, http::error_json("not a picture, GIF or video"));
    };
    let n = shared.count.fetch_add(1, Ordering::Relaxed);
    let dir = shared.incoming.join(n.to_string());
    let path = dir.join(file_name(&name, ext));
    *taken = true;
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| write_body(&path, &req.body_start, stream, len)) {
        log::warn!("browser link: {e}");
        let _ = std::fs::remove_dir_all(&dir);
        return (400, http::error_json("cut short"));
    }
    let source = source_of(req);
    if shared.events.lock().unwrap_or_else(|e| e.into_inner()).send(Event::Media { path, source, extra: extra_of(req) }).is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return (503, http::error_json("app closing"));
    }
    shared.ctx.request_repaint();
    (200, "{\"ok\":true}".to_string())
}

/// Read and drop what is left of a body not taken (see `DRAIN`).
fn drain(req: &Request, stream: &mut impl Read) {
    let Some(len) = req.content_length() else { return };
    let left = len.saturating_sub(req.body_start.len() as u64);
    if left > 0 && left <= DRAIN {
        let _ = std::io::copy(&mut stream.take(left), &mut std::io::sink());
    }
}

/// Where it came from, as the extension's headers say.
fn source_of(req: &Request) -> Source {
    Source {
        page: req.text("x-osn-page"),
        post: req.text("x-osn-post"),
        author: super::clean_handle(&req.text("x-osn-author")),
        site: req.text("x-osn-site").chars().filter(|c| !c.is_control()).take(40).collect(),
        media: req.text("x-osn-media"),
    }
}

/// What the user asked for with it, as the extension's headers say.
fn extra_of(req: &Request) -> Extra {
    let tags = crate::canvas::shape::parse_tags(&req.text("x-osn-tags")).into_iter().take(32).map(|t| t.chars().take(64).collect()).collect();
    let slide = req.header("x-osn-slide").and_then(|s| s.trim().parse::<usize>().ok()).map(|i| (i, req.text("x-osn-slide-name")));
    Extra { tags, slide }
}

/// Read a picture's bytes (at most `MAX_PICTURE`) from the body.
fn picture_body(req: &Request, stream: &mut impl Read, taken: &mut bool) -> Result<Vec<u8>, (u16, String)> {
    let Some(len) = req.content_length() else { return Err((411, http::error_json("length"))) };
    if len > MAX_PICTURE {
        return Err((413, http::error_json("too big")));
    }
    *taken = true;
    let mut body = req.body_start[..req.body_start.len().min(len as usize)].to_vec();
    let rest = len - body.len() as u64;
    if stream.take(rest).read_to_end(&mut body).map_or(true, |n| n as u64 != rest) {
        return Err((400, http::error_json("cut short")));
    }
    Ok(body)
}

/// Is this picture on a slide already? `{"found":[{"index":3,"name":"…"}]}`.
fn check(req: &Request, stream: &mut impl Read, shared: &Shared, taken: &mut bool) -> (u16, String) {
    let body = match picture_body(req, stream, taken) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let Some(print) = crate::tools::similar::Print::of_bytes(&body) else { return (415, http::error_json("not a picture")) };
    let (tx, rx) = mpsc::channel();
    if !tell(shared, Event::Check { print, reply: tx }) {
        return (503, http::error_json("app closing"));
    }
    match rx.recv_timeout(CHECK_WAIT) {
        Ok(found) => {
            let list: Vec<String> = found.iter().map(|(i, n)| format!("{{\"index\":{i},\"name\":{}}}", http::json_str(n))).collect();
            (200, format!("{{\"found\":[{}]}}", list.join(",")))
        }
        Err(_) => (503, http::error_json("still reading the slides")),
    }
}

/// Open the app's find with pictures like this one.
fn find(req: &Request, stream: &mut impl Read, shared: &Shared, taken: &mut bool) -> (u16, String) {
    let body = match picture_body(req, stream, taken) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let ext = match image::guess_format(&body) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::WebP) => "webp",
        Ok(image::ImageFormat::Gif) => "gif",
        Ok(image::ImageFormat::Bmp) => "bmp",
        _ => return (415, http::error_json("not a picture")),
    };
    let n = shared.count.fetch_add(1, Ordering::Relaxed);
    let dir = shared.incoming.join(n.to_string());
    let path = dir.join(format!("look-for.{ext}"));
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &body)) {
        return (500, http::error_json(&e.to_string()));
    }
    if !tell(shared, Event::Find(path)) {
        return (503, http::error_json("app closing"));
    }
    (200, "{\"ok\":true}".to_string())
}

fn tell(shared: &Shared, event: Event) -> bool {
    let sent = shared.events.lock().unwrap_or_else(|e| e.into_inner()).send(event).is_ok();
    shared.ctx.request_repaint();
    sent
}

/// A video the page only streams: download it from its post with yt-dlp
/// (the body holds the site's cookies, in the Netscape format yt-dlp
/// reads, or nothing). Answered at once; the download goes on in its own
/// thread, and each file it brings lands like a sent one.
fn link(req: &Request, stream: &mut impl Read, shared: &Arc<Shared>, taken: &mut bool) -> (u16, String) {
    let url = req.text("x-osn-link");
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 4000 {
        return (400, http::error_json("no address to download from"));
    }
    let Some(exe) = crate::media::ytdlp::find() else {
        tell(shared, Event::NeedYtdlp);
        return (503, http::error_json("ytdlp"));
    };
    let len = req.content_length().unwrap_or(0);
    if len > MAX_COOKIES {
        return (413, http::error_json("too big"));
    }
    let n = shared.count.fetch_add(1, Ordering::Relaxed);
    let dir = shared.incoming.join(n.to_string());
    let cookies = (len > 0).then(|| dir.join("cookies.txt"));
    *taken = true;
    let written = std::fs::create_dir_all(&dir).and_then(|_| match &cookies {
        Some(c) => write_body(c, &req.body_start, stream, len),
        None => Ok(()),
    });
    if let Err(e) = written {
        log::warn!("browser link: {e}");
        let _ = std::fs::remove_dir_all(&dir);
        return (400, http::error_json("cut short"));
    }
    let source = source_of(req);
    let extra = extra_of(req);
    let shared = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("osn-ytdlp".into()).spawn(move || {
        let site = if source.site.is_empty() { String::new() } else { format!(" from {}", source.site) };
        tell(&shared, Event::Note(format!("Downloading the video{site}…")));
        let mut got = crate::media::ytdlp::download(&exe, &url, &dir, cookies.as_deref());
        // Sites change, and yt-dlp follows them: an old one is often why a
        // download fails. Updated once a run, then tried again.
        let unreachable = got.as_ref().err().is_some_and(|e| e.starts_with(crate::media::ytdlp::UNREACHABLE));
        if got.is_err() && !unreachable && crate::media::ytdlp::claim_update() {
            tell(&shared, Event::Note("Updating yt-dlp, then trying again…".into()));
            match crate::media::ytdlp::update(&exe) {
                Ok(true) => got = crate::media::ytdlp::download(&exe, &url, &dir, cookies.as_deref()),
                Ok(false) => {}
                Err(e) => log::warn!("yt-dlp update: {e}"),
            }
        }
        // The site's cookies go as soon as yt-dlp is done with them.
        if let Some(c) = &cookies {
            let _ = std::fs::remove_file(c);
        }
        match got {
            Ok(files) => {
                for path in files {
                    tell(&shared, Event::Media { path, source: source.clone(), extra: extra.clone() });
                }
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                log::warn!("yt-dlp could not download {url}: {e}");
                tell(&shared, Event::Note(format!("Couldn't download the video: {e}")));
            }
        }
    });
    if let Err(e) = spawned {
        return (503, http::error_json(&e.to_string()));
    }
    (202, "{\"ok\":true}".to_string())
}

/// Write exactly `len` bytes: those that came with the head, then the rest.
fn write_body(path: &Path, start: &[u8], stream: &mut impl Read, len: u64) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    let first = start.len().min(len as usize);
    out.write_all(&start[..first])?;
    let rest = len - first as u64;
    let copied = std::io::copy(&mut stream.take(rest), &mut out)?;
    if copied != rest {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the file was cut short"));
    }
    out.flush()
}

/// The extension a file of this type is imported as, from its type, or
/// else its name. `None` for anything else.
pub(super) fn ext_for(content_type: &str, name: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let by_type = match mime.as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/gif" => Some("gif"),
        "video/mp4" => Some("mp4"),
        "video/x-m4v" => Some("m4v"),
        "video/webm" => Some("webm"),
        "video/quicktime" => Some("mov"),
        "video/x-matroska" => Some("mkv"),
        _ => None,
    };
    by_type.or_else(|| {
        let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
        let known = crate::media::import::IMAGE_EXTS.iter().chain(crate::media::import::VIDEO_EXTS).chain(&["gif"]);
        known.copied().find(|k| *k == ext)
    })
}

/// A safe file name from what the page called it, ending in `ext`.
pub(super) fn file_name(name: &str, ext: &str) -> String {
    // Read by hand, not as a `Path`: on Windows "a:b" would be a drive.
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    let mut clean: String = stem.chars().map(|c| if c.is_alphanumeric() || "-_. ".contains(c) { c } else { '_' }).take(80).collect();
    clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        clean = "from-browser".into();
    }
    format!("{clean}.{ext}")
}

/// Remove what is in `dir` (a run's leftovers), keeping `dir`.
fn clear_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let r = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        if let Err(err) = r {
            log::warn!("could not clear {}: {err}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(text: &str) -> Request {
        http::read_head(&mut text.as_bytes()).unwrap()
    }

    #[test]
    fn only_the_extension_is_let_in() {
        let ok = req("GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1:9\r\nX-OSN: 1\r\nOrigin: moz-extension://abc\r\n\r\n");
        assert!(admit(&ok, 9).is_ok());
        let chrome = req("GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1:9\r\nX-OSN: 1\r\nOrigin: chrome-extension://abc\r\n\r\n");
        assert!(admit(&chrome, 9).is_ok());
        let page = req("POST /v1/pair HTTP/1.1\r\nHost: 127.0.0.1:9\r\nX-OSN: 1\r\nOrigin: https://evil.example\r\n\r\n");
        assert_eq!(admit(&page, 9).unwrap_err().0, 403);
        let preflight = req("OPTIONS /v1/media HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n");
        assert_eq!(admit(&preflight, 9).unwrap_err().0, 403);
        let plain = req("POST /v1/media HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n");
        assert_eq!(admit(&plain, 9).unwrap_err().0, 403);
        let rebound = req("GET /v1/status HTTP/1.1\r\nHost: evil.example:9\r\nX-OSN: 1\r\n\r\n");
        assert_eq!(admit(&rebound, 9).unwrap_err().0, 403);
    }

    #[test]
    fn files_are_typed_by_their_type_then_their_name() {
        assert_eq!(ext_for("image/jpeg", "x"), Some("jpg"));
        assert_eq!(ext_for("video/mp4; codecs=avc1", ""), Some("mp4"));
        assert_eq!(ext_for("application/octet-stream", "clip.WEBM"), Some("webm"));
        assert_eq!(ext_for("", "a.gif"), Some("gif"));
        assert_eq!(ext_for("image/avif", "a.avif"), None);
        assert_eq!(ext_for("text/html", "page.html"), None);
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(file_name("GxY12abc.jpg", "jpg"), "GxY12abc.jpg");
        // Only the last part of a path is kept.
        assert_eq!(file_name("../../evil\\name.png", "png"), "name.png");
        assert_eq!(file_name("../../evil.png", "png"), "evil.png");
        assert_eq!(file_name("", "mp4"), "from-browser.mp4");
        assert_eq!(file_name("...", "gif"), "from-browser.gif");
        assert_eq!(file_name("a:b*c?.webp", "webp"), "a_b_c_.webp");
    }

    #[test]
    fn a_body_is_written_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("osn-body-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.bin");
        write_body(&p, b"abc", &mut &b"defgh"[..], 6).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"abcdef");
        assert!(write_body(&p, b"ab", &mut &b"c"[..], 6).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Find artist" through a real socket: the page, the next picture as
    /// a JPEG, an artist and a skip, then done; and none of it without
    /// the token.
    #[test]
    fn the_extension_works_through_find_artist() {
        let dir = std::env::temp_dir().join(format!("osn-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut tokens = Tokens::load_from(dir.join("browser-link.ron"));
        let token = tokens.add("Test", "");
        let port = {
            let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            probe.local_addr().unwrap().port()
        };
        let link = Link::start(port, egui::Context::default(), dir.join("incoming"), tokens);
        let ask = |method: &str, path: &str, extra: &str, token: &str| -> (String, Vec<u8>) {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let text = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-OSN: 1\r\nX-OSN-Token: {token}\r\n{extra}Content-Length: 0\r\n\r\n");
            s.write_all(text.as_bytes()).unwrap();
            let mut out = Vec::new();
            s.read_to_end(&mut out).unwrap();
            let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            (String::from_utf8_lossy(&out[..split]).into_owned(), out[split + 4..].to_vec())
        };
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 10, 10, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let png = std::sync::Arc::new(png);
        assert!(link.finding().is_none());
        link.find_artists(vec![(2, "a.png".into(), queue::Picture::Bytes(png.clone())), (2, "b.png".into(), queue::Picture::Bytes(png))]);
        assert_eq!(link.finding(), Some(Finding { answered: 0, total: 2, slide: 2, key: "a.png".into() }));
        // The page the browser opens needs no token, and says nothing.
        let (head, body) = ask("GET", "/v1/queue", "", "");
        assert!(head.contains("200 OK") && head.contains("text/html"), "{head}");
        assert!(String::from_utf8_lossy(&body).contains("On-Screen Notes extension"));
        assert!(ask("GET", "/v1/queue/next", "", "nope").0.contains("401"));
        let (_, next) = ask("GET", "/v1/queue/next", "", &token);
        let next = String::from_utf8(next).unwrap();
        assert!(next.starts_with("{\"done\":false,\"id\":"), "{next}");
        let id: u64 = next.split("\"id\":").nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
        let (head, jpeg) = ask("GET", &format!("/v1/queue/{id}/picture"), "", &token);
        assert!(head.contains("image/jpeg"), "{head}");
        assert_eq!(image::load_from_memory(&jpeg).unwrap().width(), 40);
        assert!(ask("POST", &format!("/v1/queue/{id}/artist"), "", &token).0.contains("400"), "no name, no tag");
        let (head, _) = ask("POST", &format!("/v1/queue/{id}/artist"), "X-OSN-Author: %40Antin_Illust\r\nX-OSN-Site: X\r\n", &token);
        assert!(head.contains("200 OK"), "{head}");
        let ev = (0..200).find_map(|_| link.try_event().or_else(|| { std::thread::sleep(Duration::from_millis(10)); None }));
        match ev {
            Some(Event::Artist { slide, key, source }) => {
                assert_eq!((slide, key.as_str(), source.author.as_str(), source.site.as_str()), (2, "a.png", "Antin_Illust", "X"));
            }
            _ => panic!("no artist event"),
        }
        assert!(ask("POST", &format!("/v1/queue/{id}/artist"), "X-OSN-Author: x\r\n", &token).0.contains("404"), "answered once");
        let (_, next) = ask("GET", "/v1/queue/next", "", &token);
        let second: u64 = String::from_utf8(next).unwrap().split("\"id\":").nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
        assert!(ask("POST", &format!("/v1/queue/{second}/skip"), "", &token).0.contains("200 OK"));
        let (_, done) = ask("GET", "/v1/queue/next", "", &token);
        assert_eq!(String::from_utf8(done).unwrap(), "{\"done\":true,\"tagged\":1,\"skipped\":1,\"total\":2}");
        assert!(link.finding().is_none());
        drop(link);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole way through a real socket: refused until paired, then
    /// the file lands in `incoming` and the UI hears of it.
    #[test]
    fn a_paired_extension_sends_a_picture() {
        let dir = std::env::temp_dir().join(format!("osn-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let port = {
            let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            probe.local_addr().unwrap().port()
        };
        let link = Link::start(port, egui::Context::default(), dir.join("incoming"), Tokens::load_from(dir.join("browser-link.ron")));
        assert!(link.status().starts_with("Listening"), "{}", link.status());
        let send = |text: &str, body: &[u8]| -> String {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(text.as_bytes()).unwrap();
            s.write_all(body).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        let media = |token: &str| {
            format!(
                "POST /v1/media HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-OSN: 1\r\nX-OSN-Token: {token}\r\nContent-Type: image/png\r\nContent-Length: 4\r\nX-OSN-Name: pic%201.png\r\nX-OSN-Author: %40Antin_Illust\r\nX-OSN-Site: X\r\nX-OSN-Post: https%3A%2F%2Fx.com%2FAntin_Illust%2Fstatus%2F1\r\n\r\n"
            )
        };
        assert!(send(&media("nope"), b"\x89PNG").contains("401"));
        // Turned away with a big body still coming: the answer arrives
        // whole rather than as a reset connection.
        let big = vec![7u8; 4 << 20];
        let head = media("nope").replace("Content-Length: 4", &format!("Content-Length: {}", big.len()));
        assert!(send(&head, &big).contains("401 Unauthorized"));
        // Pairing waits on the UI: answer it from here.
        let asker = std::thread::spawn(move || {
            let text = format!("POST /v1/pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-OSN: 1\r\nX-OSN-Client: Firefox\r\nContent-Length: 0\r\n\r\n");
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(text.as_bytes()).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        });
        let ev = (0..200).find_map(|_| link.try_event().or_else(|| { std::thread::sleep(Duration::from_millis(10)); None }));
        match ev {
            Some(Event::Pair { client, reply }) => {
                assert_eq!(client, "Firefox");
                reply.send(true).unwrap();
            }
            _ => panic!("no pair request"),
        }
        let answer = asker.join().unwrap();
        assert!(answer.contains("200 OK"), "{answer}");
        let token = answer.split("\"token\":\"").nth(1).unwrap().split('"').next().unwrap().to_string();
        let ok = send(&media(&token), b"\x89PNG");
        assert!(ok.contains("{\"ok\":true}"), "{ok}");
        let ev = (0..200).find_map(|_| link.try_event().or_else(|| { std::thread::sleep(Duration::from_millis(10)); None }));
        match ev {
            Some(Event::Media { path, source, .. }) => {
                assert_eq!(path.file_name().unwrap(), "pic 1.png");
                assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG");
                assert_eq!(source.author, "Antin_Illust");
                assert_eq!(source.site, "X");
                assert_eq!(source.post, "https://x.com/Antin_Illust/status/1");
            }
            _ => panic!("no media"),
        }
        assert!(link.status().contains("connected: Firefox"), "{}", link.status());
        drop(link);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
