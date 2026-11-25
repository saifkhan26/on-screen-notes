# Send to On-Screen Notes — browser extension

Sends pictures, GIFs and videos from Firefox or Chrome straight onto the slide
you have open in On-Screen Notes, with no download step. One codebase for both
browsers (Manifest V3).

- **Right-click** any picture, GIF or video → **Send to On-Screen Notes**. It
  works through the covers sites lay over their pictures (Instagram, Pinterest,
  X): the extension looks at everything under the click, not only the top layer.
- **Pick**: `Alt+Shift+S`, the toolbar button, or **Pick media to send…** in the
  right-click menu. Hover to see what would be sent (and whether it is **on a
  slide already**), the mouse wheel steps to the picture beneath, click sends,
  `Shift`+click picks several and `Enter` sends them, `Ctrl`+click sends a
  video's frame, `Alt`+click records a video as it plays, `Esc` stops.
- **Send what's under the pointer**: `Alt+Shift+D` — no menu, no picker.
- **All media on this page**: `Alt+Shift+A` or the right-click menu — every
  picture, GIF and video on the page as a grid; tick some (or *Select all*, or
  *Select ones not on a slide*) and send them. Tiles already on a slide say
  which. Scroll the page to load more first (Pinterest boards, profiles).
- **Send all pictures in this post**: an X post's pictures, an Instagram
  carousel (stepped through), a Bluesky or Tumblr post's.
- **Send this video frame**: the frame a paused (or playing) video shows.
- **Look in my slides**: the app's find opens with pictures like this one.
- **Send to** (right-click menu): the open slide, or one of the last visited —
  everything goes there until you change it back.
- **Tags for what you send…** (right-click menu): type some tags once; everything
  you send gets them until you clear them.
- **Who posted it**: on X, Instagram, Bluesky, DeviantArt, Tumblr and
  ArtStation (artist subdomains) the poster's handle goes with the picture; on a
  Pinterest pin, the artist of the page the pin links to. The app only *offers*
  it as a tag (`+#handle` beside the tag button); nothing is tagged by itself.
  Right-click the offer if the poster is not the artist.
- **Find the artist, then send…** (right-click a picture): it opens on Google Lens
  in a new tab; pick the artist on the bar there and the picture is sent tagged
  with them (or *Send without an artist*, or *Cancel*).
- The biggest version is fetched where the site has one (X `name=orig`,
  Pinterest `/originals/`, ArtStation `4k`, the largest `srcset` entry). Picture
  types the app can't read (AVIF, SVG…) are turned into PNG first. A picture
  whose file the site refuses is sent as the screen shows it.

