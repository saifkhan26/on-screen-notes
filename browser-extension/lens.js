// "Find artist" in the page: takes over the tab On-Screen Notes opens
// (http://127.0.0.1:<port>/v1/queue), and on Google's pages, while that
// tab is looking pictures up, has the picture uploaded to Lens. A bar
// shows over the results — and over any page the lookup tab goes to, or a
// tab opened from it (background.js says when): the artists its links
// point at, one click (or `1`–`6`) to tag, Same as last (`S`), Skip and
// Stop. The steps between pages are background.js's.
"use strict";
(function () {
  if (globalThis.__osnLensLoaded || window !== window.top) return;
  globalThis.__osnLensLoaded = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const S = globalThis.OSNSites;
  const onQueuePage = location.hostname === "127.0.0.1" && location.pathname === "/v1/queue";
  const onGoogle = /(^|\.)google\.[a-z.]+$/.test(location.hostname);
  // Chips at most, and the keys that pick them.
  const CHIPS = 6;

  const ready = () => new Promise((r) => (document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", r, { once: true }) : r()));

  if (onQueuePage) {
    ready().then(() => {
      // The page's "nothing happened?" note stays hidden.
      document.documentElement.dataset.osnTaken = "1";
      api.runtime.sendMessage({ type: "osn-lens-begin" }).catch(() => {});
    });
    return;
  }

  function isResults() {
    const p = new URLSearchParams(location.search);
    return (location.pathname === "/search" && (p.has("vsrid") || p.get("udm") === "26" || p.has("lns_mode"))) || location.hostname.startsWith("lens.");
  }

  let bar = null;

  function makeBar(state) {
    const host = document.createElement("osn-lens-bar");
    host.style.cssText = "all: initial; position: fixed; left: 50%; bottom: 16px; transform: translateX(-50%); z-index: 2147483647;";
    const root = host.attachShadow({ mode: "open" });
    root.innerHTML =
      "<style>" +
      ".bar{font:13px/1.4 system-ui,sans-serif;color:#f4f4f5;background:#18181b;border:1px solid #3f3f46;border-radius:10px;padding:8px 10px;" +
      "box-shadow:0 6px 24px rgba(0,0,0,.4);display:flex;flex-direction:column;gap:6px;max-width:min(760px,92vw)}" +
      ".row{display:flex;gap:6px;align-items:center;flex-wrap:wrap}" +
      ".title{font-weight:600;margin-right:4px}.muted{color:#a1a1aa;font-size:12px}" +
      "button{font:inherit;color:inherit;background:#27272a;border:1px solid #3f3f46;border-radius:999px;padding:2px 10px;cursor:pointer}" +
      "button:hover{border-color:#a1a1aa}button.plain{border-radius:6px;background:transparent}" +
      "kbd{font:11px ui-monospace,monospace;color:#a1a1aa;margin-right:4px}" +
      "img{height:44px;border-radius:4px;cursor:grab}" +
      "</style>" +
      '<div class="bar"><div class="row"><img hidden draggable="true"><span class="title"></span><span class="chips row"></span>' +
      '<button class="plain same" hidden></button><button class="plain skip">Skip</button><button class="plain stop">Stop</button></div><div class="muted note"></div></div>';
    (document.body || document.documentElement).appendChild(host);
    const b = { host, root, state, list: [], title: root.querySelector(".title"), chips: root.querySelector(".chips"), note: root.querySelector(".note"), img: root.querySelector("img") };
    const fromPage = state.mode === "browser";
    b.title.textContent = fromPage ? "On-Screen Notes · who made it? It's sent once you pick" : `On-Screen Notes · picture ${state.index} of ${state.total}`;
    if (fromPage) {
      root.querySelector(".skip").textContent = "Send without an artist";
      root.querySelector(".stop").textContent = "Cancel";
    }
    const keyNote = onGoogle ? " (Space)" : "";
    root.querySelector(".skip").title = `Skip this picture${keyNote}`;
    root.querySelector(".stop").title = `Stop finding artists${onGoogle ? " (Esc)" : ""}`;
    // The artist tagged last: often this one's too.
    const same = root.querySelector(".same");
    if (state.last && state.last.author) {
      same.innerHTML = "<kbd>S</kbd>";
      same.append(`Same as last: @${state.last.author}`);
      same.title = `Tag it #${state.last.author}${state.last.site ? ` (${state.last.site})` : ""}, as the picture before`;
      same.hidden = false;
      same.addEventListener("click", sameAsLast);
    }
    root.querySelector(".skip").addEventListener("click", skip);
    root.querySelector(".stop").addEventListener("click", stop);
    return b;
  }

  function sameAsLast() {
    if (bar && bar.state.last && bar.state.last.author) send({ type: "osn-lens-same" }, `Tagging #${bar.state.last.author}`);
  }

  function skip() {
    send({ type: "osn-lens-skip" }, "Skipped");
  }

  function stop() {
    send({ type: "osn-lens-stop" }, "Stopped");
    closeBar();
  }

  function closeBar() {
    if (bar) bar.host.remove();
    bar = null;
  }

  async function send(msg, doing) {
    if (!bar) return;
    bar.note.textContent = `${doing}…`;
    const r = await api.runtime.sendMessage(msg).catch((e) => ({ ok: false, error: String(e?.message || e) }));
    if (!bar) return;
    if (r && r.ok === false) {
      bar.note.textContent = r.error || "That didn't work";
      return;
    }
    // Done, or the lookup tab moves on: this bar has had its say. (In the
    // lookup tab itself the page is replaced anyway.)
    if (r && r.done) bar.note.textContent = "All done";
    else bar.note.textContent = "On to the next picture in the lookup tab";
    setTimeout(closeBar, 1600);
  }

  // The artists this page names: who the page itself is by first (a post
  // or a profile), then whom its links point at, most linked first.
  function scan() {
    const found = new Map();
    const add = (r, weight) => {
      if (!r) return;
      const k = `${r.site}:${r.author.toLowerCase()}`;
      const e = found.get(k) || { ...r, n: 0, here: false };
      e.n += weight;
      if (weight > 1) e.here = true;
      if (!e.post && r.post) e.post = r.post;
      found.set(k, e);
    };
    if (!onGoogle) add(S.authorFromLink(location.href, document.title), 1000);
    for (const a of document.querySelectorAll("a[href]")) {
      if (bar && bar.host.contains(a)) continue;
      add(S.authorFromLink(a.href, a.textContent), 1);
    }
    return [...found.values()].sort((a, b) => b.n - a.n).slice(0, CHIPS);
  }

  function pick(e) {
    send({ type: "osn-lens-artist", author: e.author, site: e.site, post: e.post }, `Tagging #${e.author}`);
  }

  function showChips() {
    if (!bar) return;
    const list = scan();
    const sig = list.map((e) => `${e.site}${e.author}${e.n}`).join("|");
    if (sig === bar.sig) return;
    bar.sig = sig;
    bar.list = list;
    bar.chips.textContent = "";
    list.forEach((e, i) => {
      const b = document.createElement("button");
      const k = document.createElement("kbd");
      k.textContent = String(i + 1);
      b.append(k, `@${e.author} · ${e.site}${e.here ? " · this page" : e.n > 1 ? ` ×${e.n}` : ""}`);
      b.title = `Tag the picture #${e.author} in On-Screen Notes (${i + 1})`;
      b.addEventListener("click", () => pick(e));
      bar.chips.appendChild(b);
    });
    if (!list.length) bar.note.textContent = "No artist links found yet — open a result, then right-click a link or a highlighted name → Tag as the artist";
    else bar.note.textContent = "Click who made it, or press its number — or right-click any link or highlighted name → Tag as the artist";
  }

  function typingIn(e) {
    const el = (e.composedPath && e.composedPath()[0]) || e.target;
    if (!(el instanceof Element)) return false;
    return el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName);
  }

  // The bar's keys: numbers pick a chip, S is Same as last; on Google's
  // pages Space skips and Esc stops (elsewhere the page keeps those).
  window.addEventListener(
    "keydown",
    (e) => {
      if (!bar || e.repeat || e.ctrlKey || e.altKey || e.metaKey || typingIn(e)) return;
      const n = /^(?:Digit|Numpad)([1-9])$/.exec(e.code);
      let act = null;
      if (n && bar.list[Number(n[1]) - 1]) {
        const chip = bar.list[Number(n[1]) - 1];
        act = () => pick(chip);
      } else if (e.code === "KeyS" && !e.shiftKey && bar.state.last && bar.state.last.author) act = sameAsLast;
      else if (onGoogle && e.code === "Space") act = skip;
      else if (onGoogle && e.code === "Escape") act = stop;
      if (!act) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      act();
    },
    true,
  );

  // The chips again as results come in and the page scrolls.
  function watch() {
    showChips();
    let ticks = 0;
    const timer = setInterval(() => {
      if (!bar) return clearInterval(timer);
      showChips();
      if (++ticks > 40) clearInterval(timer);
    }, 1500);
    let pending = 0;
    window.addEventListener(
      "scroll",
      () => {
        clearTimeout(pending);
        pending = setTimeout(showChips, 400);
      },
      { passive: true },
    );
  }

  function onPage(state) {
    if (bar) return;
    bar = makeBar(state);
    watch();
  }

  async function onUpload(state) {
    if (bar) return;
    bar = makeBar(state);
    bar.note.textContent = "Handing the picture to Google Lens…";
    const r = await api.runtime.sendMessage({ type: "osn-lens-upload-here" }).catch(() => null);
    if (!r || r.how !== "none") return; // Lens took it: the results page comes next.
    // By hand: the picture to drag onto the page, or to copy and paste.
    bar.img.src = r.picture;
    bar.img.hidden = false;
    bar.note.textContent = "Lens didn't take the picture by itself — drag it from this bar onto the page, or click it to copy, then press Ctrl+V";
    bar.img.addEventListener("click", async () => {
      try {
        const blob = await (await fetch(r.picture)).blob();
        const png = await new Promise((resolve) => {
          const img = new Image();
          img.onload = () => {
            const c = document.createElement("canvas");
            c.width = img.naturalWidth;
            c.height = img.naturalHeight;
            c.getContext("2d").drawImage(img, 0, 0);
            c.toBlob(resolve, "image/png");
          };
          img.src = URL.createObjectURL(blob);
        });
        await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
        bar.note.textContent = "Copied — press Ctrl+V on this page";
      } catch {
        bar.note.textContent = "Couldn't copy it — drag it onto the page instead";
      }
    });
  }

  // Any page the lookup tab goes to (or a tab opened from it): background.js
  // says so once it has loaded.
  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || msg.type !== "osn-lens-show") return;
    // Google's own pages set themselves up (below).
    if (!onGoogle && msg.state && msg.state.active) ready().then(() => onPage(msg.state));
    sendResponse({ ok: true });
  });

  if (onGoogle) {
    ready().then(async () => {
      const state = await api.runtime.sendMessage({ type: "osn-lens-state" }).catch(() => null);
      if (!state || !state.active) return;
      if (isResults()) onPage(state);
      else if (location.pathname === "/" || location.pathname === "/webhp" || location.pathname === "/imghp") onUpload(state);
    });
  }
})();
