// The extension's background: the right-click menu, the picker's key and
// button, fetching what is to be sent, and sending it to On-Screen Notes
// on this computer (http://127.0.0.1:<port>). The app must have let the
// extension in once: `connect` asks it, and keeps the token it gives.
// Firefox runs this as an event page, Chrome as a service worker.
"use strict";

const api = globalThis.browser ?? globalThis.chrome;
const DEFAULT_PORT = 47823;
// What On-Screen Notes reads as is; other picture types become PNG first.
const APP_TYPES = new Set(["image/png", "image/jpeg", "image/webp", "image/bmp", "image/gif", "video/mp4", "video/webm", "video/quicktime", "video/x-matroska"]);
const NOT_RUNNING = "On-Screen Notes isn't running — or \"Receive from the browser\" is off in its settings";
const STREAM = "This video plays as a stream — right-click it → Record video as it plays";
// Where "Find artist" uploads a picture: Google's start page with the
// Lens dialog open.
const LENS_URL = "https://www.google.com/?olud";

// ---- Menus, key and button --------------------------------------------

// What the right-click menu does to what is under the click: the mode
// content.js's `sendAll` takes ("post": every picture of the post).
const ACTIONS = [
  ["osn-send", "Send to On-Screen Notes", "send"],
  ["osn-post", "Send all pictures in this post", "post"],
  ["osn-frame", "Send this video frame", "frame"],
  ["osn-record", "Record video as it plays", "record"],
  ["osn-artist", "Find the artist, then send…", "artist"],
  ["osn-look", "Look in my slides", "look"],
];
// Slides offered to send to, besides "the open one".
const SLIDE_ITEMS = 10;

function setupMenus() {
  Promise.resolve(api.contextMenus.removeAll())
    .catch(() => {})
    .then(() => {
      for (const [id, title] of ACTIONS) api.contextMenus.create({ id, title, contexts: ["all"] });
      api.contextMenus.create({ id: "osn-pick", title: "Pick media to send…", contexts: ["all"] });
      api.contextMenus.create({ id: "osn-grid", title: "All media on this page…", contexts: ["all"] });
      api.contextMenus.create({ id: "osn-sep", type: "separator", contexts: ["all"] });
      api.contextMenus.create({ id: "osn-slides", title: "Send to: the open slide", contexts: ["all"] });
      api.contextMenus.create({ id: "osn-slide-open", parentId: "osn-slides", type: "radio", title: "The open slide", checked: true, contexts: ["all"] });
      for (let k = 0; k < SLIDE_ITEMS; k++) {
        api.contextMenus.create({ id: `osn-slide-${k}`, parentId: "osn-slides", type: "radio", title: "…", visible: false, contexts: ["all"] });
      }
      api.contextMenus.create({ id: "osn-tags", title: "Tags for what you send…", contexts: ["all"] });
      // Only while "Find artist" is looking a picture up.
      api.contextMenus.create({ id: "osn-lens-tag", title: "Tag as the artist → On-Screen Notes", contexts: ["link", "selection"], visible: false });
      lensLoad().then((l) => setLensMenu(!!l));
      refreshSlides();
      refreshTagsTitle();
    });
}
api.runtime.onInstalled.addListener(setupMenus);
api.runtime.onStartup.addListener(setupMenus);

api.contextMenus.onClicked.addListener((info, tab) => {
  if (!tab) return;
  const id = String(info.menuItemId);
  const action = ACTIONS.find((a) => a[0] === id);
  if (action) sendFromMenu(info, tab, action[2]);
  else if (id === "osn-pick") togglePicker(tab);
  else if (id === "osn-grid") toggleGrid(tab);
  else if (id === "osn-tags") askTags(tab);
  else if (id === "osn-lens-tag") tagFromMenu(info, tab);
  else if (id.startsWith("osn-slide-")) pickSlide(id);
});

// The browser's own shortcuts (set in its settings, none by default):
// every key action is one, named by its id. They work even while the
// address bar has the keyboard; the keys set in the options work on pages.
api.commands.onCommand.addListener(async (command, tab) => {
  if (!tab) [tab] = await api.tabs.query({ active: true, currentWindow: true });
  if (tab) keyAction(command, tab);
});

// Do key action `id` (`OSNSites.KEY_ACTIONS`) in `tab`.
function keyAction(id, tab) {
  const action = globalThis.OSNSites.KEY_ACTIONS.find((a) => a.id === id);
  if (!action) return;
  if (action.where === "pointer") sendHovered(tab, action.mode);
  else if (action.where === "link") tagHovered(tab);
  else if (id === "pick") togglePicker(tab);
  else if (id === "grid") toggleGrid(tab);
  else if (id === "tags") askTags(tab);
  else if (id === "same-artist") {
    lensSame().then((r) => {
      if (r && !r.ok) toast(tab, r.error, "error");
    });
  }
}

// The slides to offer change in the app: asked again when the browser
// comes back to the front, or another tab is.
api.tabs.onActivated.addListener(() => refreshSlides());
api.windows.onFocusChanged.addListener((w) => {
  if (w !== api.windows.WINDOW_ID_NONE) refreshSlides();
});
if (api.menus && api.menus.onShown) {
  // Firefox: just before the menu shows.
  api.menus.onShown.addListener(async () => {
    await refreshSlides();
    api.menus.refresh();
  });
}

