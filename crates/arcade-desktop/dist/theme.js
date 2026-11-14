// Themes: apps from the App Store's "themes" category that change how this window looks and is laid out,
// never what it does. Every screen (Wallet, Messages, Apps ...) is the window's own; a theme only chooses
//
//   - a frame around them: "window" (a menu bar, a toolbar of the screens, a status bar: an application window)
//     or "desktop" (each screen a window on a desktop, with a taskbar, a dock, a start menu, a clock),
//   - the screens' colours, font, text size and corner roundness: values, each checked (text must stand out from
//     its backgrounds, as the original arcade's themes, packages.py CONTRAST),
//   - the frame's own style: CSS drawn only inside the frame (a shadow root), which can't reach a screen.
//     Selectors may name the frame's parts only, and properties are from a short list (colours, borders, shadows,
//     fonts, spacing; no position, size, display, opacity, transform or content), so a theme can't hide,
//     cover or rewrite what a screen shows: an address, an amount, a send confirmation.
//
// A theme app is an ordinary app inscription (text/html: a preview page, sandboxed like any app when opened)
// whose card says "category": "themes" and whose page carries its theme as
//   <script type="application/arcade-theme+json">{ "arcade_theme": 1, ... }</script>
// (docs/THEMES.md). The choice is kept in this window (localStorage): the desktop app and each browser choose
// for themselves. A theme follows its publisher's updates, as an installed app does.
"use strict";

