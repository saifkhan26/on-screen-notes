"use strict";
const api = globalThis.browser ?? globalThis.chrome;
const DEFAULT_PORT = 47823;
const ALL = { origins: ["<all_urls>"] };
const $ = (id) => document.getElementById(id);

async function refresh() {
  $("state").textContent = "Checking…";
  const s = await api.runtime.sendMessage({ type: "osn-status" }).catch(() => ({ running: false }));
  if (!s.running) $("state").textContent = "On-Screen Notes isn't answering — is it running, with “Receive from the browser” on?";
  else if (!s.paired) $("state").textContent = "On-Screen Notes is running, but this browser isn't connected yet — press Connect.";
  else $("state").textContent = `Connected to On-Screen Notes ${s.version}.`;
  const has = await api.permissions.contains(ALL).catch(() => true);
  $("access").hidden = has;
}

$("check").addEventListener("click", refresh);

$("connect").addEventListener("click", async () => {
  $("state").textContent = "Waiting — press Connect in On-Screen Notes…";
  const r = await api.runtime.sendMessage({ type: "osn-connect" }).catch((e) => ({ ok: false, error: String(e) }));
  if (r.ok) refresh();
  else $("state").textContent = r.error;
});

$("grant").addEventListener("click", async () => {
  await api.permissions.request(ALL).catch(() => false);
  refresh();
});

$("save").addEventListener("click", async () => {
  const port = Math.round(Number($("port").value));
  if (!(port >= 1024 && port <= 65535)) {
    $("state").textContent = "A port is a number from 1024 to 65535.";
    return;
  }
  await api.storage.local.set({ port });
  refresh();
});

// ---- Keys -----------------------------------------------------------

const S = globalThis.OSNSites;
let keys = S.defaultKeys();
let recording = null; // the action id whose key is being pressed

async function loadKeys() {
  const s = await api.storage.local.get({ keys: null });
  keys = Object.assign(S.defaultKeys(), s.keys || {});
}

async function saveKeys() {
  await api.storage.local.set({ keys });
}

function drawKeys() {
  const table = $("keys");
  table.textContent = "";
  for (const a of S.KEY_ACTIONS) {
    const tr = document.createElement("tr");
    const name = document.createElement("td");
    name.textContent = a.label;
    const cell = document.createElement("td");
    cell.className = "key";
    const b = document.createElement("button");
    b.className = "key";
    if (recording === a.id) {
      b.classList.add("rec");
      b.textContent = "Press keys…";
    } else if (keys[a.id]) {
      b.textContent = keys[a.id];
    } else {
      b.classList.add("none");
      b.textContent = "none";
    }
    b.addEventListener("click", () => {
      recording = recording === a.id ? null : a.id;
      $("keys-note").textContent = recording ? `Press the keys for “${a.label}”` : "";
      drawKeys();
    });
    cell.appendChild(b);
    tr.append(name, cell);
    table.appendChild(tr);
  }
}

document.addEventListener(
  "keydown",
  async (e) => {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    const id = recording;
    const label = S.KEY_ACTIONS.find((a) => a.id === id).label;
    if (e.code === "Escape") {
      recording = null;
      $("keys-note").textContent = "";
      drawKeys();
      return;
    }
    if (e.code === "Backspace" || e.code === "Delete") {
      keys[id] = "";
      recording = null;
      await saveKeys();
      $("keys-note").textContent = `“${label}” has no key now`;
      drawKeys();
      return;
    }
    const combo = S.keyCombo(e);
    if (!combo) return; // a modifier on its own: wait for the key
    const check = S.keyCheck(combo);
    if (!check.ok) {
      $("keys-note").textContent = check.note;
      return;
    }
    // A key does one thing: the action that had it lets it go.
    const had = S.KEY_ACTIONS.find((a) => a.id !== id && keys[a.id] === combo);
    if (had) keys[had.id] = "";
    keys[id] = combo;
    recording = null;
    await saveKeys();
    const notes = [];
    if (had) notes.push(`Taken from “${had.label}”, which has no key now.`);
    if (check.note) notes.push(check.note + ".");
    $("keys-note").textContent = notes.join(" ");
    drawKeys();
  },
  true,
);

$("keys-reset").addEventListener("click", async () => {
  keys = S.defaultKeys();
  recording = null;
  await saveKeys();
  $("keys-note").textContent = "Back to the defaults";
  drawKeys();
});

$("shortcuts").addEventListener("click", async () => {
  const r = await api.runtime.sendMessage({ type: "osn-open-shortcuts" }).catch(() => null);
  if (!r || !r.ok) $("keys-note").textContent = `Open ${r && r.where ? r.where : "the browser's extension shortcuts"}.`;
});

(async () => {
  const s = await api.storage.local.get({ port: DEFAULT_PORT });
  $("port").value = s.port;
  await loadKeys();
  drawKeys();
  refresh();
})();
