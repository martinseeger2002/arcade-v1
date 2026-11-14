// Shared by the main window (index.html) and app windows (appwindow.html); loaded first.
// Each tab's script is an IIFE and uses these globals; app.js is a plain script beside them.
const $ = (id) => document.getElementById(id);
// In the desktop app the window's calls go to the app; in a browser (arcaded serves these same
// files at /ui/, docs/WEBUI.md) the same four calls go to arcaded over HTTP.
const invoke = window.__TAURI__ ? window.__TAURI__.core.invoke : webInvoke;
// In a browser: notes meant only there (class "web-only") show; in the app they stay hidden.
const IN_BROWSER = !window.__TAURI__;
document.addEventListener("DOMContentLoaded", () => document.querySelectorAll(".web-only").forEach((e) => { e.hidden = !IN_BROWSER; }));

// The key from `arcade web`'s link (#k=...), kept for this browser; it changes when arcaded restarts.
function webKey(ask) {
  const m = location.hash.match(/[#&]k=([0-9a-f]{16,128})/);
  if (m) {
    try { localStorage.setItem("arcade.uikey", m[1]); } catch {}
    history.replaceState(null, "", location.pathname + location.search);
    return m[1];
  }
  let k = "";
  try { k = localStorage.getItem("arcade.uikey") || ""; } catch {}
  if (!k || ask) {
    const got = (prompt("This arcade is locked. Paste its link or key (run: arcade web)") || "").trim();
    const g = got.match(/([0-9a-f]{16,128})\s*$/);
    if (g) { k = g[1]; try { localStorage.setItem("arcade.uikey", k); } catch {} }
  }
  return k;
}

const WEB_WINDOWS = {};
async function webInvoke(name, args = {}) {
  if (name === "api") {
    // arcaded's JSON-RPC with the cookie: the same commands the app sends over the socket.
    const { cmd, ...params } = args.req || {};
    for (let tries = 0; tries < 2; tries++) {
      let r;
      try {
        r = await fetch("/rpc", { method: "POST", headers: { "Content-Type": "application/json", "Authorization": "Basic " + btoa("__cookie__:" + webKey(tries > 0)) }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: cmd, params }) });
      } catch (e) {
        return { ok: false, error: "the arcade can't be reached (is it running, and the tunnel open?)" };
      }
      if (r.status === 401 && tries === 0) continue;
      if (r.status === 401) return { ok: false, error: "locked: open the link from `arcade web` (the key changes when the arcade restarts)" };
      let j;
      try { j = await r.json(); } catch { return { ok: false, error: `the arcade answered ${r.status}` }; }
      return j.error ? { ok: false, error: j.error.message || String(j.error) } : { ok: true, result: j.result };
    }
  }
  if (name === "open_app") {
    if (!String(args.frame || "").startsWith("http://127.0.0.1:")) throw "apps run only from this arcade";
    let url = `appwindow.html?frame=${encodeURIComponent(args.frame)}&id=${encodeURIComponent(args.id)}&title=${encodeURIComponent(args.title)}`;
    if (args.ring && /^[0-9a-f]{16}$/.test(args.ring)) url += `&ring=${args.ring}`;
    const label = "app-" + String(args.id).replace(/[^A-Za-z0-9_-]/g, "_");
    const w = WEB_WINDOWS[label];
    if (w && !w.closed && !args.ring) { w.focus(); return; }
    WEB_WINDOWS[label] = window.open(url, label, "popup,width=1000,height=720");
    if (!WEB_WINDOWS[label]) throw "the browser blocked the app's window: allow pop-ups for this page";
    return;
  }
  if (name === "open_content") {
    const ok = /^http:\/\/127\.0\.0\.1:\d+\/(content\/[A-Za-z0-9]+|(app|ord)\/[a-z]+\/[A-Za-z0-9]+)$/.test(String(args.url || ""));
    if (!ok) throw "only content from this arcade opens here";
    window.open(args.url, "_blank", "noopener,popup,width=900,height=900");
    return;
  }
  if (name === "save_file") {
    const bin = atob(args.data || "");
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([bytes]));
    a.download = String(args.name || "file").replace(/[\/\\]/g, "_");
    document.body.append(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 60000);
    return a.download;
  }
  throw `unknown call ${name}`;
}

// Everything goes through arcaded's local API (one JSON request each).
async function api(cmd, args = {}) {
  const r = await invoke("api", { req: { cmd, ...args } });
  if (!r.ok) throw new Error(r.error || "failed");
  return r.result;
}

// Element builder: h("div", { class: "x", onclick: f }, kids...). Attributes that are
// undefined, null or false are left out; strings and numbers become text nodes.
const h = (tag, attrs = {}, ...kids) => {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === "class") e.className = v; else if (k.startsWith("on")) e[k] = v; else e.setAttribute(k, v === true ? "" : v);
  }
  for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false) e.append(k.nodeType ? k : document.createTextNode(String(k)));
  return e;
};