const THEME = (() => {
  const KEY = "arcade.theme";            // {id, chain, txid, name}
  const CACHE = "arcade.theme.cache";    // {txid, manifest}: drawn at once on the next start
  const DESK = "arcade.theme.desk.";     // + theme id: windows' places
  const TABS = [
    ["home", "Home"], ["wallet", "Wallet"], ["messages", "Messages"], ["feed", "Feed"], ["apps", "Apps"],
    ["explorer", "Explorer"], ["chains", "Chains & Coins"], ["mine", "Mine"], ["activity", "Activity"],
    ["apitab", "API"], ["options", "Options"],
  ];
  const COLOURS = ["bg", "panel", "panel-2", "ink", "muted", "line", "accent", "accent-ink", "accent-soft",
    "warn-bg", "warn-line", "bad", "faint"];
  // Pairs that must read: [text, background, least contrast].
  const CONTRAST = [["ink", "bg", 4.5], ["ink", "panel", 4.5], ["accent-ink", "accent", 4.5],
    ["muted", "panel", 3], ["bad", "panel", 3], ["ink", "warn-bg", 4.5]];
  const FONTS = {
    system: 'system-ui, -apple-system, "Segoe UI", Ubuntu, sans-serif',
    tahoma: 'Tahoma, "Microsoft Sans Serif", "MS Sans Serif", Geneva, Verdana, sans-serif',
    ubuntu: 'Ubuntu, Cantarell, "Noto Sans", "DejaVu Sans", system-ui, sans-serif',
    mac: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", Helvetica, Arial, sans-serif',
    serif: 'Georgia, "Times New Roman", serif',
    mono: 'ui-monospace, "DejaVu Sans Mono", Menlo, Consolas, monospace',
  };
  // The frame's parts: the only classes a theme's CSS may name.
  const PARTS = new Set(["frame", "layout-window", "layout-desktop", "menubar", "mbitem", "menu", "mitem", "msep",
    "check", "toolbar", "tbitem", "ticon", "statusbar", "sync", "links", "brand", "wbody", "desk", "icons", "icon",
    "icon-img", "icon-label", "win", "active", "max", "titlebar", "title", "ticn", "tbtns", "tbtn", "tmin", "tmax",
    "tclose", "bar", "bar-top", "bar-bottom", "start", "tasks", "task", "on", "tray", "clock", "dock", "dock-left",
    "dock-bottom", "ditem", "open", "startmenu", "smhead", "smitem", "topbar", "chain", "conn", "selected"]);
  const SMALL = new Set(["titlebar", "tbtn", "tmin", "tmax", "tclose", "bar", "start", "task", "tray", "clock",
    "menubar", "mbitem", "mitem", "toolbar", "tbitem", "ticon", "statusbar", "dock", "ditem", "icon", "icon-img",
    "ticn", "topbar", "smhead", "smitem", "brand", "msep"]);
  const PROPS = new Set(["color", "background", "background-color", "background-image", "background-size",
    "background-position", "background-repeat", "border", "border-top", "border-right", "border-bottom",
    "border-left", "border-color", "border-width", "border-style", "border-top-color", "border-right-color",
    "border-bottom-color", "border-left-color", "border-top-width", "border-right-width", "border-bottom-width",
    "border-left-width", "border-radius", "border-top-left-radius", "border-top-right-radius",
    "border-bottom-left-radius", "border-bottom-right-radius", "box-shadow", "text-shadow", "font-family",
    "font-size", "font-weight", "font-style", "letter-spacing", "text-transform", "text-align", "padding",
    "padding-top", "padding-right", "padding-bottom", "padding-left", "margin", "margin-top", "margin-right",
    "margin-bottom", "margin-left", "gap", "outline", "outline-offset", "cursor", "image-rendering", "line-height",
    "justify-content", "align-items", "backdrop-filter", "width", "height", "min-width", "min-height",
    "max-width", "max-height", "transition"]);
  const SIZES = new Set(["width", "height", "min-width", "min-height", "max-width", "max-height"]);
  const WALL = /^url\(\s*["']?data:image\/(png|gif|webp|jpeg|svg\+xml);base64,[A-Za-z0-9+/=]+["']?\s*\)$/;

  // --- checking a theme ------------------------------------------------------------------------------------------
  const hexOk = (v) => typeof v === "string" && /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(v);
  function lum(hex) {
    let x = hex.slice(1); if (x.length === 3) x = [...x].map((c) => c + c).join("");
    const [r, g, b] = [0, 2, 4].map((i) => parseInt(x.slice(i, i + 2), 16) / 255).map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  }
  const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

  function colours(set, what) {
    if (!set || typeof set !== "object") throw new Error(`${what}: no colours`);
    const out = {};
    for (const k of COLOURS) {
      if (set[k] === undefined) continue;
      if (!hexOk(set[k])) throw new Error(`${what}.${k}: a colour is #rgb or #rrggbb`);
      out[k] = set[k];
    }
    for (const k of ["bg", "panel", "ink", "muted", "line", "accent", "accent-ink"]) if (!out[k]) throw new Error(`${what}: ${k} is missing`);
    out["panel-2"] ||= out.panel; out["accent-soft"] ||= out.panel; out["warn-bg"] ||= out.panel;
    out["warn-line"] ||= out.line; out.bad ||= out.ink; out.faint ||= out.muted;
    for (const [t, b, least] of CONTRAST) {
      const r = ratio(out[t], out[b]);
      if (r < least) throw new Error(`${what}: ${t} on ${b} is ${r.toFixed(1)}:1, less than ${least}:1, so it couldn't be read`);
    }
    return out;
  }

  function lengthsOk(prop, value) {
    for (const m of value.matchAll(/(-?\d*\.?\d+)(px|em|rem|%|vh|vw|vmin|vmax|ch|ex|deg|s|ms)?/g)) {
      const n = Math.abs(parseFloat(m[1])); const u = m[2] || "";
      if (["vh", "vw", "vmin", "vmax"].includes(u)) return false;
      if ((u === "em" || u === "rem" || u === "ch" || u === "ex") && n > 4) return false;
      if (u === "px" && !prop.startsWith("background") && n > (SIZES.has(prop) ? 240 : 64)) return false;
      if (prop === "letter-spacing" && (u === "px" ? n > 4 : n > 0.3)) return false;
    }
    return true;
  }

  function selectorOk(sel) {
    return sel.split(",").every((one) => {
      const s = one.trim();
      if (!s || !/^[a-z0-9.:()\s>-]+$/i.test(s)) return false;
      const compounds = s.split(/\s*>\s*|\s+/).filter(Boolean);
      return compounds.every((c) => /^(\.[a-z][a-z0-9-]*)+(:(hover|active|focus-visible|first-child|last-child)|:not\(\.[a-z][a-z0-9-]*\))*$/.test(c)
        && [...c.matchAll(/\.([a-z][a-z0-9-]*)/g)].every((m) => PARTS.has(m[1])));
    });
  }
  const lastParts = (sel) => sel.split(",").map((one) => { const cs = one.trim().split(/\s*>\s*|\s+/); return [...cs[cs.length - 1].matchAll(/\.([a-z][a-z0-9-]*)/g)].map((m) => m[1]); });

  function declsOk(sel, body) {
    const out = [];
    for (const d of body.split(";")) {
      if (!d.trim()) continue;
      const at = d.indexOf(":"); if (at < 1) throw new Error(`frame CSS: "${d.trim().slice(0, 40)}" isn't a property`);
      const prop = d.slice(0, at).trim().toLowerCase(); const value = d.slice(at + 1).trim();
      if (!PROPS.has(prop)) throw new Error(`frame CSS: ${prop} isn't one a theme may set`);
      if (/!important|expression|url\(|attr\(|image-set|element\(|@/i.test(value.replace(WALL_IN, ""))) throw new Error(`frame CSS: ${prop}: ${value.slice(0, 40)} isn't allowed`);
      if (/url\(/i.test(value) && !(prop === "background-image" || prop === "background")) throw new Error(`frame CSS: pictures only as a background`);
      if (SIZES.has(prop) && !lastParts(sel).every((ps) => ps.some((p) => SMALL.has(p)))) throw new Error(`frame CSS: ${prop} only on the frame's bars, buttons and icons`);
      if (prop === "backdrop-filter" && !/^(none|blur\(\s*\d{1,2}(\.\d+)?px\s*\)(\s+saturate\(\s*\d{1,3}%\s*\))?)$/.test(value)) throw new Error("frame CSS: backdrop-filter is blur(...) only");
      if (prop === "cursor" && !/^(auto|default|pointer|move)$/.test(value)) throw new Error("frame CSS: cursor is auto, default, pointer or move");
      if (prop.startsWith("margin") && /-/.test(value)) throw new Error("frame CSS: no negative margins");
      if (!lengthsOk(prop, value.replace(WALL_IN, ""))) throw new Error(`frame CSS: ${prop}: ${value.slice(0, 40)} is too large`);
      out.push(`${prop}:${value}`);
    }
    return out.join(";");
  }
  const WALL_IN = /url\(\s*["']?data:image\/(png|gif|webp|jpeg|svg\+xml);base64,[A-Za-z0-9+/=]+["']?\s*\)/g;

  function chromeCss(css) {
    if (typeof css !== "string") return "";
    if (css.length > 64000) throw new Error("frame CSS: more than 64 KB");
    css = css.replace(/\/\*[\s\S]*?\*\//g, "");
    if (/[\\<]|@import|@font-face|@namespace|@keyframes|javascript:/i.test(css)) throw new Error("frame CSS: escapes, @import, @font-face and the like aren't allowed");
    const out = []; let i = 0;
    const rules = (end, into) => {
      while (i < css.length) {
        while (i < css.length && /\s/.test(css[i])) i++;
        if (i >= css.length) break;
        if (css[i] === "}") { if (end) { i++; return; } throw new Error("frame CSS: a } too many"); }
        if (css.startsWith("@media", i)) {
          if (end) throw new Error("frame CSS: @media inside @media");
          const open = css.indexOf("{", i); if (open < 0) throw new Error("frame CSS: @media without {");
          const cond = css.slice(i, open).trim();
          if (!/^@media\s*\(\s*prefers-color-scheme\s*:\s*(dark|light)\s*\)$/.test(cond)) throw new Error(`frame CSS: only @media (prefers-color-scheme: dark|light), not ${cond.slice(0, 40)}`);
          i = open + 1; const inner = []; rules(true, inner); out.push(`${cond}{${inner.join("")}}`);
          continue;
        }
        const open = css.indexOf("{", i); if (open < 0) throw new Error("frame CSS: a rule without {");
        const sel = css.slice(i, open).trim(); const close = css.indexOf("}", open);
        if (close < 0) throw new Error("frame CSS: a rule without }");
        const body = css.slice(open + 1, close);
        if (body.includes("{")) throw new Error("frame CSS: rules can't nest");
        if (!selectorOk(sel)) throw new Error(`frame CSS: "${sel.slice(0, 50)}" names something other than the frame's parts`);
        into.push(`${sel}{${declsOk(sel, body)}}`);
        i = close + 1;
      }
      if (end) throw new Error("frame CSS: @media without }");
    };
    rules(false, out);
    return out.join("\n");
  }

  const pic = (v) => (typeof v === "string" && v.length < 12000 && /^data:image\/(png|gif|webp|svg\+xml);base64,[A-Za-z0-9+/=]+$/.test(v) ? v : "");
  const word = (v, n) => (typeof v === "string" ? v.replace(/\s+/g, " ").trim().slice(0, n) : "");

  /** The checked theme in a theme app's page, or an Error saying what is wrong with it. */
  function check(raw) {
    const t = typeof raw === "string" ? JSON.parse(raw) : raw;
    if (!t || t.arcade_theme !== 1) throw new Error('not a theme ("arcade_theme": 1 is missing)');
    const layout = t.layout === "desktop" ? "desktop" : t.layout === "window" ? "window" : null;
    if (!layout) throw new Error('layout is "window" or "desktop"');
    const light = colours(t.colors?.light, "colors.light");
    const dark = t.colors?.dark ? colours(t.colors.dark, "colors.dark") : light;
    const font = FONTS[t.font] ? t.font : "system";
    const size = Math.min(16, Math.max(11, Number(t.size) || 14));
    const radius = Math.min(16, Math.max(0, Math.round(Number(t.radius ?? 8))));
    const d = t.desktop || {};
    const desktop = {
      bar: ["bottom", "top", "none"].includes(d.bar) ? d.bar : "bottom",
      dock: ["none", "left", "bottom"].includes(d.dock) ? d.dock : "none",
      topbar: d.topbar === "menus" || d.topbar === "clock" ? d.topbar : "none",
      start: word(d.start ?? "Start", 16), icons: d.icons !== false, clock: d.clock !== false,
      buttons: d.buttons === "left" ? "left" : "right",
    };
    const icons = {};
    for (const k of [...TABS.map((x) => x[0]), "start"]) { const p = pic(t.icons?.[k]); if (p) icons[k] = p; }
    const brand = word(t.brand || "", 24);
    return { layout, light, dark, single: !t.colors?.dark, font, size, radius, desktop, icons, brand, window: { toolbar: t.window?.toolbar !== false },
      chrome: chromeCss(t.chrome || ""), name: word(t.name || "", 40) };
  }

  function fromPage(html) {
    const m = String(html || "").match(/<script[^>]*type=["']application\/arcade-theme\+json["'][^>]*>([\s\S]*?)<\/script>/i);
    if (!m) throw new Error("this app's page carries no theme");
    return check(m[1]);
  }

  // --- the screens' colours (the page itself) --------------------------------------------------------------------
  function screensCss(th) {
    const vars = (c) => COLOURS.map((k) => `--${k}:${c[k]}`).join(";");
    const r = th.radius;
    return `:root{${vars(th.light)};color-scheme:${th.single ? "light" : "light dark"}}
@media (prefers-color-scheme: dark){:root{${vars(th.dark)};color-scheme:${th.single ? "light" : "dark"}}}
body[data-themed]{font:${th.size}px/1.45 ${FONTS[th.font]};background:var(--bg);color:var(--ink);overflow:hidden}
body[data-themed]>header,body[data-themed]>nav,body[data-themed]>main{display:none}
body[data-themed] section.tab{color:var(--ink);font:${th.size}px/1.45 ${FONTS[th.font]};font-style:normal;letter-spacing:normal;
  text-shadow:none;text-transform:none;visibility:visible;cursor:auto;word-spacing:normal;white-space:normal;direction:ltr;
  text-align:left;background:var(--bg);padding:14px 16px 24px;margin:0;max-width:none;min-height:100%}
body[data-themed] :is(button,input,textarea,select,.card,.chain,.arcade,.warn,table,.ap-card,.ap-mine){border-radius:${r}px}
body[data-themed=desktop] section.tab{display:block}
body[data-themed] .chainsel .menu{z-index:50}
body[data-themed] .chainsel .menu{left:auto;right:0;max-height:70vh;overflow:auto}
body[data-themed][data-chain=bottom] .chainsel .menu{top:auto;bottom:calc(100% + 6px)}
body[data-themed] #conn{white-space:nowrap}`;
  }

  // --- the frame -------------------------------------------------------------------------------------------------
  const FRAME_CSS = `
:host{all:initial}
*{box-sizing:border-box}
[hidden]{display:none!important}
button{font:inherit;color:inherit;background:none;border:0;padding:0;cursor:default}
.frame{position:fixed;inset:0;display:flex;flex-direction:column;overflow:hidden;font-size:13px;line-height:1.3}
.menubar,.topbar{display:flex;align-items:center;flex:none;user-select:none;position:relative;z-index:40}
.mbitem{position:relative}
.mbitem>button{padding:4px 10px}
.menu{position:absolute;left:0;top:100%;min-width:200px;display:flex;flex-direction:column;z-index:60;padding:3px 0}
.bar-bottom .startmenu,.startmenu{position:absolute;z-index:70;min-width:220px;display:flex;flex-direction:column;padding:3px 0}
.mitem,.smitem{display:flex;align-items:center;gap:8px;text-align:left;padding:4px 16px;white-space:nowrap}
.msep{margin:3px 0;height:0}
.toolbar{display:flex;gap:2px;flex:none;overflow-x:auto;align-items:center}
.tbitem{display:flex;align-items:center;gap:6px;white-space:nowrap;padding:5px 10px}
.ticon,.ticn{width:18px;height:18px;flex:none;display:inline-flex}
.ticon svg,.ticn svg,.icon-img svg,.ditem svg{width:100%;height:100%}
.ticon img,.ticn img,.icon-img img,.ditem img{width:100%;height:100%;object-fit:contain}
.wbody{flex:1;overflow:auto;position:relative;min-height:0}
.layout-window>.wbody{display:block}
.statusbar{display:flex;justify-content:space-between;align-items:center;gap:12px;flex:none}
.middle{flex:1;display:flex;min-height:0;position:relative}
.desk{flex:1;position:relative;overflow:hidden}
.icons{position:absolute;inset:8px;display:flex;flex-direction:column;flex-wrap:wrap;align-content:flex-start;gap:6px;pointer-events:none}
.icon{width:76px;display:flex;flex-direction:column;align-items:center;gap:4px;padding:4px;pointer-events:auto;text-align:center}
.icon-img{width:32px;height:32px;display:flex}
.icon-label{font-size:inherit;overflow-wrap:anywhere}
.win{position:absolute;display:flex;flex-direction:column;min-width:260px;min-height:160px;resize:both;overflow:hidden}
.win.max{inset:0!important;width:auto!important;height:auto!important;resize:none}
.titlebar{display:flex;align-items:center;gap:6px;flex:none;user-select:none;touch-action:none}
.title{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tbtns{display:flex;gap:2px;flex:none}
.tbtn{display:inline-flex;align-items:center;justify-content:center;width:18px;height:16px;line-height:1}
.win>.wbody{flex:1}
.bar{display:flex;align-items:center;gap:4px;flex:none;position:relative;z-index:40}
.start{display:flex;align-items:center;gap:4px;white-space:nowrap}
.tasks{flex:1;display:flex;gap:3px;overflow:hidden;min-width:0}
.task{display:flex;align-items:center;gap:4px;min-width:0;max-width:170px;flex:0 1 170px;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}
.tray{display:flex;align-items:center;gap:8px;flex:none;white-space:nowrap}
.clock{white-space:nowrap}
.dock{display:flex;gap:6px;flex:none;align-items:center;z-index:45}
.dock-left{flex-direction:column;justify-content:flex-start}
.dock-bottom{justify-content:center}
.ditem{width:40px;height:40px;display:flex;align-items:center;justify-content:center}
.topbar .grow{flex:1}
.topbar .center{position:absolute;left:50%;transform:translateX(-50%)}
`;
  const ICONS = {
    home: "M3 11 12 4l9 7v9h-6v-6H9v6H3z", wallet: "M3 7h15a3 3 0 0 1 3 3v8H3zM3 7l12-3v3M16 13h2",
    messages: "M4 5h16v11H9l-5 4z", feed: "M5 5h14M5 10h14M5 15h9M5 20h6", apps: "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
    explorer: "M10 4a6 6 0 1 1 0 12 6 6 0 0 1 0-12zM15 15l5 5", chains: "M9 8h-3a4 4 0 0 0 0 8h3M15 8h3a4 4 0 0 1 0 8h-3M8 12h8",
    mine: "M5 20 15 10M13 4l7 7M11 6l7 7", activity: "M3 12h4l3-7 4 14 3-7h4", apitab: "M8 7l-5 5 5 5M16 7l5 5-5 5",
    options: "M12 8a4 4 0 1 1 0 8 4 4 0 0 1 0-8zM12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M5 19l2-2M17 7l2-2",
  };
  const svg = (k) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="${ICONS[k] || ICONS.apps}"/></svg>`;
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text !== undefined) e.textContent = text; return e; };
  const iconEl = (th, k, cls) => { const s = el("span", cls); if (th.icons[k]) { const i = document.createElement("img"); i.src = th.icons[k]; i.alt = ""; s.append(i); } else s.innerHTML = svg(k); return s; };
  const label = (k) => (TABS.find((t) => t[0] === k) || [k, k])[1];
  const go = (k) => { const b = document.querySelector(`nav button[data-tab="${k}"]`); if (b) b.click(); };

  let shell = null, root = null, current = null;

  function menus(th, onPick) {
    // ArcadeCore's menu bar, one for one: File, Settings, Window, Go, View (the looks).
    return [
      ["File", [["Backup (your 12 words)…", () => go("wallet")]]],
      ["Settings", [["Options…", () => go("options")], ["Theme…", () => { go("options"); setTimeout(() => document.getElementById("o-theme")?.scrollIntoView(), 50); }]]],
      ["Window", [["Activity", () => go("activity")], ["API", () => go("apitab")], ["Chains & Coins", () => go("chains")]]],
      ["Go", TABS.map(([k, l]) => [l, () => go(k)])],
      ["View", null],
    ].map(([name, items]) => {
      const m = el("div", "mbitem"); const b = el("button", "", name); const list = el("div", "menu"); list.hidden = true;
      const fill = async () => {
        list.replaceChildren();
        const rows = items || await viewItems();
        for (const r of rows) {
          if (!r) { list.append(el("div", "msep")); continue; }
          const i = el("button", "mitem" + (r[2] ? " check" : ""), (r[2] ? "✓ " : "") + r[0]);
          i.onclick = () => { closeMenus(); r[1](); onPick?.(); };
          list.append(i);
        }
      };
      b.onclick = async (e) => { e.stopPropagation(); const was = !list.hidden; closeMenus(); if (!was) { await fill(); list.hidden = false; m.classList.add("on"); } };
      b.onmouseenter = async () => { if (root.querySelector(".mbitem.on") && list.hidden) { closeMenus(); await fill(); list.hidden = false; m.classList.add("on"); } };
      m.append(b, list);
      return m;
    });
  }
  async function viewItems() {
    const rows = [["Default look", () => clear(), !current]];
    try {
      for (const a of await installedThemes()) rows.push([a.card.name, () => use(a), current?.id === a.id]);
    } catch {}
    return rows;
  }
  function closeMenus() {
    root?.querySelectorAll(".menu,.startmenu").forEach((x) => { x.hidden = true; });
    root?.querySelectorAll(".mbitem.on,.start.on").forEach((x) => x.classList.remove("on"));
  }

  function statusParts() {
    const sync = el("span", "sync"); const s = document.createElement("slot"); s.name = "conn"; sync.append(s);
    const chain = el("span", "chain"); const c = document.createElement("slot"); c.name = "chain"; chain.append(c);
    return [sync, chain];
  }
  function clockEl() {
    const c = el("span", "clock");
    const tick = () => { c.textContent = new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }); };
    tick(); setInterval(tick, 15000);
    return c;
  }

  function buildWindow(th, frame) {
    frame.classList.add("layout-window");
    document.body.dataset.chain = "bottom";        // the chain picker sits in the status bar: its menu opens upward
    const bar = el("div", "menubar");
    if (th.brand) bar.append(el("span", "brand", th.brand));
    bar.append(...menus(th));
    const tb = el("div", "toolbar");
    const items = TABS.map(([k, l]) => { const b = el("button", "tbitem"); b.dataset.tab = k; b.append(iconEl(th, k, "ticon"), el("span", "", l)); b.onclick = () => go(k); return b; });
    tb.append(...items);
    const body = el("div", "wbody"); const all = document.createElement("slot"); all.name = "screens"; body.append(all);
    const status = el("div", "statusbar"); status.append(...statusParts());
    frame.append(bar); if (th.window.toolbar) frame.append(tb); frame.append(body, status);
    const mark = () => { const on = document.querySelector("section.tab.on")?.id; items.forEach((b) => b.classList.toggle("selected", b.dataset.tab === on)); };
    new MutationObserver(mark).observe(document.body, { subtree: true, attributes: true, attributeFilter: ["class"] });
    mark();
    for (const s of document.querySelectorAll("section.tab")) s.slot = "screens";
  }

  function buildDesktop(th, frame, id) {
    const D = th.desktop;
    frame.classList.add("layout-desktop", "bar-" + D.bar);
    document.body.dataset.bar = D.bar;
    document.body.dataset.chain = D.topbar !== "none" || D.bar === "top" ? "top" : "bottom";
    const places = (() => { try { return JSON.parse(localStorage.getItem(DESK + id) || "{}"); } catch { return {}; } })();
    const save = () => { try { localStorage.setItem(DESK + id, JSON.stringify(places)); } catch {} };
    const desk = el("div", "desk"); const middle = el("div", "middle");
    const wins = {}; let z = 10;
    const tasks = el("div", "tasks");
    const taskOf = {}, dockOf = {};
    const front = (k) => {
      for (const [n, w] of Object.entries(wins)) { w.classList.toggle("active", n === k); taskOf[n]?.classList.toggle("on", n === k && !w.hidden); }
      if (wins[k]) wins[k].style.zIndex = String(++z);
    };
    const openWin = (k) => {
      const w = wins[k]; w.hidden = false; places[k] = { ...(places[k] || {}), open: true, min: false }; save();
      if (!taskOf[k]) {
        const t = el("button", "task"); t.append(iconEl(th, k, "ticn"), el("span", "", label(k)));
        t.onclick = () => { if (!w.hidden && w.classList.contains("active")) { w.hidden = true; places[k].min = true; save(); t.classList.remove("on"); } else { w.hidden = false; front(k); go(k); } };
        taskOf[k] = t; tasks.append(t);
      }
      dockOf[k]?.classList.add("open");
      front(k); go(k);
    };
    const closeWin = (k) => { wins[k].hidden = true; places[k] = { ...(places[k] || {}), open: false }; save(); taskOf[k]?.remove(); delete taskOf[k]; dockOf[k]?.classList.remove("open"); };
    let n = 0;
    for (const [k, l] of TABS) {
      const sec = document.getElementById(k); if (!sec) continue;
      sec.slot = "w-" + k;
      const w = el("div", "win"); w.hidden = true;
      const p = places[k] || {};
      const W = desk.clientWidth || innerWidth, Hh = desk.clientHeight || innerHeight;
      w.style.left = (p.x ?? (D.icons ? 110 : 40) + (n % 6) * 28) + "px"; w.style.top = (p.y ?? 30 + (n % 6) * 26) + "px";
      w.style.width = (p.w ?? Math.min(900, W - 120)) + "px"; w.style.height = (p.h ?? Math.min(640, Hh - 120)) + "px";
      if (p.max) w.classList.add("max");
      n++;
      const tb = el("div", "titlebar"); const btns = el("div", "tbtns");
      const mk = (cls, txt, f) => { const b = el("button", "tbtn " + cls, txt); b.onclick = (e) => { e.stopPropagation(); f(); }; return b; };
      const bMin = mk("tmin", "_", () => { w.hidden = true; places[k].min = true; save(); taskOf[k]?.classList.remove("on"); });
      const bMax = mk("tmax", "□", () => { w.classList.toggle("max"); places[k] = { ...(places[k] || {}), max: w.classList.contains("max") }; save(); });
      const bClose = mk("tclose", "×", () => closeWin(k));
      const order = [bMin, bMax, bClose];
      // a Mac's order (close first) when the buttons sit at the left
      btns.append(...(D.buttons === "left" ? [bClose, bMin, bMax] : order));
      if (D.buttons === "left") tb.append(btns, iconEl(th, k, "ticn"), el("span", "title", l));
      else tb.append(iconEl(th, k, "ticn"), el("span", "title", l), btns);
      const body = el("div", "wbody"); const s = document.createElement("slot"); s.name = "w-" + k; body.append(s);
      w.append(tb, body);
      w.addEventListener("pointerdown", () => { if (!w.classList.contains("active")) { front(k); go(k); } });
      tb.addEventListener("dblclick", () => btns.querySelector(".tmax").click());
      tb.addEventListener("pointerdown", (e) => {
        if (e.target.closest(".tbtn") || w.classList.contains("max")) return;
        const sx = e.clientX - w.offsetLeft, sy = e.clientY - w.offsetTop; tb.setPointerCapture(e.pointerId);
        const move = (m) => { w.style.left = Math.max(-200, m.clientX - sx) + "px"; w.style.top = Math.max(0, m.clientY - sy) + "px"; };
        const up = () => { tb.removeEventListener("pointermove", move); tb.removeEventListener("pointerup", up); places[k] = { ...(places[k] || {}), x: w.offsetLeft, y: w.offsetTop }; save(); };
        tb.addEventListener("pointermove", move); tb.addEventListener("pointerup", up);
      });
      new ResizeObserver(() => { if (!w.hidden && !w.classList.contains("max")) { places[k] = { ...(places[k] || {}), w: w.offsetWidth, h: w.offsetHeight }; save(); } }).observe(w);
      wins[k] = w; desk.append(w);
    }
    if (D.icons) {
      const icons = el("div", "icons");
      for (const [k, l] of TABS) { const i = el("button", "icon"); i.append(iconEl(th, k, "icon-img"), el("span", "icon-label", l)); i.ondblclick = () => openWin(k); i.onclick = (e) => { if (e.detail === 0) openWin(k); }; icons.append(i); }
      desk.prepend(icons);
    }
    // the start menu (or Activities): every screen, then the looks
    const sm = el("div", "startmenu"); sm.hidden = true;
    const fillStart = async () => {
      sm.replaceChildren(el("div", "smhead", th.brand || "Arcade"));
      for (const [k, l] of TABS) { const i = el("button", "smitem"); i.append(iconEl(th, k, "ticn"), el("span", "", l)); i.onclick = () => { closeMenus(); openWin(k); }; sm.append(i); }
      sm.append(el("div", "msep"));
      for (const r of await viewItems()) { const i = el("button", "smitem" + (r[2] ? " check" : ""), (r[2] ? "✓ " : "") + r[0]); i.onclick = () => { closeMenus(); r[1](); }; sm.append(i); }
    };
    const startBtn = (txt) => {
      const b = el("button", "start"); if (th.icons.start) { const i = document.createElement("img"); i.src = th.icons.start; i.alt = ""; i.className = "ticn"; b.append(i); }
      b.append(el("span", "", txt));
      b.onclick = async (e) => { e.stopPropagation(); const was = !sm.hidden; closeMenus(); if (!was) { await fillStart(); sm.hidden = false; b.classList.add("on");
        const r = b.getBoundingClientRect(), f = frame.getBoundingClientRect();
        sm.style.left = Math.max(0, r.left - f.left) + "px";
        if (r.top - f.top > f.height / 2) { sm.style.bottom = (f.bottom - r.top) + "px"; sm.style.top = "auto"; } else { sm.style.top = (r.bottom - f.top) + "px"; sm.style.bottom = "auto"; } } };
      return b;
    };
    // the bar along the top: none, a clock (and Activities), or menus (a Mac's)
    if (D.topbar !== "none") {
      const top = el("div", "topbar");
      if (D.topbar === "menus") { if (th.brand) top.append(el("span", "brand", th.brand)); top.append(...menus(th)); top.append(el("span", "grow")); top.append(...statusParts()); if (D.clock) top.append(clockEl()); }
      else { if (D.start) top.append(startBtn(D.start)); const c = el("span", "center"); if (D.clock) c.append(clockEl()); top.append(c, el("span", "grow"), ...statusParts()); }
      frame.append(top);
    }
    if (D.dock !== "none") {
      const dock = el("div", "dock dock-" + D.dock);
      for (const [k, l] of TABS) { const b = el("button", "ditem"); b.title = l; b.append(iconEl(th, k, "icon-img")); b.onclick = () => openWin(k); dockOf[k] = b; dock.append(b); }
      if (D.dock === "left") { middle.append(dock, desk); } else { middle.append(desk); }
      frame.append(middle);
      if (D.dock === "bottom") frame.append(dock);
    } else { middle.append(desk); frame.append(middle); }
    if (D.bar !== "none") {
      const bar = el("div", "bar");
      const tray = el("div", "tray"); if (D.topbar === "none") { tray.append(...statusParts()); if (D.clock) tray.append(clockEl()); }
      if (D.start && D.topbar === "none") bar.append(startBtn(D.start));
      bar.append(tasks, tray);
      if (D.bar === "top") frame.prepend(bar); else frame.append(bar);
    }
    frame.append(sm);
    // Windows open at the last start come back, minimized ones first so the shown ones end in front.
    const opened = TABS.map(([k]) => k).filter((k) => places[k]?.open);
    const minimized = opened.filter((k) => places[k]?.min), shown = opened.filter((k) => !places[k]?.min);
    for (const k of minimized) { openWin(k); wins[k].hidden = true; places[k].min = true; taskOf[k]?.classList.remove("on"); }
    for (const k of (shown.length ? shown : minimized.length ? [] : ["home"])) openWin(k);
    save();
    // A screen opened from elsewhere (a link in another screen, goTab): its window comes up.
    new MutationObserver(() => { const on = document.querySelector("section.tab.on")?.id; if (on && wins[on] && (wins[on].hidden || !taskOf[on])) openWin(on); })
      .observe(shell, { subtree: true, attributes: true, attributeFilter: ["class"] });
  }

  function draw(th, id) {
    let st = document.getElementById("theme-screens");
    if (!st) { st = el("style"); st.id = "theme-screens"; document.head.append(st); }
    st.textContent = screensCss(th);
    document.body.dataset.themed = th.layout;
    shell = el("div"); shell.id = "theme-frame"; document.body.append(shell);
    root = shell.attachShadow({ mode: "open" });
    const base = el("style");
    base.textContent = FRAME_CSS + `.frame{font:${th.size}px/1.3 ${FONTS[th.font]};color:${th.light.ink}}
@media (prefers-color-scheme: dark){.frame{color:${th.dark.ink}}}`;
    const own = el("style"); own.textContent = th.chrome;
    const frame = el("div", "frame");
    root.append(base, own, frame);
    // The window's own pieces the frame shows: the chain picker and the connection, then every screen.
    const chainsel = document.querySelector(".chainsel"); if (chainsel) { chainsel.slot = "chain"; shell.append(chainsel); }
    const conn = document.getElementById("conn"); if (conn) { conn.slot = "conn"; shell.append(conn); }
    for (const s of document.querySelectorAll("main > section.tab")) shell.append(s);
    if (th.layout === "desktop") buildDesktop(th, frame, id); else buildWindow(th, frame);
    root.addEventListener("click", () => closeMenus());
    document.addEventListener("click", () => closeMenus());
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeMenus(); });
  }

  // --- choosing ------------------------------------------------------------------------------------------------
  const read = (k) => { try { return JSON.parse(localStorage.getItem(k) || "null"); } catch { return null; } };
  const write = (k, v) => { try { v === null ? localStorage.removeItem(k) : localStorage.setItem(k, JSON.stringify(v)); } catch {} };

  async function installedThemes() {
    const list = await api("apps.installed");
    return (list || []).filter((a) => !a.missing && !a.removed && a.card?.category === "themes");
  }
  async function manifestOf(a) {
    // An app page is an arcade ledger inscription (INSC): its text, up to 64 KB, as the Explorer reads it.
    const r = await api("explorer.ledger_inscription", { chain: a.chain, id: a.txid });
    const text = r?.content?.text;
    if (typeof text !== "string") throw new Error("this theme's page couldn't be read here (is its chain kept or borrowed?)");
    return fromPage(text);
  }
  /** Wear an installed theme app (apps.installed / apps.store row). Checks it first; reloads the window. */
  async function use(a) {
    const th = await manifestOf(a);
    // A store row says whether it is installed; an installed row has no such flag. Same id either way.
    if (a.installed === false) await api("apps.install", { chain: a.chain, txid: a.txid });
    const id = a.id;
    write(KEY, { id, chain: a.chain, txid: a.txid, name: a.card?.name || th.name });
    write(CACHE, { txid: a.txid, manifest: th });
    location.reload();
  }
  function clear() { write(KEY, null); write(CACHE, null); location.reload(); }

  async function start() {
    const ref = read(KEY); if (!ref) return;
    current = ref;
    const narrow = () => innerWidth < 700;
    const cached = read(CACHE);
    let th = null;
    try { if (cached?.manifest) th = check({ arcade_theme: 1, ...unparse(cached.manifest) }); } catch { th = null; }
    if (th && !(th.layout === "desktop" && narrow())) draw(th, ref.id);
    // then the newest version of it, in the background: a publisher's update is worn at the next start
    try {
      const a = (await installedThemes()).find((x) => x.id === ref.id);
      if (!a) { write(KEY, null); write(CACHE, null); if (th) location.reload(); return; }
      if (a.txid !== cached?.txid) {
        const fresh = await manifestOf(a);
        write(CACHE, { txid: a.txid, manifest: fresh }); write(KEY, { ...ref, txid: a.txid });
        if (!th && !(fresh.layout === "desktop" && narrow())) draw(fresh, ref.id);
      }
    } catch (e) { console.warn("theme:", e.message); }
  }
  // A checked theme kept in the cache, back into the form `check` takes (it is checked again on every start).
  function unparse(m) {
    return { layout: m.layout, colors: { light: m.light, dark: m.dark }, font: m.font, size: m.size, radius: m.radius,
      desktop: m.desktop, icons: m.icons, brand: m.brand, window: m.window, chrome: m.chrome, name: m.name };
  }

  return { start, use, clear, check, fromPage, installedThemes, current: () => read(KEY) };
})();

document.addEventListener("DOMContentLoaded", () => { THEME.start(); });