api.action.onClicked.addListener((tab) => togglePicker(tab));

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg || typeof msg.type !== "string") return;
  if (msg.type === "osn-send") {
    send(msg.desc, sender.tab).then(sendResponse);
    return true;
  }
  if (msg.type === "osn-connect") {
    connect().then(
      () => sendResponse({ ok: true }),
      (e) => sendResponse({ ok: false, error: e.message }),
    );
    return true;
  }
  if (msg.type === "osn-status") {
    appStatus().then(sendResponse);
    return true;
  }
  if (msg.type === "osn-link") {
    sendLink(msg.desc, msg.link, sender.tab).then(sendResponse);
    return true;
  }
  if (msg.type === "osn-relay-toast") {
    toast(sender.tab, msg.text, msg.kind);
    return;
  }
  if (msg.type.startsWith("osn-lens-")) {
    lensMessage(msg, sender).then(sendResponse, (e) => sendResponse({ ok: false, error: e?.message || String(e) }));
    return true;
  }
  if (msg.type === "osn-key-action") {
    if (sender.tab) keyAction(msg.action, sender.tab);
    return;
  }
  if (msg.type === "osn-tag-artist") {
    // The "Tag as the artist" key, pressed on a page (maybe in a frame).
    if (sender.tab) tagWith(msg, sender.tab, true);
    return;
  }
  if (msg.type === "osn-open-shortcuts") {
    openShortcuts().then(sendResponse);
    return true;
  }
  if (msg.type === "osn-check") {
    checkPicture(msg.desc, sender.tab).then(sendResponse);
    return true;
  }
  if (msg.type === "osn-look") {
    lookFor(msg.desc, sender.tab).then(sendResponse);
    return true;
  }
});

// A file from a content script, in pieces (a recording, or a video file
// kept in the page): put together, then sent like a fetched one.
api.runtime.onConnect.addListener((port) => {
  if (port.name !== "osn-upload") return;
  let desc = null;
  let type = "";
  const parts = [];
  port.onMessage.addListener(async (msg) => {
    if (msg.start) {
      desc = msg.start;
      type = msg.type || "";
    } else if (msg.chunk) {
      parts.push(base64ToBytes(msg.chunk));
    } else if (msg.end) {
      const blob = new Blob(parts, { type });
      parts.length = 0;
      const r = await send(desc, port.sender && port.sender.tab, blob);
      try {
        port.postMessage({ done: r });
      } catch {
        /* the page went away */
      }
    }
  });
});

// The content scripts are not in tabs that were open before the
// extension came: put them in when needed.
async function ensureContent(tab, frameId = 0) {
  try {
    await api.tabs.sendMessage(tab.id, { type: "osn-ping" }, { frameId });
    return true;
  } catch {
    try {
      await api.scripting.executeScript({ target: { tabId: tab.id, frameIds: [frameId] }, files: ["sites.js", "content.js", "lens.js", "grid.js"] });
      return true;
    } catch {
      return false;
    }
  }
}

async function togglePicker(tab) {
  if (await ensureContent(tab)) {
    await api.tabs.sendMessage(tab.id, { type: "osn-picker" }, { frameId: 0 }).catch(() => {});
  }
}

async function toggleGrid(tab) {
  if (await ensureContent(tab)) {
    await api.tabs.sendMessage(tab.id, { type: "osn-grid" }, { frameId: 0 }).catch(() => {});
  }
}

// A key: act on what is under the pointer (`mode` as content.js's
// `sendAll` takes it).
async function sendHovered(tab, mode = "send") {
  if (!(await ensureContent(tab))) return;
  const answer = await api.tabs.sendMessage(tab.id, { type: "osn-send-hovered", mode }, { frameId: 0 }).catch(() => null);
  if (answer && answer.none) {
    toast(tab, answer.frame ? "That's inside an embedded player — right-click it instead" : "Nothing to send under the pointer — point at a picture, GIF or video", "error");
  }
}

// The browser's own shortcut settings, where it can open them: `{ ok }`,
// or `{ ok: false, where }` saying where to find them.
async function openShortcuts() {
  try {
    if (api.commands.openShortcutSettings) {
      await api.commands.openShortcutSettings(); // Firefox 137+
      return { ok: true };
    }
    if (!/Firefox\//.test(navigator.userAgent)) {
      await api.tabs.create({ url: "chrome://extensions/shortcuts" });
      return { ok: true };
    }
  } catch {
    /* told below where to find them */
  }
  return { ok: false, where: "about:addons → the ⚙ menu → Manage Extension Shortcuts" };
}

// ---- Tags and slide for what is sent ----------------------------------

async function sendTags() {
  return (await api.storage.local.get({ sendTags: "" })).sendTags || "";
}

function tagList(text) {
  return text.split(/[\s,]+/).filter(Boolean).map((t) => `#${t.replace(/^#/, "")}`).join(" ");
}

async function refreshTagsTitle() {
  const t = await sendTags();
  const title = t ? `Tags for what you send: ${tagList(t)}` : "Tags for what you send…";
  Promise.resolve(api.contextMenus.update("osn-tags", { title: title.slice(0, 90) })).catch(() => {});
}

async function askTags(tab) {
  if (!(await ensureContent(tab))) return;
  const r = await api.tabs.sendMessage(tab.id, { type: "osn-ask-tags", current: await sendTags() }, { frameId: 0 }).catch(() => null);
  if (!r || typeof r.tags !== "string") return; // cancelled
  const tags = r.tags.trim();
  await api.storage.local.set({ sendTags: tags });
  refreshTagsTitle();
  toast(tab, tags ? `Everything you send now gets ${tagList(tags)}` : "Nothing you send gets tags now", "ok");
}

