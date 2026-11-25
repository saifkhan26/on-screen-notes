// Run with: node --test browser-extension/tests/
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const S = require("../sites.js");

test("X posts name their author in the address", () => {
  assert.deepEqual(S.authorFromUrl("https://x.com/Antin_Illust/status/1789?s=20"), {
    author: "Antin_Illust",
    site: "X",
    post: "https://x.com/Antin_Illust/status/1789",
  });
  assert.equal(S.authorFromUrl("https://twitter.com/abc/status/5/photo/1").author, "abc");
  assert.equal(S.authorFromUrl("https://x.com/home"), null);
  assert.equal(S.authorFromUrl("https://x.com/i/status/123"), null);
  assert.equal(S.authorFromUrl("https://x.com/someone"), null);
});

test("Instagram addresses name the author when they hold it", () => {
  assert.deepEqual(S.authorFromUrl("https://www.instagram.com/kaizin.art/p/C1x2Y3/?img_index=2"), {
    author: "kaizin.art",
    site: "Instagram",
    post: "https://www.instagram.com/kaizin.art/p/C1x2Y3/",
  });
  assert.equal(S.authorFromUrl("https://www.instagram.com/nod.artss/reel/Abc/").author, "nod.artss");
  assert.equal(S.authorFromUrl("https://www.instagram.com/p/C1x2Y3/"), null);
  assert.equal(S.authorFromUrl("https://www.instagram.com/explore/p/x/"), null);
  assert.equal(S.authorFromUrl("https://www.instagram.com/"), null);
});

test("other sites", () => {
  assert.equal(S.authorFromUrl("https://bsky.app/profile/artist.bsky.social/post/3k").author, "artist");
  assert.equal(S.authorFromUrl("https://bsky.app/profile/did:plc:abc/post/3k"), null);
  assert.equal(S.authorFromUrl("https://www.deviantart.com/someone/art/Title-123").author, "someone");
  assert.equal(S.authorFromUrl("https://drawer.tumblr.com/post/123/slug").author, "drawer");
  assert.equal(S.authorFromUrl("https://www.tumblr.com/drawer/7234567").author, "drawer");
  assert.equal(S.authorFromUrl("https://painter.artstation.com/projects/abc").author, "painter");
  assert.equal(S.authorFromUrl("https://www.artstation.com/artwork/abc"), null);
  assert.equal(S.authorFromUrl("https://www.pinterest.com/pin/123/"), null);
  assert.equal(S.authorFromUrl("not a url"), null);
});

test("X post links", () => {
  assert.deepEqual(S.xPostFromHref("/Abc_1/status/42", "https://x.com/home"), { author: "Abc_1", post: "https://x.com/Abc_1/status/42" });
  assert.equal(S.xPostFromHref("/home", "https://x.com/"), null);
});

test("bigger versions of pictures come first", () => {
  assert.deepEqual(S.biggerUrls("https://pbs.twimg.com/media/GxY12ab?format=jpg&name=small"), [
    "https://pbs.twimg.com/media/GxY12ab?format=jpg&name=orig",
    "https://pbs.twimg.com/media/GxY12ab?format=jpg&name=small",
  ]);
  assert.deepEqual(S.biggerUrls("https://pbs.twimg.com/media/GxY12ab.jpg:large"), [
    "https://pbs.twimg.com/media/GxY12ab.jpg:orig",
    "https://pbs.twimg.com/media/GxY12ab.jpg:large",
  ]);
  assert.deepEqual(S.biggerUrls("https://i.pinimg.com/236x/aa/bb/cc/abc.jpg"), [
    "https://i.pinimg.com/originals/aa/bb/cc/abc.jpg",
    "https://i.pinimg.com/736x/aa/bb/cc/abc.jpg",
    "https://i.pinimg.com/236x/aa/bb/cc/abc.jpg",
  ]);
  assert.deepEqual(S.biggerUrls("https://i.pinimg.com/originals/aa/abc.gif"), ["https://i.pinimg.com/originals/aa/abc.gif"]);
  const art = S.biggerUrls("https://cdna.artstation.com/p/assets/images/images/012/345/678/medium/x.jpg?1");
  assert.equal(art[0], "https://cdna.artstation.com/p/assets/images/images/012/345/678/4k/x.jpg?1");
  assert.equal(art[1], "https://cdna.artstation.com/p/assets/images/images/012/345/678/large/x.jpg?1");
  assert.deepEqual(S.biggerUrls("https://example.com/a.png"), ["https://example.com/a.png"]);
});

