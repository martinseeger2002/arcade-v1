// Language: the window's words in the language chosen under Options → Language (docs/I18N.md).
// The window is written in English, and English stays the source: a language is a catalog from each
// English text to its translation (locales/<code>.js). Text is translated where it appears, whichever tab
// put it there (a MutationObserver), so the tabs stay plain English, and text without a translation yet
// simply shows in English. Every change is remembered (the English it replaced), so another choice applies
// at once, without a reload, and English puts back exactly what the tabs wrote.
// Loaded before every other script, in the main window and in app windows.
"use strict";
const I18N = (() => {
  // The window's languages (the six of the United Nations, then German), each by its own name (shown as is, never translated).
  const LANGS = { en: "English", ar: "العربية", "zh-Hans": "中文（简体）", fr: "Français", ru: "Русский", es: "Español", de: "Deutsch" };
  const RTL = new Set(["ar"]);                 // written right to left: the window mirrors (dir="rtl")
  const STORE = "arcade.lang";                 // this window's choice (localStorage), like its look (theme.js)
  const COLLECT = "arcade.i18n.collect";       // "1": keep the texts that found no translation (I18N.dump())

  const stored = () => { try { const s = localStorage.getItem(STORE) || ""; return LANGS[s] ? s : "en"; } catch { return "en"; } };
  let lang = "en";                             // the language shown now
  let cur = null;                              // its catalog (null: English)
  const CATALOGS = new Map();                  // code -> catalog
  const norm = (s) => s.replace(/\s+/g, " ").trim();
  const LETTER = /[A-Za-z]/;
  const HEAD = 5;                              // patterns are found by the first letters of their text
  let missing = null;
  try { if (localStorage.getItem(COLLECT) === "1") missing = new Set(); } catch {}

  // A catalog: exact texts, patterns ("{0} blocks behind") by their first letters (or loose, with a value
  // first), and the way back (translation -> English) for a translated text copied elsewhere by the window.
  function catalog(table) {
    const c = { exact: new Map(), byHead: new Map(), loose: [], back: new Map() };
    for (const [k, v] of Object.entries(table)) {
      if (typeof v !== "string" || !v) continue;
      const nk = norm(k);
      if (/\{\d+\}/.test(nk)) compile(c, nk, v);
      else { c.exact.set(nk, v); if (!c.back.has(norm(v))) c.back.set(norm(v), nk); }
    }
    return c;
  }
  function compile(c, key, out) {
    const parts = key.split(/\{(\d+)\}/);        // literal, number, literal, number, ..., literal
    let re = "^"; const order = [];
    for (let i = 0; i < parts.length; i++) {
      if (i % 2) { re += "(.*?)"; order.push(parts[i]); }      // a value may be empty (a plural "s" that isn't there)
      else re += parts[i].replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    }
    // A pattern with few words of its own ("{0} of {1}{2}") would catch any text with " of " in it: it counts
    // only where a value is a number or a known text, and no value is a phrase of its own.
    const weak = parts.filter((x, i) => !(i % 2)).join("").replace(/[^A-Za-z]/g, "").length < 6;
    const p = { re: new RegExp(re + "$", "s"), order, out, weak };
    if (parts[0].length >= HEAD) {
      const h = parts[0].slice(0, HEAD);
      if (!c.byHead.has(h)) c.byHead.set(h, []);
      c.byHead.get(h).push(p);
    } else c.loose.push(p);
  }

  // One English text (whitespace already folded) -> its translation, or null.
  function lookup(text) {
    if (!cur) return null;
    const hit = cur.exact.get(text);
    if (hit !== undefined) return hit;
    if (text.length < 400 && text.includes(" ")) {
      const cands = cur.byHead.get(text.slice(0, HEAD));
      for (const list of cands ? [cands, cur.loose] : [cur.loose]) {
        for (const p of list) {
          const m = p.re.exec(text);
          if (!m) continue;
          const vals = m.slice(1);
          if (p.weak && (!vals.some((v) => /\d/.test(v) || cur.exact.has(norm(v))) || vals.some((v) => norm(v).split(" ").length > 2 && !cur.exact.has(norm(v))))) continue;
          const val = {};
          p.order.forEach((n, i) => { const v = vals[i]; val[n] = value(v); });
          return p.out.replace(/\{(\d+)\}/g, (all, n) => (n in val ? val[n] : all));
        }
      }
    }
    if (missing && LETTER.test(text) && text.length < 2000) missing.add(text);
    return null;
  }
  // A value in a pattern: a known text is translated too ("pay 1 DOGE" in "You {0} in all"), one level deep.
  let depth = 0;
  function value(v) {
    const k = norm(v);
    const hit = cur.exact.get(k);
    if (hit !== undefined) return hit;
    if (depth || !LETTER.test(k) || !k.includes(" ")) return v;
    depth++;
    try { const r = lookup(k); return r === null ? v : r; } finally { depth--; }
  }
  const t = (s) => { if (!cur || s == null) return s; const r = lookup(norm(String(s))); return r === null ? s : r; };

  // ---- what was changed, and the English it replaced ----
  const TXT = new WeakMap();                   // text node -> [English, shown]
  const ATT = new WeakMap();                   // element -> { attribute: [English, shown] }
  const PHR = new WeakMap();                   // element -> { top: [nodes shown], snap: [[element, English nodes, shown nodes]] }

  // ---- text nodes and attributes ----
  const SKIP = new Set(["SCRIPT", "STYLE", "TEXTAREA", "CODE", "PRE", "KBD", "SAMP", "SVG", "CANVAS", "IFRAME", "VIDEO", "AUDIO", "NOSCRIPT"]);
  const skip = (el) => SKIP.has(el.tagName) || el.isContentEditable || el.getAttribute("translate") === "no" || el.classList.contains("notranslate");
  const idLike = (s) => !s.includes(" ") && (s.length > 24 || /^(@|0x|https?:)|^[0-9a-f]{8,}$/i.test(s));
  const around = (d, r) => d.match(/^\s*/)[0] + r + d.match(/\s*$/)[0];

  function text(node) {
    const d = node.data;
    const rec = TXT.get(node);
    if (rec) { if (rec[1] === d) return; TXT.delete(node); }    // shown already; or the window wrote new English
    if (!LETTER.test(d)) return;
    const k = norm(d);
    if (idLike(k)) return;
    const r = lookup(k);
    if (r !== null) { const shown = around(d, r); node.data = shown; TXT.set(node, [d, shown]); return; }
    const en = cur.back.get(k);                                  // a translated text the window copied (theme menus)
    if (en !== undefined) TXT.set(node, [around(d, en), d]);
  }
  const ATTRS = ["title", "placeholder", "aria-label", "alt"];
  function attr(el, a, v, set) {
    const recs = ATT.get(el);
    const rec = recs && recs[a];
    if (rec && rec[1] === v) return;
    if (!v || !LETTER.test(v)) return;
    const r = lookup(norm(v));
    if (r === null) return;
    set(r);
    if (recs) recs[a] = [v, r]; else ATT.set(el, { [a]: [v, r] });
  }
  function attrs(el) {
    for (const a of ATTRS) attr(el, a, el.getAttribute(a), (r) => el.setAttribute(a, r));
    if (el.tagName === "INPUT" && /^(button|submit|reset)$/i.test(el.type)) attr(el, "value", el.value, (r) => { el.value = r; });
  }

  // ---- sentences with markup in them ----
  // "Use <b>Restore</b> on Home" is one sentence for a translator, whose words may go in another
  // order. Its key numbers the elements in it: "Use <0>Restore</0> on Home"; the translation uses
  // the same numbers, and the window's own elements (with their ids and handlers) are put back in
  // the translation's order. Only elements that hold nothing but text and other such elements count.
  const BLOCK = new Set(("DIV P SECTION ARTICLE ASIDE TABLE THEAD TBODY TFOOT TR TD TH UL OL LI DL DT DD H1 H2 H3 H4 H5 H6 " +
    "SELECT OPTION OPTGROUP TEXTAREA NAV HEADER MAIN FOOTER FORM FIELDSET DETAILS SUMMARY FIGURE BLOCKQUOTE HR IMG").split(" "));
  const inline = (el) => !BLOCK.has(el.tagName) && !SKIP.has(el.tagName) && [...el.children].every(inline);
  const same = (list, arr) => list.length === arr.length && arr.every((n, i) => list[i] === n);
  function skeleton(el, els) {
    let s = "";
    for (const c of el.childNodes) {
      if (c.nodeType === 3) s += c.data;
      else if (c.nodeType === 1) {
        const n = els.length; els.push(c);
        s += c.childNodes.length ? `<${n}>${skeleton(c, els)}</${n}>` : `<${n}/>`;
      }
    }
    return s;
  }
  function phrase(el) {
    const was = PHR.get(el);
    if (was) { if (same(el.childNodes, was.top)) return true; PHR.delete(el); }
    let words = false, kids = 0;
    for (const c of el.childNodes) {
      if (c.nodeType === 3) { if (LETTER.test(c.data)) words = true; } else if (c.nodeType === 1) { if (!inline(c)) return false; kids++; }
    }
    if (!words || !kids) return false;
    const els = [];
    const k = norm(skeleton(el, els));
    const r = lookup(k);
    if (r === null) return false;
    const toks = r.match(/<\d+\/>|<\d+>|<\/\d+>|[^<]+|</g) || [];
    const used = new Set();
    for (const tk of toks) { const m = tk.match(/^<\/?(\d+)\/?>$/); if (m) used.add(+m[1]); }
    if (used.size !== els.length || [...used].some((n) => n >= els.length)) return false;   // a translation that lost an element
    const snap = [el, ...els].map((e) => [e, [...e.childNodes]]);
    let i = 0;
    const fill = (into, close) => {
      into.replaceChildren();
      for (; i < toks.length; i++) {
        const tk = toks[i];
        let m;
        if ((m = tk.match(/^<(\d+)\/>$/))) into.append(els[+m[1]]);
        else if ((m = tk.match(/^<(\d+)>$/))) { const e = els[+m[1]]; i++; fill(e, +m[1]); into.append(e); }
        else if ((m = tk.match(/^<\/(\d+)>$/))) { if (+m[1] === close) return; }
        else into.append(document.createTextNode(tk));
      }
    };
    fill(el, -1);
    for (const s of snap) s.push([...s[0].childNodes]);
    PHR.set(el, { top: [...el.childNodes], snap });
    return true;
  }

  function walk(node) {
    if (node.nodeType === 3) { if (node.parentElement && !skip(node.parentElement)) text(node); return; }
    if (node.nodeType === 11) { for (const c of [...node.childNodes]) walk(c); return; }    // a shadow root (a theme's frame)
    if (node.nodeType !== 1) return;
    if (node.shadowRoot) walk(node.shadowRoot);
    if (skip(node)) { if (node.tagName === "TEXTAREA") attrs(node); return; }   // its placeholder, never what's typed
    attrs(node);
    if (phrase(node)) return;
    for (const c of [...node.childNodes]) walk(c);
  }

  // Back to the English the window wrote, where it is still what this layer put there.
  function restore(node) {
    if (node.nodeType === 3) {
      const rec = TXT.get(node);
      if (rec) { if (node.data === rec[1]) node.data = rec[0]; TXT.delete(node); }
      return;
    }
    if (node.nodeType === 1) {
      if (node.shadowRoot) restore(node.shadowRoot);
      const recs = ATT.get(node);
      if (recs) {
        for (const [a, [en, shown]] of Object.entries(recs)) {
          if (a === "value") { if (node.value === shown) node.value = en; } else if (node.getAttribute(a) === shown) node.setAttribute(a, en);
        }
        ATT.delete(node);
      }
      const p = PHR.get(node);
      if (p) {
        if (same(node.childNodes, p.top)) for (const [e, en, shown] of p.snap) { if (e === node || same(e.childNodes, shown)) e.replaceChildren(...en); }
        PHR.delete(node);
      }
    } else if (node.nodeType !== 11) return;
    for (const c of [...node.childNodes]) restore(c);
  }

  // ---- watching the window ----
  const ROOTS = [];                            // open shadow roots made in this window (a theme's frame)
  const attach = Element.prototype.attachShadow;
  Element.prototype.attachShadow = function (init) {
    const root = attach.call(this, init);
    if (init && init.mode === "open") { ROOTS.push(root); if (cur && obs) watch(root); }
    return root;
  };
  const obs = new MutationObserver((recs) => {
    if (!cur) return;
    for (const r of recs) {
      if (r.type === "childList") r.addedNodes.forEach((n) => { if (n.isConnected) (n.nodeType === 3 && r.target.nodeType === 1 && phrase(r.target)) || walk(n); });
      else if (r.type === "characterData") { const p = r.target.parentElement; if (p && !skip(p) && !(p.childElementCount && phrase(p))) text(r.target); }
      else if (r.type === "attributes" && r.target.nodeType === 1 && !skip(r.target)) attrs(r.target);
    }
    obs.takeRecords();                         // our own writes above
  });
  const watch = (target) => obs.observe(target, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: [...ATTRS, "value"] });

  // Show `code` now: everything back to English, then into the new language, and watch for more.
  function apply(code) {
    obs.disconnect();
    if (cur) restore(document.documentElement);             // a theme's frame too: its host is in the page
    lang = code;
    cur = code === "en" ? null : CATALOGS.get(code) || null;
    const html = document.documentElement;
    html.lang = code;
    html.dir = RTL.has(code) ? "rtl" : "ltr";
    if (cur) {
      if (document.body) walk(document.body);
      watch(document.documentElement);
      for (const r of ROOTS) watch(r);
    }
    obs.takeRecords();
    picker();
    window.dispatchEvent(new CustomEvent("arcade:lang", { detail: { lang: code } }));
  }

  // A language's file (locales/<code>.js) calls this with its catalog.
  const waiting = new Map();                   // code -> resolve
  function add(code, table) {
    CATALOGS.set(code, catalog(table));
    const done = waiting.get(code); if (done) { waiting.delete(code); done(); }
  }
  function load(code) {
    if (code === "en" || CATALOGS.has(code)) return Promise.resolve();
    return new Promise((resolve) => {
      waiting.set(code, resolve);
      const s = document.createElement("script");
      s.src = "locales/" + code + ".js";
      s.onerror = () => { waiting.delete(code); resolve(); };      // no catalog: English
      (document.head || document.documentElement).append(s);
    });
  }
  const ready = () => new Promise((r) => (document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", r, { once: true }) : r()));

  // Choose a language for this window: kept (localStorage), and shown at once.
  let asked = 0;
  async function use(code, keep = true) {
    if (!LANGS[code]) code = "en";
    if (keep) { try { if (code === "en") localStorage.removeItem(STORE); else localStorage.setItem(STORE, code); } catch {} }
    const n = ++asked;
    await load(code);
    await ready();
    if (n === asked) apply(code);
  }

  // Options → Language (index.html #o-lang-pick): six choices, each in its own script.
  function picker() {
    const sel = document.getElementById("o-lang-pick");
    if (!sel) return;
    if (!sel.options.length) {
      for (const [c, name] of Object.entries(LANGS)) {
        const o = document.createElement("option");
        o.value = c; o.textContent = name; o.lang = c; o.dir = RTL.has(c) ? "rtl" : "ltr"; o.setAttribute("translate", "no");
        sel.append(o);
      }
      sel.addEventListener("change", () => use(sel.value));
    }
    sel.value = lang;
  }

  // Dialogs from the tabs (alert/confirm/prompt) say their text in this language too.
  for (const f of ["alert", "confirm", "prompt"]) {
    const orig = window[f];
    if (typeof orig !== "function") continue;
    window[f] = function (msg, ...rest) { return orig.call(window, msg == null || !cur ? msg : String(msg).split("\n").map(t).join("\n"), ...rest); };
  }
  // Another window of this app (an app window, the main window) chose: follow it.
  window.addEventListener("storage", (e) => { if (e.key === STORE) use(e.newValue || "en", false); });

  // At start: the kept choice. Until its catalog is in, the window stays hidden (briefly), so it never
  // flashes English first.
  const first = stored();
  if (first !== "en") {
    const html = document.documentElement;
    html.style.visibility = "hidden";
    const show = () => { html.style.visibility = ""; };
    setTimeout(show, 2500);
    use(first, false).then(show, show);
  } else {
    document.documentElement.lang = "en";
    ready().then(picker);
  }

  const dump = () => (missing ? [...missing].sort() : []);
  return { get lang() { return lang; }, langs: LANGS, rtl: (c) => RTL.has(c || lang), t, add, use, dump };
})();