let slideOffer = []; // [{ index, name }] as the menu shows them

function slideTitle(s) {
  return `Slide ${s.index + 1}${s.name ? ` · ${s.name}` : ""}`;
}

async function refreshSlides() {
  let list = null;
  try {
    const { port, token } = await settings();
    if (token) {
      const r = await fetch(`http://127.0.0.1:${port}/v1/slides`, { headers: { "X-OSN": "1", "X-OSN-Token": token } });
      if (r.ok) list = await r.json();
    }
  } catch {
    /* the app isn't running: the menu keeps what it had */
  }
  if (list) slideOffer = list.slides || [];
  const { target } = await api.storage.local.get({ target: null });
  const update = (id, props) => Promise.resolve(api.contextMenus.update(id, props)).catch(() => {});
  for (let k = 0; k < SLIDE_ITEMS; k++) {
    const s = slideOffer[k];
    if (!s) {
      update(`osn-slide-${k}`, { visible: false });
      continue;
    }
    const open = list && s.index === list.active ? " (open now)" : "";
    update(`osn-slide-${k}`, { visible: true, title: slideTitle(s) + open, checked: !!target && target.index === s.index && target.name === s.name });
  }
  update("osn-slide-open", { checked: !target });
  update("osn-slides", { title: target ? `Send to: ${slideTitle(target)}` : "Send to: the open slide" });
}

async function pickSlide(id) {
  const k = id === "osn-slide-open" ? -1 : Number(id.slice("osn-slide-".length));
  const s = slideOffer[k];
  await api.storage.local.set({ target: k < 0 || !s ? null : { index: s.index, name: s.name } });
  refreshSlides();
}

// The headers that carry the tags and the slide asked for.
async function extras(desc) {
  const { target } = await api.storage.local.get({ target: null });
  const tags = [await sendTags(), ...(desc.tags || [])].join(" ").trim();
  const h = {};
  if (tags) h["X-OSN-Tags"] = enc(tags);
  if (target) {
    h["X-OSN-Slide"] = String(target.index);
    h["X-OSN-Slide-Name"] = enc(target.name);
  }
  return { headers: h, tags, target };
}

// What the toast says beyond "Sent": where to, and the tags.
function sentNote(x) {
  let s = "";
  if (x.target) s += ` to ${slideTitle(x.target)}`;
  if (x.tags) s += `, tagged ${tagList(x.tags)}`;
  return s;
}

// ---- Looking at a picture: on a slide already? like it? ---------------

// What the page shows of a picture, small: at most `side` on its long
// side, as JPEG. Quick to fetch (the browser has it), enough to compare.
async function smallPicture(desc, side) {
  let blob = null;
  if (desc.dataUrl) blob = dataUrlToBlob(desc.dataUrl);
  else {
    const urls = [desc.shown, ...(desc.urls || []).slice().reverse()].filter((u) => u && /^https?:/i.test(u));
    for (const u of urls) {
      try {
        const r = await fetch(u, { credentials: "include" });
        if (r.ok) {
          blob = await r.blob();
          break;
        }
      } catch {
        /* the next one */
      }
    }
  }
  if (!blob) throw new Error("Couldn't fetch the picture");
  const bmp = await createImageBitmap(blob);
  const k = Math.min(1, side / Math.max(bmp.width, bmp.height, 1));
  const c = new OffscreenCanvas(Math.max(1, Math.round(bmp.width * k)), Math.max(1, Math.round(bmp.height * k)));
  c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
  return c.convertToBlob({ type: "image/jpeg", quality: 0.9 });
}

// Which slides hold this picture: `{ ok, found: [{ index, name }] }`.
// Never asks to connect: it runs as the pointer moves.
async function checkPicture(desc) {
  try {
    const pic = await smallPicture(desc, 512);
    const j = await appRequest("POST", "/v1/check", { headers: { "Content-Type": "image/jpeg" }, body: pic, quiet: true });
    return { ok: true, found: j.found || [] };
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  }
}

async function lookFor(desc) {
  try {
    const pic = await smallPicture(desc, 1600);
    await appRequest("POST", "/v1/find", { headers: { "Content-Type": "image/jpeg" }, body: pic });
    return { ok: true, message: "On-Screen Notes is looking for pictures like it on your slides" };
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  }
}

function toast(tab, text, kind) {
  if (tab) api.tabs.sendMessage(tab.id, { type: "osn-toast", text, kind }, { frameId: 0 }).catch(() => {});
}

async function sendFromMenu(info, tab, mode) {
  const frameId = info.frameId ?? 0;
  // The content script finds what is under the click and sends it from
  // the page, where a streamed video can be recorded.
  let answer = null;
  try {
    answer = await api.tabs.sendMessage(tab.id, { type: "osn-send-at-click", srcUrl: info.srcUrl || "", mode }, { frameId });
  } catch {
    // No content script in that frame (a tab open from before): what the
    // browser saw under the click is all there is.
  }
  if (answer && answer.started) return;
  if (!info.srcUrl || mode !== "send") {
    await ensureContent(tab);
    const what = mode === "record" || mode === "frame" ? "No video there" : "No picture, GIF or video there";
    toast(tab, `${what} — try the picker (Alt+Shift+S)${answer ? "" : ", or reload the page"}`, "error");
    return;
  }
  {
    const desc = {
      kind: info.mediaType === "video" ? "video" : "image",
      urls: [info.srcUrl],
      page: info.pageUrl || tab.url || "",
      title: tab.title || "",
      author: "",
      site: "",
      post: "",
      name: "",
    };
    await ensureContent(tab);
    const r = await send(desc, tab);
    toast(tab, r.ok ? r.message : r.error, r.ok ? "ok" : "error");
  }
}