test("the biggest srcset candidate is picked", () => {
  const ig = "https://scontent.cdninstagram.com/a.jpg?x=1 640w,https://scontent.cdninstagram.com/b.jpg?x=1 1080w,https://scontent.cdninstagram.com/c.jpg 750w";
  assert.equal(S.largestFromSrcset(ig, "https://www.instagram.com/"), "https://scontent.cdninstagram.com/b.jpg?x=1");
  assert.equal(S.largestFromSrcset(["a.jpg 1x, b.jpg 2x", "c.jpg 1500w"], "https://e.com/p/"), "https://e.com/p/b.jpg");
  assert.equal(S.largestFromSrcset("", "https://e.com/"), null);
  assert.equal(S.largestFromSrcset(null, "https://e.com/"), null);
});

test("feeds are told from single posts", () => {
  assert.equal(S.isFeedUrl("https://x.com/home"), true);
  assert.equal(S.isFeedUrl("https://x.com/a/status/1"), false);
  assert.equal(S.isFeedUrl("https://www.instagram.com/"), true);
  assert.equal(S.isFeedUrl("https://www.instagram.com/kaizin.art/"), true);
  assert.equal(S.isFeedUrl("https://www.instagram.com/p/C1/"), false);
  assert.equal(S.isFeedUrl("https://www.instagram.com/reels/C1/"), false);
  assert.equal(S.isFeedUrl("https://www.instagram.com/kaizin.art/reel/C1/"), false);
  assert.equal(S.isFeedUrl("https://www.youtube.com/watch?v=x"), false);
  assert.equal(S.isFeedUrl("https://www.youtube.com/"), true);
  assert.equal(S.isFeedUrl("https://www.pinterest.com/pin/123/"), false);
  assert.equal(S.isFeedUrl("https://example.com/"), true);
  assert.equal(S.isFeedUrl("https://example.com/video/1"), false);
});

test("cookies are written the way yt-dlp reads them", () => {
  const text = S.netscapeCookies([
    { name: "sessionid", value: "abc", domain: ".instagram.com", hostOnly: false, path: "/", secure: true, httpOnly: true, expirationDate: 1800000000.5 },
    { name: "csrftoken", value: "x1", domain: "www.instagram.com", hostOnly: true, path: "/", secure: true, httpOnly: false, session: true },
    { name: "bad\tname", value: "v", domain: ".a.com", path: "/" },
  ]);
  const lines = text.trim().split("\n");
  assert.equal(lines[0], "# Netscape HTTP Cookie File");
  assert.equal(lines[1], "#HttpOnly_.instagram.com\tTRUE\t/\tTRUE\t1800000000\tsessionid\tabc");
  assert.equal(lines[2], "www.instagram.com\tFALSE\t/\tTRUE\t0\tcsrftoken\tx1");
  assert.equal(lines.length, 3);
});

