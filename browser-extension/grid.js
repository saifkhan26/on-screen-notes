// "All media on this page": every picture, GIF and video the page holds
// (from a size up), as a grid over the page: tick some, or all, and send
// them. Each tile says whether it is on a slide already. Uses what
// content.js shares (`__osnContent`); only in the top frame.
"use strict";
(function () {
  if (globalThis.__osnGrid || window !== window.top) return;
  const S = globalThis.OSNSites;
  // Smaller than this on both its shown and its own sides: an icon.
  const MIN = 100;

  let grid = null;

  // Every picture, GIF and video on the page, once each.
  function collect(min) {
    const C = globalThis.__osnContent;
    const out = [];
    const seen = new Set();
    const add = (m) => {
      const key = m && C.keyOf(m);
      if (!key || seen.has(key)) return;
      seen.add(key);
      out.push(m);
    };
    for (const el of document.querySelectorAll("img, video")) {
      const r = el.getBoundingClientRect();
      const w = Math.max(r.width, el.naturalWidth || el.videoWidth || 0);
      const h = Math.max(r.height, el.naturalHeight || el.videoHeight || 0);
      if (w < min || h < min) continue;
      add(C.asMedia(el));
    }
    for (const el of document.querySelectorAll("[style*='background']")) {
      const r = el.getBoundingClientRect();
      if (r.width < min || r.height < min) continue;
      add(C.asMedia(el));
    }
    return out;
  }

  function thumb(m) {
    if (m.kind === "image") return m.el.currentSrc || m.el.src;
    if (m.kind === "background") return m.url;
    if (m.kind === "video") return m.el.poster || "";
    return "";
  }

  function kindLabel(m) {
    if (m.kind === "video") return "Video";
    return /\.gif(\?|$)/i.test(thumb(m)) ? "GIF" : "Picture";
  }

  function size(m) {
    const w = m.el.naturalWidth || m.el.videoWidth || Math.round(m.el.getBoundingClientRect().width);
    const h = m.el.naturalHeight || m.el.videoHeight || Math.round(m.el.getBoundingClientRect().height);
    return w && h ? `${w}×${h}` : "";
  }

  function close() {
    if (!grid) return;
    window.removeEventListener("keydown", grid.onKey, true);
    grid.host.remove();
    grid = null;
  }

  function open() {
    const C = globalThis.__osnContent;
    if (!C) return;
    const items = collect(MIN).map((m) => ({ m, on: false, tile: null }));
    const host = document.createElement("osn-grid");
    host.style.cssText = "all: initial; position: fixed; inset: 0; z-index: 2147483647;";
    const root = host.attachShadow({ mode: "closed" });
    root.innerHTML =
      "<style>" +
      ":host{all:initial}.wrap{position:fixed;inset:0;background:rgba(9,9,11,.92);color:#f4f4f5;font:13px/1.4 system-ui,sans-serif;display:flex;flex-direction:column}" +
      ".top{display:flex;gap:8px;align-items:center;flex-wrap:wrap;padding:10px 16px;border-bottom:1px solid #3f3f46}" +
      ".title{font-weight:600;margin-right:8px}.muted{color:#a1a1aa}" +
      "button{font:inherit;color:inherit;background:#27272a;border:1px solid #3f3f46;border-radius:6px;padding:4px 12px;cursor:pointer}" +
      "button:hover{border-color:#a1a1aa}button.go{background:#f4f4f5;color:#18181b;border-color:#f4f4f5}button:disabled{opacity:.4;cursor:default}" +
      ".grid{flex:1;overflow:auto;padding:12px 16px;display:grid;grid-template-columns:repeat(auto-fill,minmax(170px,1fr));gap:10px;align-content:start}" +
      ".tile{position:relative;background:#18181b;border:2px solid #3f3f46;border-radius:8px;overflow:hidden;cursor:pointer;aspect-ratio:1}" +
      ".tile.on{border-color:#f4f4f5;box-shadow:0 0 0 2px #f4f4f5 inset}" +
      ".tile img{width:100%;height:100%;object-fit:contain;display:block;background:#0c0c0e}" +
      ".tile .ph{width:100%;height:100%;display:grid;place-items:center;color:#a1a1aa}" +
      ".tile .cap{position:absolute;left:0;right:0;bottom:0;padding:3px 6px;background:rgba(0,0,0,.7);font-size:11px;display:flex;justify-content:space-between;gap:6px}" +
      ".tile .have{position:absolute;left:6px;top:6px;background:#18181b;border:1px solid #a1a1aa;border-radius:999px;padding:1px 8px;font-size:11px}" +
      ".tile .tick{position:absolute;right:6px;top:6px;width:18px;height:18px;border-radius:4px;border:2px solid #f4f4f5;background:rgba(0,0,0,.5)}" +
      ".tile.on .tick{background:#f4f4f5}" +
      "</style>" +
      '<div class="wrap"><div class="top"><span class="title"></span>' +
      '<button class="all">Select all</button><button class="none">Clear</button><button class="new">Select ones not on a slide</button>' +
      '<span class="muted note"></span><span style="flex:1"></span>' +
      '<button class="go" disabled>Send</button><button class="close">Close</button></div><div class="grid"></div></div>';
    (document.body || document.documentElement).appendChild(host);
    const $ = (s) => root.querySelector(s);
    grid = { host, items };
    $(".title").textContent = items.length ? `${items.length} pictures, GIFs and videos on this page` : "No pictures on this page — scroll to load some, then open this again";
    $(".note").textContent = "Click to pick · scroll the page for more, then open this again";
    const count = () => {
      const n = items.filter((i) => i.on).length;
      const go = $(".go");
      go.disabled = n === 0;
      go.textContent = n ? `Send ${n}` : "Send";
    };
    const set = (it, on) => {
      it.on = on;
      it.tile.classList.toggle("on", on);
    };
    const box = $(".grid");
    for (const it of items) {
      const tile = document.createElement("div");
      tile.className = "tile";
      const src = thumb(it.m);
      if (src) {
        const img = document.createElement("img");
        img.loading = "lazy";
        img.src = src;
        tile.appendChild(img);
      } else {
        const ph = document.createElement("div");
        ph.className = "ph";
        ph.textContent = kindLabel(it.m);
        tile.appendChild(ph);
      }
      const cap = document.createElement("div");
      cap.className = "cap";
      cap.innerHTML = "<span></span><span></span>";
      cap.children[0].textContent = kindLabel(it.m);
      cap.children[1].textContent = size(it.m);
      tile.appendChild(cap);
      const tick = document.createElement("div");
      tick.className = "tick";
      tile.appendChild(tick);
      tile.title = S.nameFromUrl(src) || kindLabel(it.m);
      tile.addEventListener("click", () => {
        set(it, !it.on);
        count();
      });
      it.tile = tile;
      box.appendChild(tile);
    }
    $(".all").addEventListener("click", () => {
      items.forEach((it) => set(it, true));
      count();
    });
    $(".none").addEventListener("click", () => {
      items.forEach((it) => set(it, false));
      count();
    });
    $(".new").addEventListener("click", () => {
      items.forEach((it) => set(it, it.found !== undefined && it.found !== null && it.found.length === 0));
      count();
    });
    $(".close").addEventListener("click", close);
    $(".go").addEventListener("click", () => {
      const picked = items.filter((i) => i.on).map((i) => i.m);
      close();
      C.sendAll(picked, "send");
    });
    grid.onKey = (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("keydown", grid.onKey, true);
    count();
    // Which are on a slide already: a few at a time.
    let next = 0;
    const worker = async () => {
      while (grid && next < items.length) {
        const it = items[next++];
        const found = await C.checkMedia(it.m).catch(() => null);
        it.found = found;
        if (!grid || !found || !found.length) continue;
        const have = document.createElement("div");
        have.className = "have";
        have.textContent = `On slide ${found[0].index + 1}`;
        have.title = C.foundText(found).replace(/^ · /, "");
        it.tile.appendChild(have);
      }
    };
    for (let k = 0; k < 3; k++) worker();
  }

  globalThis.__osnGrid = {
    toggle() {
      if (grid) close();
      else open();
    },
  };
})();