// ---- Getting the file -------------------------------------------------

function base64ToBytes(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

function dataUrlToBlob(url) {
  const [head, data] = url.split(",", 2);
  const type = (head.match(/^data:([^;,]+)/) || [])[1] || "application/octet-stream";
  if (/;base64/i.test(head)) {
    const bin = atob(data);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return new Blob([bytes], { type });
  }
  return new Blob([decodeURIComponent(data)], { type });
}

// What the bytes are, by their first few, else by what the server said.
async function sniff(blob, contentType) {
  const h = new Uint8Array(await blob.slice(0, 16).arrayBuffer());
  const is = (sig, at = 0) => sig.every((b, i) => h[at + i] === b);
  if (is([0x89, 0x50, 0x4e, 0x47])) return "image/png";
  if (is([0xff, 0xd8, 0xff])) return "image/jpeg";
  if (is([0x47, 0x49, 0x46, 0x38])) return "image/gif";
  if (is([0x52, 0x49, 0x46, 0x46]) && is([0x57, 0x45, 0x42, 0x50], 8)) return "image/webp";
  if (is([0x42, 0x4d])) return "image/bmp";
  if (is([0x66, 0x74, 0x79, 0x70], 4)) {
    const brand = String.fromCharCode(...h.slice(8, 12));
    return /^(avif|avis|heic|heix|mif1|msf1)/.test(brand) ? "image/avif" : "video/mp4";
  }
  if (is([0x1a, 0x45, 0xdf, 0xa3])) return "video/webm";
  const ct = (contentType || "").split(";")[0].trim().toLowerCase();
  return ct.startsWith("image/") || ct.startsWith("video/") ? ct : null;
}

async function toPng(blob) {
  const bmp = await createImageBitmap(blob);
  const c = new OffscreenCanvas(bmp.width, bmp.height);
  c.getContext("2d").drawImage(bmp, 0, 0);
  return c.convertToBlob({ type: "image/png" });
}

const EXT = { "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp", "image/bmp": "bmp", "image/gif": "gif", "video/mp4": "mp4", "video/webm": "webm", "video/quicktime": "mov", "video/x-matroska": "mkv" };

// `name` with the extension its type has.
function named(name, type) {
  const ext = EXT[type] || "bin";
  const stem = (name || "").replace(/\.[A-Za-z0-9]{1,5}$/, "") || "from-browser";
  return `${stem}.${ext}`;
}

// The bytes in a type On-Screen Notes reads, or an error saying why not.
async function readable(blob, type) {
  if (APP_TYPES.has(type)) return type === blob.type ? blob : new Blob([blob], { type });
  if (type.startsWith("image/")) {
    try {
      return await toPng(blob);
    } catch {
      throw new Error(`Can't read this kind of picture (${type})`);
    }
  }
  throw new Error(`On-Screen Notes can't show ${type}`);
}

// What the screen shows of a picture: the last resort, for one whose
// file the site will not hand over.
async function screenGrab(tab, desc) {
  const shot = await api.tabs.captureVisibleTab(tab.windowId, { format: "png" });
  const bmp = await createImageBitmap(dataUrlToBlob(shot));
  const k = bmp.width / (desc.vw || bmp.width);
  const x = Math.max(0, Math.round(desc.rect.x * k));
  const y = Math.max(0, Math.round(desc.rect.y * k));
  const w = Math.min(bmp.width - x, Math.round(desc.rect.w * k));
  const h = Math.min(bmp.height - y, Math.round(desc.rect.h * k));
  if (w < 8 || h < 8) throw new Error("It is off screen");
  const c = new OffscreenCanvas(w, h);
  c.getContext("2d").drawImage(bmp, x, y, w, h, 0, 0, w, h);
  return c.convertToBlob({ type: "image/png" });
}

// `{ blob, name, grabbed }` for what `desc` describes.
async function getFile(desc, tab) {
  // A video's frame the page wouldn't let be read: what the screen shows.
  if (desc.frame && !desc.dataUrl) {
    if (!desc.topFrame) throw new Error("Can't take a frame from inside an embedded player");
    return { blob: await screenGrab(tab, desc), name: named(desc.name, "image/png"), grabbed: true };
  }
  if (desc.dataUrl) {
    const blob = dataUrlToBlob(desc.dataUrl);
    const type = await sniff(blob, blob.type);
    if (!type) throw new Error("Not a picture, GIF or video");
    return { blob: await readable(blob, type), name: named(desc.name, APP_TYPES.has(type) ? type : "image/png") };
  }
  const urls = (desc.urls || []).filter((u) => /^https?:/i.test(u));
  if (!urls.length) {
    if (desc.stream || (desc.urls || []).some((u) => u.startsWith("blob:"))) throw new Error(STREAM);
    throw new Error("It has no address to fetch it from");
  }
  let lastError = null;
  for (const url of urls) {
    try {
      const r = await fetch(url, { credentials: "include" });
      if (!r.ok) {
        lastError = new Error(`${new URL(url).hostname} answered ${r.status}`);
        continue;
      }
      const blob = await r.blob();
      const type = await sniff(blob, r.headers.get("content-type"));
      if (!type) {
        lastError = new Error("That address gave something that is not a picture, GIF or video");
        continue;
      }
      const name = desc.name || globalThis.OSNSites?.nameFromUrl(url) || "";
      return { blob: await readable(blob, type), name: named(name, APP_TYPES.has(type) ? type : "image/png") };
    } catch (e) {
      lastError = e;
    }
  }
  if (desc.kind !== "video" && desc.topFrame && desc.rect) {
    try {
      return { blob: await screenGrab(tab, desc), name: named(desc.name || "screen-grab", "image/png"), grabbed: true };
    } catch {
      /* the fetch's error says more */
    }
  }
  throw lastError || new Error("Couldn't fetch it");
}

// A file the page handed over: typed by its bytes, named.
async function fromPage(blob, desc) {
  const type = await sniff(blob, blob.type);
  if (!type) throw new Error("Not a picture, GIF or video");
  return { blob: await readable(blob, type), name: named(desc.name, APP_TYPES.has(type) ? type : "image/png") };
}

// ---- Talking to the app -----------------------------------------------

async function settings() {
  const s = await api.storage.local.get({ port: DEFAULT_PORT, token: "" });
  return { port: Number(s.port) || DEFAULT_PORT, token: s.token || "" };
}

function browserName() {
  const ua = navigator.userAgent;
  if (/Firefox\//.test(ua)) return "Firefox";
  if (/Edg\//.test(ua)) return "Edge";
  if (/OPR\//.test(ua)) return "Opera";
  if (/Vivaldi\//.test(ua)) return "Vivaldi";
  return "Chrome";
}

const enc = (s) => encodeURIComponent(s || "").slice(0, 4000);

// This install's own id for itself, made once: the app keeps one pass
// per install, so connecting again replaces the old one.
async function installId() {
  let { installId: id } = await api.storage.local.get({ installId: "" });
  if (!id) {
    const bytes = crypto.getRandomValues(new Uint8Array(8));
    id = [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
    await api.storage.local.set({ installId: id });
  }
  return id;
}

// Ask On-Screen Notes to let this extension in; it waits for the user.
async function connect() {
  const { port } = await settings();
  const install = await installId();
  let r;
  try {
    r = await fetch(`http://127.0.0.1:${port}/v1/pair`, { method: "POST", headers: { "X-OSN": "1", "X-OSN-Client": enc(browserName()), "X-OSN-Install": install } });
  } catch {
    throw new Error(NOT_RUNNING);
  }
  const j = await r.json().catch(() => ({}));
  if (r.ok && j.token) {
    await api.storage.local.set({ token: j.token });
    return j.token;
  }
  if (j.error === "denied") throw new Error("Not connected: On-Screen Notes said no");
  if (r.status === 408) throw new Error("On-Screen Notes got no answer — try again, and press Connect there");
  throw new Error(`Couldn't connect: ${j.error || r.status}`);
}

async function appStatus() {
  const { port, token } = await settings();
  try {
    const r = await fetch(`http://127.0.0.1:${port}/v1/status`, { headers: { "X-OSN": "1", "X-OSN-Token": token } });
    const j = await r.json();
    return { running: true, paired: !!j.paired, ytdlp: !!j.ytdlp, version: j.version || "" };
  } catch {
    return { running: false, paired: false };
  }
}

async function post(file, desc, token, port, more = {}) {
  const headers = {
    ...more,
    "X-OSN": "1",
    "X-OSN-Token": token,
    "Content-Type": file.blob.type || "application/octet-stream",
    "X-OSN-Name": enc(file.name),
    "X-OSN-Page": enc(desc.page),
    "X-OSN-Post": enc(desc.post),
    "X-OSN-Author": enc(desc.author),
    "X-OSN-Site": enc(desc.site),
    "X-OSN-Media": enc((desc.urls || [])[0] || ""),
  };
  try {
    return await fetch(`http://127.0.0.1:${port}/v1/media`, { method: "POST", headers, body: file.blob });
  } catch {
    throw new Error(NOT_RUNNING);
  }
}

// The site's cookies for `url`, for yt-dlp: a post seen while logged in
// downloads only with them. Firefox keeps them per container.
async function cookiesFor(url, tab) {
  const query = { url };
  if (tab && tab.cookieStoreId) query.storeId = tab.cookieStoreId;
  try {
    return await api.cookies.getAll(query);
  } catch {
    try {
      // Firefox with first-party isolation asks for this too.
      return await api.cookies.getAll({ ...query, firstPartyDomain: null });
    } catch {
      return [];
    }
  }
}

// Ask the app to download a streamed video from its post (`link`) with
// yt-dlp. `{ ok, message }`; `{ ok: false, fallback: true }` when the app
// can't (no yt-dlp), so the page records it instead.
async function sendLink(desc, link, tab) {
  try {
    const app = await appStatus();
    if (!app.running) throw new Error(NOT_RUNNING);
    let { port, token } = await settings();
    if (!token || !app.paired) {
      toast(tab, "Connecting — press Connect in On-Screen Notes", "");
      token = await connect();
    }
    const jar = await cookiesFor(link, tab);
    const body = jar.length ? globalThis.OSNSites.netscapeCookies(jar) : "";
    const x = await extras(desc);
    const headers = {
      ...x.headers,
      "X-OSN": "1",
      "X-OSN-Token": token,
      "Content-Type": "text/plain",
      "X-OSN-Link": enc(link),
      "X-OSN-Page": enc(desc.page),
      "X-OSN-Post": enc(desc.post || link),
      "X-OSN-Author": enc(desc.author),
      "X-OSN-Site": enc(desc.site),
    };
    let r;
    try {
      r = await fetch(`http://127.0.0.1:${port}/v1/link`, { method: "POST", headers, body });
    } catch {
      throw new Error(NOT_RUNNING);
    }
    const j = await r.json().catch(() => ({}));
    if (r.ok) return { ok: true, message: `Downloading the video in On-Screen Notes — it lands${x.target ? ` on ${slideTitle(x.target)}` : " on the slide"} when done` };
    if (j.error === "ytdlp") return { ok: false, fallback: true, error: "yt-dlp isn't installed" };
    throw new Error(`On-Screen Notes turned it down: ${j.error || r.status}`);
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  }
}

// Fetch what `desc` describes (or take `blob`, already fetched) and send
// it to the app: `{ ok, message }` or `{ ok: false, error }`.
async function send(desc, tab, blob = null) {
  try {
    // Asked first, so nothing is fetched for an app that isn't there,
    // and a token the app no longer knows is replaced before sending.
    const app = await appStatus();
    if (!app.running) throw new Error(NOT_RUNNING);
    let { port, token } = await settings();
    if (!token || !app.paired) {
      toast(tab, "Connecting — press Connect in On-Screen Notes", "");
      token = await connect();
    }
    const file = blob ? await fromPage(blob, desc) : await getFile(desc, tab);
    const x = await extras(desc);
    let r = await post(file, desc, token, port, x.headers);
    if (r.status === 401) {
      // The app forgot this extension: ask again, once.
      await api.storage.local.set({ token: "" });
      toast(tab, "Connecting — press Connect in On-Screen Notes", "");
      token = await connect();
      r = await post(file, desc, token, port, x.headers);
    }
    if (!r.ok) {
      const j = await r.json().catch(() => ({}));
      throw new Error(`On-Screen Notes turned it down: ${j.error || r.status}`);
    }
    const who = desc.author && !x.tags ? ` — posted by @${desc.author}` : "";
    const grab = file.grabbed ? " (as shown on screen: the site would not hand over the file)" : "";
    refreshSlides();
    return { ok: true, message: `Sent to On-Screen Notes${sentNote(x)}${who}${grab}` };
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  }
}

// ---- Find artist ------------------------------------------------------
// The app holds a list of pictures; this tab looks each up on Google
// Lens, and the user picks who made it from the results. The tab doing it
// and the picture it is on are kept across the service worker's restarts.

let lens = null; // { tabId, id, index, total, mode?, sourceTabId?, desc? }
const lensPictures = new Map(); // picture id → data: URL
// "Find the artist, then send" from a page: the file fetched, by id.
const lensFiles = new Map();

async function lensLoad() {
  if (lens) return lens;
  try {
    lens = (await api.storage.session.get("lens")).lens || null;
  } catch {
    /* no session storage: memory only */
  }
  return lens;
}

async function lensSave(value) {
  lens = value;
  try {
    await api.storage.session.set({ lens: value });
  } catch {
    /* memory only */
  }
  setLensMenu(!!value);
}

function setLensMenu(visible) {
  Promise.resolve(api.contextMenus.update("osn-lens-tag", { visible })).catch(() => {});
}

// A request to the app as the connected extension; connects first if it
// must (unless `quiet`). The parsed JSON, or a Blob for `blob`.
async function appRequest(method, path, { headers = {}, body = undefined, blob = false, quiet = false } = {}) {
  let { port, token } = await settings();
  if (!token) {
    if (quiet) throw new Error("not connected");
    token = await connect();
  }
  const go = (t) => fetch(`http://127.0.0.1:${port}${path}`, { method, headers: { "X-OSN": "1", "X-OSN-Token": t, ...headers }, body });
  let r;
  try {
    r = await go(token);
    if (r.status === 401) {
      if (quiet) throw new Error("not connected");
      await api.storage.local.set({ token: "" });
      r = await go(await connect());
    }
  } catch {
    throw new Error(NOT_RUNNING);
  }
  if (!r.ok) {
    const j = await r.json().catch(() => ({}));
    throw new Error(j.error || `On-Screen Notes answered ${r.status}`);
  }
  return blob ? r.blob() : r.json();
}

function bytesToBase64(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

// A picture as a JPEG data: URL at most 1600 on its long side, for Lens.
async function lensJpeg(blob) {
  const bmp = await createImageBitmap(blob);
  const k = Math.min(1, 1600 / Math.max(bmp.width, bmp.height, 1));
  const c = new OffscreenCanvas(Math.max(1, Math.round(bmp.width * k)), Math.max(1, Math.round(bmp.height * k)));
  c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
  const jpeg = await c.convertToBlob({ type: "image/jpeg", quality: 0.9 });
  return `data:image/jpeg;base64,${bytesToBase64(new Uint8Array(await jpeg.arrayBuffer()))}`;
}

async function lensPicture(l) {
  if (!lensPictures.has(l.id)) {
    let url;
    if (l.mode === "browser") {
      // Lost with a restart of the background: fetched again.
      if (!lensFiles.has(l.id)) lensFiles.set(l.id, await getFile(l.desc, { id: l.sourceTabId }));
      url = await lensJpeg(lensFiles.get(l.id).blob);
    } else {
      const blob = await appRequest("GET", `/v1/queue/${l.id}/picture`, { blob: true });
      url = `data:image/jpeg;base64,${bytesToBase64(new Uint8Array(await blob.arrayBuffer()))}`;
    }
    lensPictures.clear();
    lensPictures.set(l.id, url);
  }
  return lensPictures.get(l.id);
}

// "Find the artist, then send" from a page: look the picture up in a new
// tab; it is sent once the artist is picked there (or without one).
async function lensFromPage(desc, tab) {
  const file = await getFile(desc, tab);
  const id = `b${Date.now()}`;
  lensFiles.clear();
  lensFiles.set(id, file);
  lensPictures.clear();
  lensPictures.set(id, await lensJpeg(file.blob));
  const t = await api.tabs.create({ url: LENS_URL, index: tab.index + 1, openerTabId: tab.id });
  await lensSave({ mode: "browser", tabId: t.id, id, index: 1, total: 1, sourceTabId: tab.id, desc });
  return { ok: true, message: "Looking it up on Google Lens — pick the artist there" };
}

// Send the picture looked up from a page, with the artist picked (or
// none), then back to the page it came from.
async function sendPicked(l, pick) {
  const desc = { ...l.desc, author: pick.author || l.desc.author, site: pick.site || l.desc.site, post: pick.post || l.desc.post, tags: pick.author ? [pick.author] : [] };
  const source = { id: l.sourceTabId };
  const file = lensFiles.get(l.id);
  const r = file ? await send(desc, source, file.blob) : await send(desc, source);
  if (r.ok && pick.author) await rememberArtist(pick.author, pick.site);
  await lensSave(null);
  lensFiles.delete(l.id);
  await Promise.resolve(api.tabs.remove(l.tabId)).catch(() => {});
  await Promise.resolve(api.tabs.update(l.sourceTabId, { active: true })).catch(() => {});
  toast(source, r.ok ? r.message : r.error, r.ok ? "ok" : "error");
  return r;
}

// The artist tagged last on Lens: often the next picture's too ("Same
// as last"). Kept across restarts.
async function rememberArtist(author, site) {
  await api.storage.local.set({ lastArtist: { author, site: site || "" } });
}

async function lastArtist() {
  return (await api.storage.local.get({ lastArtist: null })).lastArtist;
}

// Tag the picture being looked up with the last artist.
async function lensSame() {
  const last = await lastArtist();
  if (!last || !last.author) return { ok: false, error: "No artist tagged yet to repeat" };
  try {
    return await lensTag(last.author, last.site, "");
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  }
}

// The lookup as the bar in tab `tabId` shows it: the lookup tab, or a tab
// opened from it.
async function lensStateFor(tabId, openerTabId) {
  const l = await lensLoad();
  if (!l || (l.tabId !== tabId && openerTabId !== l.tabId)) return { active: false };
  return { active: true, index: l.index, total: l.total, mode: l.mode || "app", last: await lastArtist() };
}

// The bar follows the lookup: each page the lookup tab (or a tab opened
// from it) loads gets it again.
api.tabs.onUpdated.addListener(async (tabId, info, tab) => {
  if (info.status !== "complete") return;
  const state = await lensStateFor(tabId, tab && tab.openerTabId);
  if (state.active) api.tabs.sendMessage(tabId, { type: "osn-lens-show", state }, { frameId: 0 }).catch(() => {});
});

// On to the app's next picture in `tabId`, or done.
async function lensNext(tabId) {
  const j = await appRequest("GET", "/v1/queue/next");
  if (j.done) {
    await lensSave(null);
    const tab = { id: tabId };
    if (j.total) toast(tab, `Finding artists done: ${j.tagged} tagged, ${j.skipped} skipped`, "ok");
    else toast(tab, "No pictures are waiting — press Find artist in On-Screen Notes", "error");
    return { ok: true, done: true };
  }
  await lensSave({ tabId, id: j.id, index: j.index, total: j.total });
  await api.tabs.update(tabId, { url: LENS_URL });
  return { ok: true };
}

// Runs inside Google's page (not the extension's sandbox): hand Lens the
// picture as a user would — its file box, else a paste, else a drop.
// Returns how it went in, or "none". Google's page leaving (on to the
// results) is the sign it worked.
async function uploadToLens(dataUrl) {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const bin = atob(dataUrl.split(",")[1]);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const file = new File([bytes], "picture.jpg", { type: "image/jpeg" });
  const dt = () => {
    const d = new DataTransfer();
    d.items.add(file);
    return d;
  };
  const start = location.href;
  const left = async (ms) => {
    for (const t = Date.now(); Date.now() - t < ms; await sleep(150)) if (location.href !== start) return true;
    return false;
  };
  const waitFor = async (find, ms) => {
    for (const t = Date.now(); Date.now() - t < ms; await sleep(150)) {
      const x = find();
      if (x) return x;
    }
    return null;
  };
  const findInput = () => document.querySelector('input[type="file"]');
  let input = await waitFor(findInput, 2500);
  if (!input) {
    const button = document.querySelector('[aria-label="Search by image"], [aria-label*="by image" i], [aria-label*="Lens" i]');
    if (button) {
      button.click();
      input = await waitFor(findInput, 5000);
    }
  }
  if (input) {
    input.files = dt().files;
    input.dispatchEvent(new Event("change", { bubbles: true }));
    if (await left(10000)) return "input";
  }
  try {
    (document.activeElement || document.body).dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt(), bubbles: true, cancelable: true }));
  } catch {
    /* no ClipboardEvent with data here */
  }
  if (await left(6000)) return "paste";
  const zone = document.querySelector('[role="dialog"]') || document.body;
  for (const type of ["dragenter", "dragover", "drop"]) {
    try {
      zone.dispatchEvent(new DragEvent(type, { dataTransfer: dt(), bubbles: true, cancelable: true }));
    } catch {
      /* no DragEvent with data here */
    }
  }
  if (await left(6000)) return "drop";
  return "none";
}

async function lensTag(author, site, post) {
  const l = await lensLoad();
  if (!l) throw new Error("No picture is being looked up — press Find artist in On-Screen Notes");
  const clean = (author || "").replace(/^@/, "").trim();
  if (!clean) throw new Error("No name there to tag with");
  if (l.mode === "browser") return sendPicked(l, { author: clean, site, post });
  try {
    await appRequest("POST", `/v1/queue/${l.id}/artist`, { headers: { "X-OSN-Author": enc(clean), "X-OSN-Site": enc(site), "X-OSN-Post": enc(post) } });
  } catch (e) {
    if (/not waiting|no such/.test(e.message)) {
      // Stopped (or answered) in the app meanwhile.
      await lensSave(null);
      throw new Error("On-Screen Notes isn't looking this picture up any more");
    }
    throw e;
  }
  await rememberArtist(clean, site);
  return lensNext(l.tabId);
}

async function lensMessage(msg, sender) {
  const tabId = sender.tab && sender.tab.id;
  switch (msg.type) {
    case "osn-lens-begin":
      return lensNext(tabId);
    case "osn-lens-state":
      return lensStateFor(tabId, sender.tab && sender.tab.openerTabId);
    case "osn-lens-same":
      return lensSame();
    case "osn-lens-browser":
      return lensFromPage(msg.desc, sender.tab);
    case "osn-lens-upload-here": {
      const l = await lensLoad();
      if (!l || l.tabId !== tabId) return { how: "inactive" };
      const picture = await lensPicture(l);
      try {
        const [res] = await api.scripting.executeScript({ target: { tabId, frameIds: [0] }, world: "MAIN", func: uploadToLens, args: [picture] });
        const how = res && res.result;
        return { how: how || "left", picture: how === "none" ? picture : "" };
      } catch {
        // The page left mid-way: on to the results.
        return { how: "left" };
      }
    }
    case "osn-lens-artist":
      return lensTag(msg.author, msg.site, msg.post);
    case "osn-lens-skip": {
      const l = await lensLoad();
      if (!l) return { ok: false, error: "Nothing to skip" };
      // From a page: send it without an artist.
      if (l.mode === "browser") return sendPicked(l, {});
      await appRequest("POST", `/v1/queue/${l.id}/skip`);
      return lensNext(l.tabId);
    }
    case "osn-lens-stop": {
      const l = await lensLoad();
      if (l && l.mode === "browser") {
        await lensSave(null);
        lensFiles.delete(l.id);
        await Promise.resolve(api.tabs.remove(l.tabId)).catch(() => {});
        return { ok: true };
      }
      await appRequest("POST", "/v1/queue/stop").catch(() => {});
      await lensSave(null);
      return { ok: true };
    }
  }
  return { ok: false, error: "unknown" };
}

// "Tag as the artist" on a link (who it points at) or a highlighted name.
async function tagFromMenu(info, tab) {
  // A right-clicked link names who it points at; else the highlighted name.
  tagWith({ href: info.linkUrl || "", text: info.selectionText || info.linkText || "", selection: info.selectionText || "", page: info.pageUrl || "" }, tab, false);
}

// The "Tag as the artist" key: the highlighted name first (picked on
// purpose), else the link under the pointer.
async function tagHovered(tab) {
  if (!(await ensureContent(tab))) return;
  const info = await api.tabs.sendMessage(tab.id, { type: "osn-hovered-link" }, { frameId: 0 }).catch(() => null);
  tagWith(info || {}, tab, true);
}

// Tag the picture being looked up with who `info` names: a link (`href`,
// its `text`), or a highlighted name (`selection`).
async function tagWith(info, tab, selectionFirst) {
  const S = globalThis.OSNSites;
  const fromLink = () => (info.href ? S.authorFromLink(info.href, info.text || "") : null);
  const fromText = () => (info.selection ? { author: S.handleFromText(info.selection), site: "", post: info.page || "" } : null);
  const who = selectionFirst ? fromText() || fromLink() : fromLink() || fromText();
  if (!who || !who.author) {
    toast(tab, "No name to read there — point at the artist's link, or highlight their name", "error");
    return;
  }
  try {
    await lensTag(who.author, who.site, who.post || info.href || "");
    toast(tab, `Tagged #${who.author} in On-Screen Notes`, "ok");
  } catch (e) {
    toast(tab, e.message || String(e), "error");
  }
}

// Chrome's service worker has no `OSNSites` unless it is loaded here;
// Firefox's event page has no `importScripts`, and does not need it.
if (typeof importScripts === "function") {
  try {
    importScripts("sites.js");
  } catch {
    /* names fall back to the page's */
  }
}
