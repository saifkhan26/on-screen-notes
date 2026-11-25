// In every page and frame: finds the picture, GIF or video under a
// right-click or under the picker, even beneath the covers sites lay
// over them, reads who posted it, and shows the little notes ("Sent…").
// The fetching and sending is background.js's.
"use strict";
(function () {
  if (globalThis.__osnContentLoaded) return;
  globalThis.__osnContentLoaded = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const S = globalThis.OSNSites;
  const top = window === window.top;

  // Smaller than this on a side is an icon, not a picture worth sending.
  const MIN_SIDE = 40;
  // A video file kept in the page is read whole only below this size.
  const MAX_PAGE_VIDEO = 1024 * 1024 * 1024;
  // Recording stops by itself after this long.
  const MAX_RECORD_S = 180;
  // Bytes per message when a recording or file goes to background.js.
  const CHUNK = 4 * 1024 * 1024;

  // ---- What is under a point ------------------------------------------

  let lastClick = null;
  document.addEventListener(
    "contextmenu",
    (e) => {
      lastClick = { x: e.clientX, y: e.clientY };
    },
    true,
  );
  // Where the pointer is, for "send what is under it" with a key.
  let pointer = null;
  document.addEventListener(
    "mousemove",
    (e) => {
      pointer = { x: e.clientX, y: e.clientY };
    },
    { capture: true, passive: true },
  );

  function shadowOf(el) {
    if ("openOrClosedShadowRoot" in el) return el.openOrClosedShadowRoot; // Firefox
    const dom = globalThis.chrome?.dom;
    if (dom?.openOrClosedShadowRoot) {
      try {
        return dom.openOrClosedShadowRoot(el);
      } catch {
        /* not an element it can read */
      }
    }
    return el.shadowRoot;
  }

  // Every element under (x, y), topmost first, inside shadow roots too.
  function stackAt(x, y) {
    const out = [];
    const walk = (root) => {
      let list;
      try {
        list = root.elementsFromPoint(x, y);
      } catch {
        return;
      }
      for (const el of list) {
        if (out.includes(el)) continue;
        const sr = shadowOf(el);
        if (sr && sr !== root) walk(sr);
        if (!out.includes(el)) out.push(el);
      }
    };
    walk(document);
    return out;
  }

  function contains(r, x, y) {
    return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
  }

  function backgroundUrl(el) {
    let v;
    try {
      v = getComputedStyle(el).backgroundImage;
    } catch {
      return null;
    }
    if (!v || v === "none") return null;
    const m = v.match(/url\((['"]?)(.*?)\1\)/);
    if (!m || m[2].startsWith("data:image/svg")) return null;
    try {
      return new URL(m[2], location.href).href;
    } catch {
      return null;
    }
  }

  // `el` as something to send, or null.
  function asMedia(el) {
    if (!(el instanceof Element)) return null;
    const tag = el.tagName.toUpperCase();
    if (tag === "IMG") return el.currentSrc || el.src ? { el, kind: "image" } : null;
    if (tag === "VIDEO") return { el, kind: "video" };
    if (tag === "CANVAS") return el.width > 0 && el.height > 0 ? { el, kind: "canvas" } : null;
    if (tag === "PICTURE") {
      const img = el.querySelector("img");
      return img ? asMedia(img) : null;
    }
    if (tag === "SVG" || tag === "HTML" || tag === "BODY") return null;
    const bg = backgroundUrl(el);
    return bg ? { el, kind: "background", url: bg } : null;
  }

  function sizeOk(el, min) {
    const r = el.getBoundingClientRect();
    return r.width >= min && r.height >= min;
  }

  // The pictures, GIFs and videos under (x, y), topmost first: those the
  // point hits, then those under covers that let the pointer through
  // (inside the topmost elements), then any whose box holds the point.
  function mediaAt(x, y, min = MIN_SIDE) {
    const found = [];
    const add = (m) => {
      if (m && !found.some((f) => f.el === m.el) && sizeOk(m.el, min)) found.push(m);
    };
    const stack = stackAt(x, y);
    for (const el of stack) add(asMedia(el));
    for (const el of stack.slice(0, 6)) {
      if (el === document.documentElement || el === document.body) continue;
      for (const inner of el.querySelectorAll("img, video, canvas")) {
        if (contains(inner.getBoundingClientRect(), x, y)) add(asMedia(inner));
      }
    }
    if (!found.length) {
      for (const inner of document.querySelectorAll("img, video, canvas")) {
        if (contains(inner.getBoundingClientRect(), x, y)) add(asMedia(inner));
      }
    }
    return found;
  }

  // ---- Who posted it --------------------------------------------------

  function handleInXName(block) {
    for (const span of block.querySelectorAll("span")) {
      const t = span.textContent.trim();
      if (/^@[A-Za-z0-9_]{1,15}$/.test(t)) return t.slice(1);
    }
    const a = block.querySelector('a[href^="/"]');
    const m = a && a.getAttribute("href").match(/^\/([A-Za-z0-9_]{1,15})$/);
    return m ? m[1] : null;
  }

  // X: the closest post around the media (a quoted post inside another
  // is closer than the one quoting it) names its author in a
  // "User-Name" block; its time stamp links to it.
  function fromX(el) {
    for (let a = el; a && a !== document.body; a = a.parentElement) {
      const name = a.querySelector('[data-testid="User-Name"]');
      if (!name) continue;
      const author = handleInXName(name);
      if (!author) return null;
      let post = "";
      const time = a.querySelector('a[href*="/status/"] time');
      const link = time && time.closest("a");
      const p = link && S.xPostFromHref(link.getAttribute("href"), location.href);
      if (p && p.author.toLowerCase() === author.toLowerCase()) post = p.post;
      return { author, site: "X", post };
    }
    return null;
  }

  // Instagram, where the address has no name (the feed): the post's
  // header links to its author's profile first.
  function fromInstagram(el) {
    const box = el.closest('article, div[role="dialog"]');
    if (!box) return null;
    for (const a of box.querySelectorAll("a[href]")) {
      let path;
      try {
        path = new URL(a.getAttribute("href"), location.href).pathname;
      } catch {
        continue;
      }
      const m = path.match(/^\/([A-Za-z0-9._]{1,30})\/?$/);
      if (!m || !S.isIgUser(m[1])) continue;
      let post = "";
      const pl = box.querySelector('a[href*="/p/"], a[href*="/reel/"]');
      if (pl) {
        try {
          const u = new URL(pl.getAttribute("href"), location.href);
          post = u.origin + u.pathname;
        } catch {
          /* no post address */
        }
      }
      return { author: m[1], site: "Instagram", post };
    }
    return null;
  }

  function fromBluesky(el) {
    const item = el.closest('[data-testid^="feedItem-by-"], [data-testid^="postThreadItem-by-"]');
    if (!item) return null;
    const author = S.bskyHandle(item.getAttribute("data-testid").replace(/^(feedItem|postThreadItem)-by-/, ""));
    if (!author) return null;
    const pl = item.querySelector('a[href*="/post/"]');
    let post = "";
    try {
      post = pl ? new URL(pl.getAttribute("href"), location.href).href : "";
    } catch {
      /* no post address */
    }
    return { author, site: "Bluesky", post };
  }

  // Pinterest: a pin's page links to where the picture came from (an
  // Instagram or X post, an ArtStation page…): who that names is the
  // likely artist; the pinner seldom is. Some links go through
  // pinterest.com/offsite/?url=….
  function fromPinterest() {
    if (!/^\/pin\/\d+/.test(location.pathname)) return null;
    let source = "";
    for (const a of document.querySelectorAll("a[href]")) {
      let u;
      try {
        u = new URL(a.href);
      } catch {
        continue;
      }
      if (/(^|\.)pinterest\.[a-z.]+$/.test(u.hostname)) {
        const out = u.pathname.startsWith("/offsite") && u.searchParams.get("url");
        if (!out) continue;
        try {
          u = new URL(out);
        } catch {
          continue;
        }
      }
      if (u.protocol !== "https:" && u.protocol !== "http:") continue;
      const r = S.authorFromLink(u.href, a.textContent);
      if (r) return { ...r, post: r.post || u.href };
      if (!source) source = u.href;
    }
    return source ? { author: "", site: "", post: source } : null;
  }

  function whoPosted(el) {
    const host = location.hostname.replace(/^(www|mobile|m)\./, "");
    const fromUrl = S.authorFromUrl(location.href);
    try {
      if (host === "x.com" || host === "twitter.com") return fromX(el) || fromUrl;
      if (host === "instagram.com") return fromUrl || fromInstagram(el);
      if (host === "bsky.app") return fromBluesky(el) || fromUrl;
      if (/(^|\.)pinterest\.[a-z.]+$/.test(location.hostname)) return fromPinterest() || fromUrl;
    } catch {
      /* a page laid out unlike the one this was written for */
    }
    return fromUrl;
  }

  // ---- What to send ---------------------------------------------------

  function imageUrls(img) {
    const sets = [img.getAttribute("srcset")];
    const picture = img.closest("picture");
    if (picture) for (const s of picture.querySelectorAll("source[srcset]")) sets.push(s.getAttribute("srcset"));
    const best = S.largestFromSrcset(sets, location.href);
    const urls = [best, img.currentSrc, img.src].filter(Boolean);
    return [...new Set(urls.flatMap((u) => S.biggerUrls(u)))];
  }

  function videoUrls(v) {
    const urls = [];
    if (v.currentSrc) urls.push(v.currentSrc);
    if (v.src) urls.push(v.src);
    for (const s of v.querySelectorAll("source[src]")) {
      try {
        urls.push(new URL(s.getAttribute("src"), location.href).href);
      } catch {
        /* a broken address */
      }
    }
    return [...new Set(urls)];
  }

  function blobToDataUrl(blob) {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.onerror = () => reject(r.error);
      r.readAsDataURL(blob);
    });
  }

  // A picture shown from this page's own memory (blob:), or a canvas, as
  // PNG: those have no address the background could fetch.
  function drawn(el) {
    const w = el.naturalWidth || el.width;
    const h = el.naturalHeight || el.height;
    const c = document.createElement("canvas");
    c.width = w;
    c.height = h;
    c.getContext("2d").drawImage(el, 0, 0, w, h);
    return c.toDataURL("image/png");
  }

  // What background.js needs to send `m`. `peek`: only to look at it
  // (check it, look for it), so a video file kept in the page isn't read.
  async function describe(m, peek = false) {
    if (m.desc && !peek) return m.desc;
    const el = m.el;
    const r = el.getBoundingClientRect();
    const who = whoPosted(el) || {};
    const d = {
      kind: m.kind,
      urls: [],
      page: location.href,
      title: document.title,
      author: who.author || "",
      site: who.site || "",
      post: who.post || "",
      name: "",
      rect: { x: r.left, y: r.top, w: r.width, h: r.height },
      vw: window.innerWidth,
      topFrame: top,
    };
    if (m.kind === "image") d.urls = imageUrls(el);
    else if (m.kind === "background") d.urls = S.biggerUrls(m.url);
    else if (m.kind === "video") d.urls = videoUrls(el);
    // What the page shows: small, quick to fetch, enough to compare.
    d.shown = m.kind === "image" ? el.currentSrc || el.src : m.kind === "background" ? m.url : m.kind === "video" ? el.poster || "" : "";
    d.name = S.nameFromUrl(d.urls[0] || "") || (d.author ? `${d.author}` : "");
    const first = d.urls[0] || "";
    try {
      if (m.kind === "canvas") {
        d.dataUrl = el.toDataURL("image/png");
      } else if (m.kind === "image" && first.startsWith("data:")) {
        d.dataUrl = first;
      } else if (m.kind === "image" && first.startsWith("blob:")) {
        d.dataUrl = drawn(el);
      } else if (m.kind === "video" && peek) {
        /* nothing to read */
      } else if (m.kind === "video" && (!first || first.startsWith("blob:") || /\.m3u8(\?|$)/i.test(first))) {
        // A whole file kept in the page can be read here; a stream (most
        // sites' players) cannot: it is downloaded from its post, or
        // recorded as it plays.
        d.stream = true;
        if (first.startsWith("blob:")) {
          const blob = await (await fetch(first)).blob();
          if (blob.size > 0 && blob.size <= MAX_PAGE_VIDEO && /^video\//.test(blob.type)) {
            m.blob = blob;
            d.stream = false;
          }
        }
      }
    } catch {
      /* the background tries what it can */
    }
    return d;
  }

  // ---- Notes on the page ----------------------------------------------

  let toastHost = null;
  let toastTimer = 0;
  function toast(text, kind) {
    if (!top) return;
    if (!toastHost || !toastHost.isConnected) {
      toastHost = document.createElement("osn-toast");
      toastHost.style.cssText = "all: initial; position: fixed; right: 16px; bottom: 16px; z-index: 2147483647;";
      const root = toastHost.attachShadow({ mode: "open" });
      root.innerHTML =
        '<style>div{font:13px/1.4 system-ui,sans-serif;color:#f4f4f5;background:#18181b;border:1px solid #3f3f46;' +
        "border-radius:8px;padding:8px 12px;max-width:360px;box-shadow:0 4px 16px rgba(0,0,0,.35)}" +
        "div.error{border-color:#ef4444}div.ok{border-color:#a1a1aa}</style><div></div>";
      (document.body || document.documentElement).appendChild(toastHost);
    }
    const box = toastHost.shadowRoot.querySelector("div");
    box.textContent = text;
    box.className = kind || "";
    toastHost.style.display = "block";
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toastHost.style.display = "none"), kind === "error" ? 6000 : 3000);
  }

  // A note in the top frame, wherever this frame is.
  function note(text, kind) {
    if (top) toast(text, kind);
    else api.runtime.sendMessage({ type: "osn-relay-toast", text, kind }).catch(() => {});
  }

  // ---- Videos that only stream ----------------------------------------

  function secs(t) {
    return Number.isFinite(t) ? Math.round(t) : 0;
  }

  // Record `v` from its start to its end (or `MAX_RECORD_S`, or Esc) as
  // it plays: a WebM of its pictures, without sound.
  async function recordVideo(v) {
    const capture = v.captureStream ? () => v.captureStream() : v.mozCaptureStream ? () => v.mozCaptureStream() : null;
    if (!capture || typeof MediaRecorder === "undefined") throw new Error("This browser can't record videos");
    const types = ["video/webm;codecs=vp9", "video/webm;codecs=vp8", "video/webm"];
    const mimeType = types.find((t) => MediaRecorder.isTypeSupported(t));
    if (!mimeType) throw new Error("This browser can't record WebM video");
    const was = { loop: v.loop, time: v.currentTime, paused: v.paused };
    v.loop = false;
    try {
      v.currentTime = 0;
    } catch {
      /* a live stream starts where it is */
    }
    await v.play().catch(() => {});
    let stream;
    try {
      stream = new MediaStream(capture().getVideoTracks());
    } catch (e) {
      throw new Error(`The site won't let this video be recorded (${e.name || e})`);
    }
    if (!stream.getVideoTracks().length) throw new Error("This video has no picture to record");
    const rec = new MediaRecorder(stream, { mimeType, videoBitsPerSecond: 8_000_000 });
    const chunks = [];
    rec.ondataavailable = (e) => e.data && e.data.size && chunks.push(e.data);
    const stopped = new Promise((resolve) => (rec.onstop = resolve));
    let failed = null;
    rec.onerror = (e) => (failed = e.error || new Error("recording failed"));
    rec.start(1000);
    const started = performance.now();
    await new Promise((resolve) => {
      let last = v.currentTime;
      const done = () => {
        clearInterval(timer);
        window.removeEventListener("keydown", onKey, true);
        v.removeEventListener("ended", done);
        resolve();
      };
      const onKey = (e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          done();
        }
      };
      const timer = setInterval(() => {
        const t = v.currentTime;
        const total = Number.isFinite(v.duration) ? `${secs(v.duration)} s` : "?";
        note(`Recording the video as it plays — ${secs(t)} of ${total} · Esc stops early`, "");
        const elapsed = (performance.now() - started) / 1000;
        // Past its end, gone back to its start (the site loops it), or long enough.
        if (failed || t + 0.05 < last || (Number.isFinite(v.duration) && t >= v.duration - 0.06) || elapsed > MAX_RECORD_S) done();
        last = Math.max(last, t);
      }, 250);
      v.addEventListener("ended", done);
      window.addEventListener("keydown", onKey, true);
    });
    rec.stop();
    await stopped;
    v.loop = was.loop;
    if (failed) throw new Error(`Recording failed: ${failed.message || failed}`);
    const blob = new Blob(chunks, { type: "video/webm" });
    if (blob.size < 1024) throw new Error("Nothing was recorded — is the video playing?");
    return blob;
  }

  // Hand a file to background.js in pieces (messages can't carry a Blob
  // in Chrome, nor anything very big), which sends it to the app.
  async function uploadBlob(blob, d) {
    const port = api.runtime.connect({ name: "osn-upload" });
    const result = new Promise((resolve) => {
      port.onMessage.addListener((msg) => msg && msg.done && resolve(msg.done));
      port.onDisconnect.addListener(() => resolve({ ok: false, error: "The extension stopped while sending" }));
    });
    port.postMessage({ start: d, type: blob.type, size: blob.size });
    for (let at = 0; at < blob.size; at += CHUNK) {
      const url = await blobToDataUrl(blob.slice(at, at + CHUNK));
      port.postMessage({ chunk: url.slice(url.indexOf(",") + 1) });
    }
    port.postMessage({ end: true });
    const r = await result;
    port.disconnect();
    return r;
  }

  // ---- What else can be done with one ----------------------------------

  // The frame a video shows now, as PNG; throws for a video from another
  // site that won't be read (then the screen is grabbed instead).
  function frameDataUrl(v) {
    if (!v.videoWidth) throw new Error("no frame yet");
    const c = document.createElement("canvas");
    c.width = v.videoWidth;
    c.height = v.videoHeight;
    c.getContext("2d").drawImage(v, 0, 0);
    return c.toDataURL("image/png");
  }

  async function sendFrame(m) {
    if (m.kind !== "video") return sendOne(m);
    const v = m.el;
    const d = await describe(m, true);
    const t = Math.floor(v.currentTime || 0);
    d.kind = "image";
    d.urls = [];
    d.name = `${d.author || "video"}-frame-${Math.floor(t / 60)}m${String(t % 60).padStart(2, "0")}s`;
    try {
      d.dataUrl = frameDataUrl(v);
    } catch {
      d.frame = true;
    }
    return api.runtime.sendMessage({ type: "osn-send", desc: d });
  }

  // Open the app's find with pictures like this one.
  async function lookFor(m) {
    if (m.kind === "video" && !m.el.poster) {
      try {
        const d = await describe(m, true);
        d.dataUrl = frameDataUrl(m.el);
        return api.runtime.sendMessage({ type: "osn-look", desc: d });
      } catch {
        return { ok: false, error: "Can't read a picture from this video — send a frame, then look from the app" };
      }
    }
    return api.runtime.sendMessage({ type: "osn-look", desc: await describe(m, true) });
  }

  // Look the picture up on Google Lens first; it is sent once the artist
  // is picked there.
  async function findArtist(m) {
    if (m.kind === "video") return { ok: false, error: "Find the artist works on pictures — send a frame of the video, then use Find artist in the app" };
    return api.runtime.sendMessage({ type: "osn-lens-browser", desc: await describe(m) });
  }

  // ---- Already on a slide? --------------------------------------------

  function keyOf(m) {
    if (m.kind === "image") return m.el.currentSrc || m.el.src;
    if (m.kind === "background") return m.url;
    if (m.kind === "video") return m.el.currentSrc || m.el.src || m.el.poster || "";
    return "";
  }

  const checked = new Map(); // what a picture is known by → Promise of the slides with it (null: unknown)
  function checkMedia(m) {
    const key = keyOf(m);
    if (!key || (m.kind === "video" && !m.el.poster)) return Promise.resolve(null);
    if (!checked.has(key)) {
      checked.set(
        key,
        (async () => {
          const r = await api.runtime.sendMessage({ type: "osn-check", desc: await describe(m, true) }).catch(() => null);
          if (!r || !r.ok) {
            checked.delete(key);
            return null;
          }
          return r.found;
        })(),
      );
    }
    return checked.get(key);
  }

  function foundText(found) {
    if (!found) return "";
    if (!found.length) return " · not on your slides";
    const f = found[0];
    return ` · on slide ${f.index + 1}${f.name ? ` · ${f.name}` : ""}${found.length > 1 ? ` (+${found.length - 1})` : ""}`;
  }

  // ---- A whole post ---------------------------------------------------

  function postAround(el) {
    const host = location.hostname.replace(/^(www|mobile|m)\./, "");
    if (host === "x.com" || host === "twitter.com") return el.closest("article");
    if (host === "instagram.com") return el.closest('article, div[role="dialog"], main');
    if (host === "bsky.app") return el.closest('[data-testid^="feedItem-by-"], [data-testid^="postThreadItem-by-"]');
    return el.closest('article, [role="article"]');
  }

  // Every picture, GIF and video in the post around `el`, each described
  // as it is found (Instagram drops a carousel's pictures from the page as
  // it moves on, so it is stepped through).
  async function mediaInPost(el) {
    const box = postAround(el);
    if (!box) return [];
    const found = [];
    const seen = new Set();
    const take = async () => {
      for (const inner of box.querySelectorAll("img, video")) {
        const r = inner.getBoundingClientRect();
        if (r.width < 100 || r.height < 100) continue;
        const m = asMedia(inner);
        const key = m && keyOf(m);
        if (!m || !key || seen.has(key)) continue;
        seen.add(key);
        m.desc = await describe(m);
        found.push(m);
      }
    };
    await take();
    const next = () => box.querySelector('button[aria-label="Next"]');
    for (let i = 0; i < 20 && next(); i++) {
      next().click();
      await new Promise((r) => setTimeout(r, 450));
      await take();
    }
    return found;
  }

  // ---- Sending --------------------------------------------------------

  // Send one picture, GIF or video; `record` records a video as it plays
  // whatever it is. `links`: posts asked for already in this lot (one
  // download brings all of a post's videos). `{ ok, message }` or
  // `{ ok: false, error }`.
  async function sendOne(m, record = false, links = new Set()) {
    const d = await describe(m);
    if (m.kind === "video" && (record || d.stream)) return sendStream(m, d, record, links);
    if (m.blob) return uploadBlob(m.blob, d);
    return api.runtime.sendMessage({ type: "osn-send", desc: d });
  }

  async function sendStream(m, d, record, links) {
    const link = d.post || (S.isFeedUrl(d.page) ? "" : d.page);
    if (!record && link && links.has(link)) return { ok: true, message: "Coming with the post's download" };
    if (!record && link) {
      links.add(link);
      // The app downloads it whole from its post, if it has yt-dlp.
      const r = await api.runtime.sendMessage({ type: "osn-link", desc: d, link });
      if (r && r.ok) return r;
      if (r && !r.fallback) return r;
    }
    note("Recording the video as it plays…", "");
    try {
      const blob = await recordVideo(m.el);
      d.name = `${d.author || "video"}-recording`;
      note("Sending the recording…", "");
      return await uploadBlob(blob, d);
    } catch (e) {
      return { ok: false, error: e.message || String(e) };
    }
  }

  // ---- The picker -----------------------------------------------------

  let picker = null;

  function label(m) {
    const r = m.el.getBoundingClientRect();
    const size = `${Math.round(r.width)}×${Math.round(r.height)}`;
    const src = m.kind === "image" ? m.el.currentSrc || m.el.src : m.kind === "background" ? m.url : "";
    const what = m.kind === "video" ? "Video" : /\.gif(\?|$)/i.test(src) ? "GIF" : "Picture";
    return `${what} ${size}`;
  }

  // Ask the app whether the picture under the picker is on a slide, once
  // the pointer rests on it.
  function pickerCheck(m) {
    if (picker.found.has(m.el)) return;
    clearTimeout(picker.checkTimer);
    picker.checkTimer = setTimeout(() => {
      checkMedia(m).then((found) => {
        if (!picker || found === null) return;
        picker.found.set(m.el, foundText(found));
        if (picker.list[picker.index] && picker.list[picker.index].el === m.el) drawPicker();
      });
    }, 250);
  }

  function place(div, r) {
    div.style.left = `${r.left}px`;
    div.style.top = `${r.top}px`;
    div.style.width = `${r.width}px`;
    div.style.height = `${r.height}px`;
  }

  function drawPicker() {
    if (!picker) return;
    picker.frame = 0;
    const list = mediaAt(picker.x, picker.y, 16);
    const keep = picker.list[picker.index];
    picker.list = list;
    const at = keep ? list.findIndex((m) => m.el === keep.el) : -1;
    picker.index = at >= 0 ? at : 0;
    const m = list[picker.index];
    if (!m) {
      picker.box.hidden = true;
      picker.tag.hidden = true;
      return;
    }
    const r = m.el.getBoundingClientRect();
    place(picker.box, r);
    picker.box.hidden = false;
    const more = list.length > 1 ? ` · ${picker.index + 1} of ${list.length} — wheel for the others` : "";
    picker.tag.textContent = label(m) + (picker.found.get(m.el) || "") + more;
    pickerCheck(m);
    picker.tag.style.left = `${Math.max(4, r.left)}px`;
    picker.tag.style.top = `${r.top > 28 ? r.top - 26 : r.bottom + 4}px`;
    picker.tag.hidden = false;
  }

  function redrawMarks() {
    for (const mark of picker.marks) {
      place(mark.div, mark.m.el.getBoundingClientRect());
    }
  }

  const swallow = (e) => {
    e.preventDefault();
    e.stopPropagation();
    e.stopImmediatePropagation();
  };

  function onMove(e) {
    picker.x = e.clientX;
    picker.y = e.clientY;
    if (!picker.frame) picker.frame = requestAnimationFrame(drawPicker);
  }

  function onWheel(e) {
    if (!picker.list.length || picker.list.length < 2) return;
    swallow(e);
    picker.index = (picker.index + (e.deltaY > 0 ? 1 : -1) + picker.list.length) % picker.list.length;
    // Drawn again from what is under the pointer now, keeping this one.
    drawPicker();
  }

  function onScroll() {
    if (!picker) return;
    if (!picker.frame) picker.frame = requestAnimationFrame(drawPicker);
    redrawMarks();
  }

  function onClick(e) {
    swallow(e);
    const m = picker.list[picker.index];
    if (!m) return;
    if (e.shiftKey) {
      const at = picker.marks.findIndex((k) => k.m.el === m.el);
      if (at >= 0) {
        picker.marks[at].div.remove();
        picker.marks.splice(at, 1);
      } else {
        const div = document.createElement("div");
        div.className = "picked";
        picker.root.appendChild(div);
        place(div, m.el.getBoundingClientRect());
        picker.marks.push({ m, div });
      }
      picker.help.textContent = picker.marks.length
        ? `${picker.marks.length} picked — Enter sends them · Shift+click picks more · Esc stops`
        : HELP;
      return;
    }
    const all = picker.marks.map((k) => k.m);
    if (!all.some((k) => k.el === m.el)) all.push(m);
    stopPicker();
    sendAll(all, e.altKey ? "record" : e.ctrlKey ? "frame" : "send");
  }

  function onKey(e) {
    if (e.key === "Escape") {
      swallow(e);
      stopPicker();
    } else if (e.key === "Enter" && picker.marks.length) {
      swallow(e);
      const all = picker.marks.map((k) => k.m);
      stopPicker();
      sendAll(all);
    }
  }

  const HELP = "Click a picture, GIF or video to send it · wheel: the one beneath · Shift+click: pick several, Enter sends · Ctrl+click: a video's frame · Alt+click: record a video · Esc: stop";
  const BLOCK = ["mousedown", "mouseup", "pointerdown", "pointerup", "dblclick", "contextmenu", "auxclick"];

  function startPicker() {
    if (picker) return stopPicker();
    const host = document.createElement("osn-picker");
    host.style.cssText = "all: initial; position: fixed; inset: 0; z-index: 2147483647; pointer-events: none;";
    const root = host.attachShadow({ mode: "closed" });
    root.innerHTML =
      "<style>" +
      ".box{position:fixed;border:2px solid #fff;outline:1px solid #000;border-radius:3px;box-shadow:0 0 0 100vmax rgba(0,0,0,.28);pointer-events:none}" +
      ".picked{position:fixed;border:2px dashed #fff;outline:1px solid #000;border-radius:3px;pointer-events:none}" +
      ".tag,.help{position:fixed;font:12px/1.4 system-ui,sans-serif;color:#f4f4f5;background:#18181b;border:1px solid #3f3f46;border-radius:6px;padding:3px 8px;white-space:nowrap;pointer-events:none}" +
      ".help{left:50%;top:12px;transform:translateX(-50%);padding:6px 10px}" +
      '</style><div class="box" hidden></div><div class="tag" hidden></div><div class="help"></div>';
    (document.body || document.documentElement).appendChild(host);
    picker = { host, root, box: root.querySelector(".box"), tag: root.querySelector(".tag"), help: root.querySelector(".help"), list: [], index: 0, x: -1, y: -1, frame: 0, marks: [], found: new Map(), checkTimer: 0 };
    picker.help.textContent = HELP;
    window.addEventListener("mousemove", onMove, true);
    window.addEventListener("wheel", onWheel, { capture: true, passive: false });
    window.addEventListener("click", onClick, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("scroll", onScroll, true);
    for (const t of BLOCK) window.addEventListener(t, swallow, true);
  }

  function stopPicker() {
    if (!picker) return;
    window.removeEventListener("mousemove", onMove, true);
    window.removeEventListener("wheel", onWheel, { capture: true, passive: false });
    window.removeEventListener("click", onClick, true);
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("scroll", onScroll, true);
    for (const t of BLOCK) window.removeEventListener(t, swallow, true);
    if (picker.frame) cancelAnimationFrame(picker.frame);
    clearTimeout(picker.checkTimer);
    picker.host.remove();
    picker = null;
  }

  // Send each of `list`: `mode` is "send", "record" (a video as it
  // plays), "frame" (a video's frame now), "look" (look for it in the
  // app) or "artist" (find the artist on Lens, then send).
  async function sendAll(list, mode = "send") {
    // The picker's frame must be gone from the screen before a screen
    // grab could be taken.
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    let sent = 0;
    let last = null;
    const links = new Set();
    const one = (m) => {
      if (mode === "frame") return sendFrame(m);
      if (mode === "look") return lookFor(m);
      if (mode === "artist") return findArtist(m);
      return sendOne(m, mode === "record" && m.kind === "video", links);
    };
    for (const m of list) {
      if (list.length > 1) note(`Sending ${sent + 1} of ${list.length}…`, "");
      const r = await one(m).catch((e) => ({ ok: false, error: String(e?.message || e) }));
      last = r;
      if (r && r.ok) sent++;
      else note((r && r.error) || "Couldn't send it", "error");
    }
    if (list.length === 1 && last && last.ok) note(last.message, "ok");
    else if (list.length > 1) note(`Sent ${sent} of ${list.length} to On-Screen Notes`, sent === list.length ? "ok" : "error");
  }

  // Act on what is at `at`: send it (or record it…) from here, where the
  // video plays. `mode` as `sendAll` takes it, or "post" for every
  // picture and video in the post around it. `srcUrl`: what the browser
  // saw under a right-click, preferred when it is there. `{ started }`, or
  // `{ none, frame }` (`frame`: the point is over an embedded player).
  function actAt(at, mode, srcUrl = "") {
    let list = at ? mediaAt(at.x, at.y) : [];
    if (at && !list.length) list = mediaAt(at.x, at.y, 1);
    let m = list[0];
    if (srcUrl) {
      const same = list.find((k) => {
        const urls = k.kind === "image" ? [k.el.currentSrc, k.el.src] : k.kind === "video" ? videoUrls(k.el) : [k.url];
        return urls.includes(srcUrl);
      });
      if (same) m = same;
    }
    if (mode === "record" || mode === "frame") m = list.find((k) => k.kind === "video");
    if (!m) {
      const under = at && document.elementFromPoint(at.x, at.y);
      return { none: true, frame: !!(under && under.tagName === "IFRAME") };
    }
    if (mode === "post") {
      note("Gathering the post's pictures…", "");
      mediaInPost(m.el).then((all) => {
        if (!all.length) note("No post around it — try All media on this page", "error");
        else sendAll(all, "send");
      });
    } else {
      sendAll([m], mode);
    }
    return { started: true };
  }

  // What "Tag as the artist" tags with: the highlighted text, and the link
  // under `at` (its address and its text).
  function linkAt(at) {
    const selection = String(window.getSelection() || "").trim().slice(0, 200);
    let href = "";
    let text = "";
    if (at) {
      const hit = stackAt(at.x, at.y).find((el) => el.closest && el.closest("a[href]"));
      const a = hit && hit.closest("a[href]");
      if (a) {
        href = a.href;
        text = a.textContent.trim().slice(0, 300);
      }
    }
    return { selection, href, text, page: location.href };
  }

  // ---- Keys ------------------------------------------------------------
  // The keys set in the options (`keys` in storage, over the defaults),
  // pressed on a page. Ones on what is under the pointer act here; the
  // rest go through background.js (the picker and the grid live in the
  // top frame).

  let keys = null;
  async function loadKeys() {
    const s = await api.storage.local.get({ keys: null }).catch(() => ({ keys: null }));
    keys = Object.assign(S.defaultKeys(), s.keys || {});
  }
  loadKeys();
  api.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes.keys) loadKeys();
  });

  function typingIn(e) {
    const el = (e.composedPath && e.composedPath()[0]) || e.target;
    if (!(el instanceof Element)) return false;
    return el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName);
  }

  window.addEventListener(
    "keydown",
    (e) => {
      if (!keys || e.repeat || picker || document.querySelector("osn-grid")) return;
      const combo = S.keyCombo(e);
      if (!combo) return;
      const action = S.KEY_ACTIONS.find((a) => keys[a.id] === combo);
      if (!action) return;
      // A plain key while typing is typing.
      if (!e.ctrlKey && !e.altKey && !e.metaKey && typingIn(e)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (action.where === "pointer") {
        const r = actAt(pointer, action.mode);
        if (r.none) note(r.frame ? "That's inside an embedded player — right-click it instead" : "Nothing under the pointer — point at a picture, GIF or video", "error");
      } else if (action.where === "link") {
        api.runtime.sendMessage({ type: "osn-tag-artist", ...linkAt(pointer) }).catch(() => {});
      } else {
        api.runtime.sendMessage({ type: "osn-key-action", action: action.id }).catch(() => {});
      }
    },
    true,
  );

  // ---- Messages from background.js ------------------------------------

  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || typeof msg.type !== "string") return;
    if (msg.type === "osn-send-at-click" || msg.type === "osn-send-hovered") {
      // The right-click menu (or a key over a picture): find what was
      // under the click (or the pointer), and act on it from here.
      const at = msg.type === "osn-send-hovered" ? pointer : lastClick;
      sendResponse(actAt(at, msg.mode || (msg.record ? "record" : "send"), msg.srcUrl || ""));
      return;
    }
    if (msg.type === "osn-hovered-link") {
      // A browser shortcut for "Tag as the artist": what to tag with.
      sendResponse(linkAt(pointer));
      return;
    }
    if (msg.type === "osn-ask-tags" && top) {
      // A plain question box: the browser's own, so it works anywhere.
      const typed = window.prompt("Tags for everything you send from now on (spaces between; leave empty for none):", msg.current || "");
      sendResponse({ tags: typed });
      return;
    }
    if (msg.type === "osn-grid" && top) {
      if (globalThis.__osnGrid) globalThis.__osnGrid.toggle();
      sendResponse({ ok: true });
      return;
    }
    if (msg.type === "osn-resolve") {
      const at = lastClick;
      if (!at) {
        sendResponse({ none: true });
        return;
      }
      let list = mediaAt(at.x, at.y);
      if (!list.length) list = mediaAt(at.x, at.y, 1);
      // What the browser itself saw under the click, when it saw one.
      let m = list[0];
      if (msg.srcUrl) {
        const same = list.find((k) => {
          const urls = k.kind === "image" ? [k.el.currentSrc, k.el.src] : k.kind === "video" ? videoUrls(k.el) : [k.url];
          return urls.includes(msg.srcUrl);
        });
        if (same) m = same;
      }
      if (!m) {
        sendResponse({ none: true });
        return;
      }
      describe(m).then(sendResponse, () => sendResponse({ none: true }));
      return true;
    }
    if (msg.type === "osn-picker" && top) {
      startPicker();
      sendResponse({ ok: true });
      return;
    }
    if (msg.type === "osn-toast" && top) {
      toast(msg.text, msg.kind);
      sendResponse({ ok: true });
      return;
    }
    if (msg.type === "osn-ping") {
      sendResponse({ ok: true });
      return;
    }
  });

  // For grid.js ("All media on this page"), in the same page.
  globalThis.__osnContent = { asMedia, keyOf, label, describe, sendAll, checkMedia, foundText, note };
})();