- **Videos that only stream** (`blob:` addresses — Instagram, X and most
  players) come in two ways:
  - **Downloaded whole** from their post by the app with
    [yt-dlp](https://github.com/yt-dlp/yt-dlp), when it is installed. The app
    offers to install it (with WinGet) the first time a stream video comes
    without it. The site's cookies go to yt-dlp for that one download, so posts
    you can see while logged in work; the app deletes them right after. A
    carousel post brings up to 6 videos.
  - **Recorded as they play**, when there is no yt-dlp or no post to download
    from (a feed with no post link): the video restarts and is recorded to its
    end (3 minutes at most; `Esc` stops early), without sound. Keep the tab in
    view while it records.
  - **Record video as it plays** in the right-click menu, or `Alt`+click in the
    picker, records whatever the video is.
- When a download fails, the app updates yt-dlp (once a run) and tries again.
  By hand: `winget upgrade yt-dlp.yt-dlp`.
- **Find artist** (for pictures already on your slides): select them in the
  app and press the arrange bar's *Find artist* button (or *Find the artist* in
  the command palette). The app opens a page in your default browser, which the
  extension takes over:
  1. It puts the first picture into Google Lens's own upload box (tried as a
     file, then a paste, then a drop; if Lens takes none, the bar shows the
     picture to drag onto the page or click to copy).
  2. Over the results, a bar lists the artists their links point at
     (`@handle · X ×3`): click who made it. Or right-click any link or a
     highlighted name → **Tag as the artist → On-Screen Notes**, on the
     results or on a page you opened from them. **Same as last: @handle**
     (beside Skip) tags it with the artist you tagged last — often the next
     picture's artist too.
  3. The app tags the picture straight away (undo takes it off) — and its
     copies on other slides — and the tab moves on to the next picture.
     **Skip** and **Stop** are on the bar; the app rings the picture being
     looked up, shows how far it got, and can stop it too.

  Keys on the bar: `1`–`6` pick a chip, `S` is *Same as last*; on Google's pages
  `Space` skips and `Esc` stops. The bar follows the lookup: it comes back on
  every page the lookup tab goes to (an X post opened from the results gets a
  chip for its own author first), and on tabs opened from it.

  Find artist passes over pictures that already have an **artist's tag** (unless
  every selected one has: then they are looked up again), and a picture whose
  copy elsewhere has one gets that tag without a lookup. Which tags are artists:
  every tag you had when this first ran, plus handles picked on Lens or taken
  from posts; tags given while sending (`pose`, `hands`) are not. Find's tag
  list (Ctrl+F, nothing typed) has an **artist** switch on each tag to fix one.

  All of the web side (Google's pages, reading links) is the extension's; the
  app only keeps the list and takes the answers. When Google changes its Lens
  page, the extension is what needs fixing.

## How it talks to the app

The app listens on `http://127.0.0.1:47823` (this computer only; the port is
`browser_link_port` in the app's config, and the extension's options). The first
time you send something, the app asks **Connect the browser extension?** — press
**Connect**, and the extension keeps the token it is given. Web pages can't use
the port: every request needs a header that makes a page's browser ask first,
and the app never says yes to that. Turn it off with **Settings → Browser
extension → Receive from the browser**.

## Installing

### Firefox

Firefox only keeps extensions that Mozilla has signed.

- **To try it now**: open `about:debugging#/runtime/this-firefox` → **Load
  Temporary Add-on…** → pick `manifest.json` in this folder. It stays until
  Firefox closes.
- **To keep it**: sign it for yourself (free, not listed in the store). Once: make
  an account at addons.mozilla.org, create API keys under *Tools → Manage API
  Keys*, and keep them in your environment (then open a new terminal):

  ```powershell
  setx AMO_JWT_ISSUER "user:…"
  ```

  ```powershell
  setx AMO_JWT_SECRET "…"
  ```

  Then, for each new version, one command in this folder: it bumps the version,
  has Mozilla sign it (usually minutes), and opens the signed `.xpi` in Firefox
  to install:

  ```powershell
  .\sign-firefox.ps1
  ```

### Chrome, Edge, Brave

`chrome://extensions` → turn on **Developer mode** → **Load unpacked** → pick this
folder. It stays installed.

### After installing

Tabs that were open before need a reload before right-click finds what is under
the cursor (the picker works in them anyway).

### Keys

Set them in the extension's options (`about:addons` → the extension →
*Preferences* in Firefox; *Extension options* in Chrome): every action has a
row — click it and press the new keys (`Esc` keeps the old one, `Backspace`
takes it off). A key moved from one action to another leaves the first without
one, and keys the browser keeps for itself (`Ctrl+W`, `Ctrl+T`…) are refused.
Defaults: `Alt+Shift+D` send what's under the pointer, `Alt+Shift+S` the picker,
`Alt+Shift+A` all media on the page, `Alt+Shift+T` *Tag as the artist* while Find
artist looks a picture up (the name you highlighted, else the link under the
pointer); the rest have none.

These work on web pages. Every action is also a *browser shortcut* (none set),
which works even while the address bar has the keyboard: the options' *Browser
shortcuts…* button opens where they are set (`about:addons` → ⚙ → *Manage
Extension Shortcuts* in Firefox, `chrome://extensions/shortcuts` in Chrome).

## Tests

```powershell
node --test browser-extension/tests/sites.test.js
```
