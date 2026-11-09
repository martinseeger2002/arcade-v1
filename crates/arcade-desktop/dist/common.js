// Shared by the main window (index.html) and app windows (appwindow.html); loaded first.
// Each tab's script is an IIFE and uses these globals; app.js is a plain script beside them.
const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__.core.invoke;

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
const TICKERS = { peptest: "PEPtest", pep: "PEP", doge: "DOGE", dgb: "DGB", dgbtest: "DGBtest", kot: "KOT", flop: "FLOP", ltc: "LTC", ltctest: "LTCtest", btc: "BTC", wjk: "WJK" };
// A chain's colour, behind its letter when it has no icon.
const COLORS = { peptest: "#2e8b44", pep: "#3f9c55", doge: "#c2a633", dgb: "#0066cc", dgbtest: "#3388dd", kot: "#7a4cc2", flop: "#d0602a", ltc: "#345d9d", ltctest: "#5b7fb8", btc: "#f7931a" };
// The original Dogecoin Arcade's coin icons (coins/<key>.png); others get a letter.
const COIN_ICONS = ["peptest", "pep", "doge", "dgb", "dgbtest", "kot", "flop", "ltc", "ltctest", "btc", "wjk"];
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