// A tab's status line (element `id`): green, or red when `bad`.
function say(id, text, bad) { const m = $(id); m.textContent = text; m.className = "msg " + (bad ? "bad" : "ok"); }

// What people call each coin (the keys underneath stay as the mesh knows them).
const TICKERS = { peptest: "PEPtest", pep: "PEP", doge: "DOGE", dgb: "DGB", dgbtest: "DGBtest", kot: "KOT", flop: "FLOP", ltc: "LTC", ltctest: "LTCtest", btc: "BTC", wjk: "WJK", bel: "BEL", lky: "LKY", shic: "SHIC", jkc: "JKC", arc: "ARCADE", arctest: "ARCADEtest" };
// A chain's colour, behind its letter when it has no icon.
const COLORS = { peptest: "#2e8b44", pep: "#3f9c55", doge: "#c2a633", dgb: "#0066cc", dgbtest: "#3388dd", kot: "#7a4cc2", flop: "#d0602a", ltc: "#345d9d", ltctest: "#5b7fb8", btc: "#f7931a", shic: "#e0812b", bel: "#c9a227", lky: "#2f9e5b", jkc: "#8a6d3b", arc: "#b0228c", arctest: "#c25aa6" };
// The original Dogecoin Arcade's coin icons (coins/<key>.png); others get a letter.
const COIN_ICONS = ["peptest", "pep", "doge", "dgb", "dgbtest", "kot", "flop", "ltc", "ltctest", "btc", "wjk", "arc", "arctest", "bel", "lky", "shic", "jkc"];
const ICON_FILE = (k) => `coins/${k}.${k === "btc" ? "svg" : "png"}`;
// A chain's coin as an element (`tag`.`cls`): its icon, or its first letter on its colour.
function coinLogo(key, name, tag, cls) {
  const e = document.createElement(tag); e.className = cls;
  if (COIN_ICONS.includes(key)) { const i = document.createElement("img"); i.src = ICON_FILE(key); i.alt = ""; e.append(i); e.classList.add("img"); }
  else { e.style.background = COLORS[key] || "#777"; e.textContent = (name || key)[0]; }
  return e;
}
// Satoshis as coins, up to 8 decimals, in `locale` (undefined: the system's).
const coinNum = (sats, locale) => (sats / 1e8).toLocaleString(locale, { maximumFractionDigits: 8 });

// The home chain (chains.list, "home"): fees, tips and posts are paid in its coin. Until the
// list is in, Pepecoin testnet (the default home).
const HOME = { key: "peptest", name: "Pepecoin testnet", known: false };
const homeTicker = () => TICKERS[HOME.key] || HOME.key;
const homeCoins = (sats) => coinNum(sats) + " " + homeTicker();
async function loadHome() {
  if (HOME.known) return HOME;
  try {
    const r = await invoke("api", { req: { cmd: "chains.list" } });
    const c = r.ok && (r.result || []).find((x) => x.home);
    if (c) {
      Object.assign(HOME, { key: c.key, name: c.name, known: true });
      document.querySelectorAll(".home-ticker").forEach((e) => { e.textContent = homeTicker(); });
      window.dispatchEvent(new CustomEvent("arcade:home", { detail: HOME }));
    }
  } catch {}
  return HOME;
}
loadHome();

// "/content/<id>" in a feed post or a message (the original arcade's links): the inscription itself, from the
// home chain's arcade ledger, else its ord index. Looked up once per id.
const CONTENT_CACHE = new Map();
const CONTENT_RE = /\/content\/([0-9a-f]{64}(?:i\d+)?)/gi;
function contentOf(id) {
  if (!CONTENT_CACHE.has(id)) {
    CONTENT_CACHE.set(id, api("explorer.ledger_inscription", { chain: HOME.key, id })
      .catch(() => api("explorer.inscription", { chain: HOME.key, id }))
      .then((r) => r.content || null).catch(() => null));
  }
  return CONTENT_CACHE.get(id);
}
function contentEmbeds(text) {
  const ids = [...new Set([...(text || "").matchAll(CONTENT_RE)].map((m) => m[1].toLowerCase()))].slice(0, 4);
  return ids.map((id) => {
    const box = h("div", { class: "fd-file fd-embed" });
    contentOf(id).then((c) => {
      if (!c) return;
      if (c.kind === "image" && c.data) box.append(h("img", { src: c.data, alt: "inscription " + id.slice(0, 12), loading: "lazy" }));
      else if (c.kind === "text" && c.text) box.append(h("pre", { class: "fd-embed-text" }, c.text.slice(0, 2000)));
      else box.append(h("span", { class: "s" }, `Inscription ${id.slice(0, 12)}… (${c.type || "file"})`));
    });
    return box;
  });
}

