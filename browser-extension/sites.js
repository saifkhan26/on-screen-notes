// What can be read from addresses alone: the biggest version of a
// picture, and who posted what a page shows. Loaded before content.js
// (as `OSNSites`); node runs it too, for the tests in tests/.
"use strict";
(function (root) {
  // Instagram paths that are not someone's profile.
  const NOT_IG_USERS = new Set([
    "p", "reel", "reels", "tv", "explore", "stories", "direct", "accounts", "about", "legal", "developer",
    "web", "challenge", "emails", "session", "api", "graphql", "static", "privacy", "terms", "ads", "help",
    "your_activity", "notifications", "create", "threads", "meta", "lite",
  ]);
  // X paths that are not someone's profile.
  const NOT_X_USERS = new Set(["i", "home", "search", "explore", "notifications", "messages", "settings", "compose", "intent", "hashtag"]);

  function isIgUser(s) {
    return /^[A-Za-z0-9._]{1,30}$/.test(s) && !NOT_IG_USERS.has(s.toLowerCase());
  }

  function isXUser(s) {
    return /^[A-Za-z0-9_]{1,15}$/.test(s) && !NOT_X_USERS.has(s.toLowerCase());
  }

  // A Bluesky handle as a tag: "name.bsky.social" → "name"; a bare
  // account id (did:…) is no name at all.
  function bskyHandle(h) {
    h = decodeURIComponent(h || "").trim().replace(/^@/, "");
    if (!h || h.startsWith("did:")) return "";
    return h.replace(/\.bsky\.social$/i, "");
  }

  // A picture's address, with the addresses of bigger versions of it
  // before it where the site has them: best first, the address itself
  // last.
  function biggerUrls(url) {
    let u;
    try {
      u = new URL(url);
    } catch {
      return [url];
    }
    const out = [];
    const host = u.hostname;
    if (host === "pbs.twimg.com" && u.pathname.startsWith("/media/")) {
      if (u.searchParams.has("name") || u.searchParams.has("format")) {
        const v = new URL(u.href);
        v.searchParams.set("name", "orig");
        out.push(v.href);
      } else {
        const m = u.pathname.match(/^(\/media\/[^.:/]+\.\w+)(:\w+)?$/);
        if (m) {
          const v = new URL(u.href);
          v.pathname = m[1] + ":orig";
          out.push(v.href);
        }
      }
    } else if (host === "i.pinimg.com") {
      const m = u.pathname.match(/^\/(\d+x\d*)\/(.+)$/);
      if (m) {
        out.push(`${u.origin}/originals/${m[2]}`);
        if (m[1] !== "736x") out.push(`${u.origin}/736x/${m[2]}`);
      }
    } else if (/(^|\.)artstation\.com$/.test(host)) {
      const small = /\/(small|smaller|medium|small_square|smaller_square|micro_square|large)\//;
      if (/\/images\//.test(u.pathname) && small.test(u.pathname)) {
        for (const size of ["4k", "large"]) {
          const v = new URL(u.href);
          v.pathname = u.pathname.replace(small, `/${size}/`);
          out.push(v.href);
        }
      }
    }
    out.push(url);
    return [...new Set(out)];
  }

  // The candidates of a `srcset`, read as the HTML spec does: an address
  // runs to the next space, and commas at its end separate candidates.
  function splitSrcset(s) {
    const out = [];
    let i = 0;
    while (i < s.length) {
      while (i < s.length && /[\s,]/.test(s[i])) i++;
      let j = i;
      while (j < s.length && !/\s/.test(s[j])) j++;
      let url = s.slice(i, j);
      let desc = "";
      if (url.endsWith(",")) {
        url = url.replace(/,+$/, "");
        i = j;
      } else {
        let k = j;
        while (k < s.length && s[k] !== ",") k++;
        desc = s.slice(j, k).trim();
        i = k + 1;
      }
      if (url) out.push({ url, desc });
    }
    return out;
  }

  // The biggest candidate of one or more `srcset`s, as a full address.
  function largestFromSrcset(srcsets, base) {
    let best = null;
    let bestSize = -1;
    for (const s of [].concat(srcsets || [])) {
      if (!s) continue;
      for (const { url, desc } of splitSrcset(s)) {
        let size = 1;
        const w = desc.match(/^(\d+(?:\.\d+)?)w$/);
        const x = desc.match(/^(\d+(?:\.\d+)?)x$/);
        if (w) size = parseFloat(w[1]);
        else if (x) size = parseFloat(x[1]) * 1000;
        if (size > bestSize) {
          bestSize = size;
          best = url;
        }
      }
    }
    if (!best) return null;
    try {
      return new URL(best, base).href;
    } catch {
      return null;
    }
  }

  // Who posted what a page shows, from its address alone:
  // `{ author, site, post }`, or null when the address does not say.
  function authorFromUrl(href) {
    let u;
    try {
      u = new URL(href);
    } catch {
      return null;
    }
    const host = u.hostname.replace(/^(www|mobile|m)\./, "");
    const parts = u.pathname.split("/").filter(Boolean);
    if (host === "x.com" || host === "twitter.com") {
      if (parts.length >= 3 && parts[1] === "status" && isXUser(parts[0]) && /^\d+$/.test(parts[2])) {
        return { author: parts[0], site: "X", post: `https://x.com/${parts[0]}/status/${parts[2]}` };
      }
      return null;
    }
    if (host === "instagram.com") {
      if (parts.length >= 3 && ["p", "reel", "reels", "tv"].includes(parts[1]) && isIgUser(parts[0])) {
        return { author: parts[0], site: "Instagram", post: `https://www.instagram.com/${parts[0]}/${parts[1]}/${parts[2]}/` };
      }
      return null;
    }
    if (host === "bsky.app") {
      if (parts[0] === "profile" && parts[2] === "post" && parts[1]) {
        const author = bskyHandle(parts[1]);
        return author ? { author, site: "Bluesky", post: `https://bsky.app/profile/${parts[1]}/post/${parts[3] || ""}` } : null;
      }
      return null;
    }
    if (host === "deviantart.com") {
      if (parts.length >= 3 && parts[1] === "art") {
        return { author: parts[0], site: "DeviantArt", post: `https://www.deviantart.com/${parts.slice(0, 3).join("/")}` };
      }
      return null;
    }
    const tumblr = host.match(/^([a-z0-9-]+)\.tumblr\.com$/);
    if (tumblr && !["www", "assets", "media", "api"].includes(tumblr[1]) && !/^\d+$/.test(tumblr[1])) {
      return { author: tumblr[1], site: "Tumblr", post: parts[0] === "post" ? `${u.origin}/${parts.slice(0, 2).join("/")}` : "" };
    }
    if (host === "tumblr.com" && parts.length >= 2 && /^\d+$/.test(parts[1]) && /^[a-z0-9-]+$/.test(parts[0])) {
      return { author: parts[0], site: "Tumblr", post: `https://www.tumblr.com/${parts[0]}/${parts[1]}` };
    }
    const artstation = host.match(/^([a-z0-9_-]+)\.artstation\.com$/);
    if (artstation && !["www", "cdna", "cdnb", "cdn", "magazine", "help"].includes(artstation[1])) {
      return { author: artstation[1], site: "ArtStation", post: "" };
    }
    return null;
  }

  // An X post's author and address from its link ("/name/status/123").
  function xPostFromHref(href, base) {
    let u;
    try {
      u = new URL(href, base);
    } catch {
      return null;
    }
    const m = u.pathname.match(/^\/([^/]+)\/status\/(\d+)/);
    return m && isXUser(m[1]) ? { author: m[1], post: `https://x.com/${m[1]}/status/${m[2]}` } : null;
  }

  // A file name from an address: its last part, if it has one.
  function nameFromUrl(url) {
    try {
      const u = new URL(url);
      if (u.protocol === "data:" || u.protocol === "blob:") return "";
      const last = decodeURIComponent(u.pathname.split("/").filter(Boolean).pop() || "");
      return last.replace(/:\w+$/, "");
    } catch {
      return "";
    }
  }

  // Pages that show many posts: their address is no use for downloading
  // one video from them.
  function isFeedUrl(href) {
    let u;
    try {
      u = new URL(href);
    } catch {
      return true;
    }
    const host = u.hostname.replace(/^(www|mobile|m)\./, "");
    const parts = u.pathname.split("/").filter(Boolean);
    if (host === "x.com" || host === "twitter.com") return !(parts.length >= 3 && parts[1] === "status");
    if (host === "instagram.com") return !parts.some((p) => ["p", "reel", "reels", "tv"].includes(p)) || (parts[0] === "reels" && parts.length < 2);
    if (host === "bsky.app") return !(parts[0] === "profile" && parts[2] === "post");
    if (host === "facebook.com") return !parts.some((p) => ["videos", "reel", "watch", "posts"].includes(p)) && !u.searchParams.has("v");
    if (host === "youtube.com") return !(u.pathname === "/watch" || parts[0] === "shorts");
    if (host === "tiktok.com") return !parts.includes("video");
    if (host === "reddit.com") return !parts.includes("comments");
    if (host === "pinterest.com" || /\.pinterest\./.test(host)) return parts[0] !== "pin";
    return parts.length === 0;
  }

  // Cookies (as the browser's cookies API gives them) in the Netscape
  // format yt-dlp reads.
  function netscapeCookies(list) {
    const lines = ["# Netscape HTTP Cookie File"];
    for (const c of list || []) {
      if (!c || !c.name || /[\t\n\r]/.test(`${c.name}${c.value}${c.domain}${c.path}`)) continue;
      const domain = c.hostOnly ? c.domain.replace(/^\./, "") : c.domain.startsWith(".") ? c.domain : `.${c.domain}`;
      const sub = c.hostOnly ? "FALSE" : "TRUE";
      const expires = c.session || !c.expirationDate ? 0 : Math.floor(c.expirationDate);
      lines.push([c.httpOnly ? `#HttpOnly_${domain}` : domain, sub, c.path || "/", c.secure ? "TRUE" : "FALSE", expires, c.name, c.value].join("\t"));
    }
    return lines.join("\n") + "\n";
  }

  // A Google result link's real address: Google wraps some in
  // /url?q=… (or url=, imgrefurl= for picture results).
  function unwrapGoogle(href) {
    let u;
    try {
      u = new URL(href);
    } catch {
      return href;
    }
    if (/(^|\.)google\.[a-z.]+$/.test(u.hostname) && (u.pathname === "/url" || u.pathname === "/imgres")) {
      return u.searchParams.get("q") || u.searchParams.get("url") || u.searchParams.get("imgrefurl") || href;
    }
    return href;
  }

  const NOT_ARTSTATION_USERS = new Set(["artwork", "search", "learning", "marketplace", "prints", "jobs", "blogs", "challenges", "contests", "channels", "about", "community", "schools", "studios", "users", "magazine", "a", "p"]);
  const NOT_DEVIANT_USERS = new Set(["tag", "search", "watch", "notifications", "settings", "daily-deviations", "topic", "popular", "users", "core-membership", "shop", "about"]);

  // Who a result link (on Google Lens, or any page) points at: a post's
  // author, or a profile's owner. `text` is the link's own text, for
  // links whose address names no one (Instagram's /p/…).
  // `{ author, site, post }` or null.
  function authorFromLink(href, text) {
    href = unwrapGoogle(href);
    const post = authorFromUrl(href);
    if (post) return post;
    let u;
    try {
      u = new URL(href);
    } catch {
      return null;
    }
    const host = u.hostname.replace(/^(www|mobile|m)\./, "");
    const parts = u.pathname.split("/").filter(Boolean);
    const profile = (author, site) => ({ author, site, post: "" });
    if (host === "x.com" || host === "twitter.com") {
      if (parts.length >= 1 && isXUser(parts[0]) && (parts.length === 1 || ["media", "with_replies", "likes", "highlights", "photo"].includes(parts[1]))) return profile(parts[0], "X");
      return null;
    }
    if (host === "instagram.com") {
      if (parts.length >= 1 && isIgUser(parts[0]) && (parts.length === 1 || ["reels", "tagged", "saved"].includes(parts[1]))) return profile(parts[0], "Instagram");
      if (["p", "reel", "reels", "tv"].includes(parts[0])) {
        // "kaizin.art on Instagram: …", "Name (@kaizin.art) • Instagram photos…"
        const t = (text || "").trim();
        const at = t.match(/@([A-Za-z0-9._]{1,30})/);
        const on = t.match(/^([A-Za-z0-9._]{1,30}) on Instagram/);
        const name = (at && at[1]) || (on && on[1]);
        if (name && isIgUser(name)) return { author: name, site: "Instagram", post: u.origin + u.pathname };
      }
      return null;
    }
    if (host === "bsky.app") {
      const author = parts[0] === "profile" && parts[1] ? bskyHandle(parts[1]) : "";
      return author ? profile(author, "Bluesky") : null;
    }
    if (host === "deviantart.com") {
      if (parts.length === 1 && /^[A-Za-z0-9-]{1,40}$/.test(parts[0]) && !NOT_DEVIANT_USERS.has(parts[0].toLowerCase())) return profile(parts[0], "DeviantArt");
      return null;
    }
    if (host === "artstation.com") {
      if (parts.length >= 1 && /^[A-Za-z0-9_-]{1,40}$/.test(parts[0]) && !NOT_ARTSTATION_USERS.has(parts[0].toLowerCase())) return profile(parts[0], "ArtStation");
      return null;
    }
    if (host === "behance.net") {
      if (parts.length >= 1 && /^[A-Za-z0-9_-]{1,40}$/.test(parts[0]) && !["gallery", "search", "galleries", "joblist", "hire", "assets"].includes(parts[0].toLowerCase())) return profile(parts[0], "Behance");
      return null;
    }
    return null;
  }

  // A name the user highlighted, as a tag: the @handle in it, or its
  // words joined by hyphens.
  function handleFromText(text) {
    const t = (text || "").trim();
    const at = t.match(/@([A-Za-z0-9._-]{1,64})/);
    if (at) return at[1];
    return t.replace(/[^\p{L}\p{N}._\s-]/gu, "").trim().split(/\s+/).filter(Boolean).join("-").slice(0, 64);
  }

  // ---- Keys -----------------------------------------------------------

  // What a key can do: on what is under the pointer ("pointer", with the
  // mode content.js's `sendAll` takes), or on the page. `key`: the
  // default, empty for none.
  const KEY_ACTIONS = [
    { id: "send", label: "Send what's under the pointer", where: "pointer", mode: "send", key: "Alt+Shift+D" },
    { id: "post", label: "Send all pictures of the post under the pointer", where: "pointer", mode: "post", key: "" },
    { id: "frame", label: "Send the frame of the video under the pointer", where: "pointer", mode: "frame", key: "" },
    { id: "record", label: "Record the video under the pointer as it plays", where: "pointer", mode: "record", key: "" },
    { id: "artist", label: "Find the artist of the picture under the pointer, then send", where: "pointer", mode: "artist", key: "" },
    { id: "look", label: "Look for the picture under the pointer in my slides", where: "pointer", mode: "look", key: "" },
    { id: "pick", label: "Pick media to send", where: "page", key: "Alt+Shift+S" },
    { id: "grid", label: "All media on this page", where: "page", key: "Alt+Shift+A" },
    { id: "tags", label: "Tags for what you send", where: "page", key: "" },
    // While Find artist looks a picture up: the highlighted name, else the
    // link under the pointer.
    { id: "tag-artist", label: "Tag as the artist (Find artist): the highlighted name, or the link under the pointer", where: "link", key: "Alt+Shift+T" },
    { id: "same-artist", label: "Same as the last artist (Find artist)", where: "page", key: "" },
  ];

  function defaultKeys() {
    return Object.fromEntries(KEY_ACTIONS.map((a) => [a.id, a.key]));
  }

  // A key by where it is on the keyboard (so Shift or another layout
  // doesn't change its name): "D", "7", "Num7", "F2", "Space", "ArrowUp"…
  function keyName(code) {
    let m;
    if ((m = /^Key([A-Z])$/.exec(code))) return m[1];
    if ((m = /^Digit(\d)$/.exec(code))) return m[1];
    if ((m = /^Numpad(\d)$/.exec(code))) return `Num${m[1]}`;
    return code;
  }

  // A key press as "Ctrl+Alt+Shift+Meta+Key" (those that are held), or ""
  // for a modifier on its own.
  function keyCombo(e) {
    if (!e || !e.code || /^(Shift|Control|Alt|Meta|OS)(Left|Right)?$/.test(e.code)) return "";
    const parts = [];
    if (e.ctrlKey) parts.push("Ctrl");
    if (e.altKey) parts.push("Alt");
    if (e.shiftKey) parts.push("Shift");
    if (e.metaKey) parts.push("Meta");
    parts.push(keyName(e.code));
    return parts.join("+");
  }

  // Keys the browser keeps for itself (a page never sees them), or that
  // every page needs as they are.
  const RESERVED = new Set([
    "Ctrl+T", "Ctrl+W", "Ctrl+N", "Ctrl+Shift+N", "Ctrl+Shift+T", "Ctrl+Shift+W", "Ctrl+Tab", "Ctrl+Shift+Tab", "Ctrl+L",
    "Ctrl+R", "Ctrl+Shift+R", "F5", "Ctrl+F5", "F11", "F12", "Ctrl+Shift+I", "Ctrl+Shift+J", "Alt+F4", "Ctrl+Q",
    "Ctrl+C", "Ctrl+V", "Ctrl+X", "Ctrl+Z", "Ctrl+Y", "Ctrl+A", "Ctrl+F", "Ctrl+P", "Ctrl+S", "Ctrl+D", "Ctrl+H", "Ctrl+J",
    "Escape", "Tab", "Shift+Tab", "Enter", "Backspace", "Delete", "Space", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
  ]);

  // Whether `combo` can be a key: `{ ok, note }`, `note` saying why not,
  // or what to keep in mind.
  function keyCheck(combo) {
    if (!combo) return { ok: true, note: "" };
    if (RESERVED.has(combo)) return { ok: false, note: `${combo} belongs to the browser or every page — pick another` };
    if (!/(^|\+)(Ctrl|Alt|Meta)\+/.test(combo)) {
      return { ok: true, note: `${combo} works only while you aren't typing, and the site may use it too` };
    }
    return { ok: true, note: "" };
  }

  const api = {
    biggerUrls, splitSrcset, largestFromSrcset, authorFromUrl, xPostFromHref, isIgUser, isXUser, bskyHandle, nameFromUrl,
    isFeedUrl, netscapeCookies, unwrapGoogle, authorFromLink, handleFromText,
    KEY_ACTIONS, defaultKeys, keyName, keyCombo, keyCheck,
  };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.OSNSites = api;
})(typeof globalThis !== "undefined" ? globalThis : this);