test("result links name who they point at", () => {
  const g = "https://www.google.com/url?sa=t&q=https%3A%2F%2Fx.com%2FAntin_Illust%2Fstatus%2F12&usg=x";
  assert.equal(S.unwrapGoogle(g), "https://x.com/Antin_Illust/status/12");
  assert.deepEqual(S.authorFromLink(g, ""), { author: "Antin_Illust", site: "X", post: "https://x.com/Antin_Illust/status/12" });
  assert.deepEqual(S.authorFromLink("https://x.com/Antin_Illust", ""), { author: "Antin_Illust", site: "X", post: "" });
  assert.equal(S.authorFromLink("https://x.com/Antin_Illust/media", "").author, "Antin_Illust");
  assert.equal(S.authorFromLink("https://x.com/home", ""), null);
  assert.equal(S.authorFromLink("https://x.com/search?q=a", ""), null);
  assert.equal(S.authorFromLink("https://www.instagram.com/kaizin.art/", "").author, "kaizin.art");
  assert.equal(S.authorFromLink("https://www.instagram.com/explore/", ""), null);
  assert.deepEqual(S.authorFromLink("https://www.instagram.com/p/C1x/", "kaizin.art on Instagram: \"sketch\""), {
    author: "kaizin.art",
    site: "Instagram",
    post: "https://www.instagram.com/p/C1x/",
  });
  assert.equal(S.authorFromLink("https://www.instagram.com/p/C1x/", "Kai (@kaizin.art) • Instagram photos").author, "kaizin.art");
  assert.equal(S.authorFromLink("https://www.instagram.com/p/C1x/", "a picture"), null);
  assert.equal(S.authorFromLink("https://www.artstation.com/painter", "").author, "painter");
  assert.equal(S.authorFromLink("https://www.artstation.com/artwork/abc", ""), null);
  assert.equal(S.authorFromLink("https://www.deviantart.com/someone", "").author, "someone");
  assert.equal(S.authorFromLink("https://bsky.app/profile/drawer.bsky.social", "").author, "drawer");
  assert.equal(S.authorFromLink("https://www.pinterest.com/pin/1/", ""), null);
  assert.equal(S.authorFromLink("https://www.google.com/search?q=x", ""), null);
});

test("highlighted names become tags", () => {
  assert.equal(S.handleFromText("by @Antin_Illust"), "Antin_Illust");
  assert.equal(S.handleFromText("  Kai Zin "), "Kai-Zin");
  assert.equal(S.handleFromText("Ilya (illustrator)!"), "Ilya-illustrator");
});

test("keys are named by where they are, with what is held", () => {
  assert.equal(S.keyCombo({ code: "KeyD", altKey: true, shiftKey: true }), "Alt+Shift+D");
  assert.equal(S.keyCombo({ code: "Digit7", ctrlKey: true }), "Ctrl+7");
  assert.equal(S.keyCombo({ code: "Numpad3" }), "Num3");
  assert.equal(S.keyCombo({ code: "F2", metaKey: true }), "Meta+F2");
  assert.equal(S.keyCombo({ code: "ShiftLeft", shiftKey: true }), "");
  assert.equal(S.keyCombo({ code: "AltRight", altKey: true }), "");
  assert.equal(S.keyCombo({}), "");
});

test("keys the browser keeps are refused, plain ones come with a warning", () => {
  assert.equal(S.keyCheck("Ctrl+W").ok, false);
  assert.equal(S.keyCheck("Escape").ok, false);
  assert.deepEqual(S.keyCheck("Alt+Shift+D"), { ok: true, note: "" });
  const plain = S.keyCheck("S");
  assert.equal(plain.ok, true);
  assert.match(plain.note, /typing/);
  assert.equal(S.keyCheck("Shift+S").note !== "", true);
  assert.deepEqual(S.keyCheck(""), { ok: true, note: "" });
});

test("every action has a default (maybe none) and the defaults don't clash", () => {
  const d = S.defaultKeys();
  assert.equal(Object.keys(d).length, S.KEY_ACTIONS.length);
  const bound = Object.values(d).filter(Boolean);
  assert.equal(new Set(bound).size, bound.length);
  for (const k of bound) assert.equal(S.keyCheck(k).ok, true, k);
});

test("file names from addresses", () => {
  assert.equal(S.nameFromUrl("https://pbs.twimg.com/media/GxY12ab.jpg:orig"), "GxY12ab.jpg");
  assert.equal(S.nameFromUrl("https://e.com/a/My%20Pic.png?x=1"), "My Pic.png");
  assert.equal(S.nameFromUrl("blob:https://e.com/123"), "");
  assert.equal(S.nameFromUrl("https://e.com/"), "");
});
