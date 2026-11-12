// Arcade V1.0 desktop: everything goes through arcaded's local API (one JSON request each);
// $, api, h and the coin helpers are in common.js.

// The chain picked at the top (explorer.js owns the picker); every tab follows it.
const picked = () => (window.arcadeChain && window.arcadeChain()) || { key: HOME.key, name: HOME.name, home: true };
const coins = (sats, key) => coinNum(sats) + " " + (TICKERS[key || picked().key] || "");
window.addEventListener("arcade:chain", () => refresh());
const short = (s, n = 12) => (s && s.length > n ? s.slice(0, n) + "…" : s || "");
function msg(el, text, kind = "") { el.textContent = text; el.className = "msg " + kind; }
function rows(tbody, items, cells) {
  tbody.replaceChildren(...items.map((it) => {
    const tr = document.createElement("tr");
    cells(it).forEach(([text, cls]) => { const td = document.createElement("td"); td.textContent = text; if (cls) td.className = cls; tr.append(td); });
    return tr;
  }));
}

// Tabs
document.querySelectorAll("nav button").forEach((b) => b.addEventListener("click", () => {
  document.querySelectorAll("nav button").forEach((x) => x.classList.toggle("on", x === b));
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("on", t.id === b.dataset.tab));
  refresh();
}));

let state = { wallet: false, keys: [], claims: [] };

async function refreshHome() {
  const s = await api("status");
  state.wallet = s.wallet;
  $("conn").textContent = "connected"; $("conn").className = "pill ok";
  if (!$("home").classList.contains("on")) return;
  await drawStart();
  drawMesh(await api("mesh.overview"));
}

// Home: a getting-started guide until this arcade's owner has a handle.
const goTab = (t) => { const b = document.querySelector(`nav button[data-tab="${t}"]`); if (b) b.click(); };
// Getting started, all on Home (Martin): make the wallet here (its 12 words shown here, with
// "I wrote them down"), wait for 1 test coin (asked for by itself), claim the @handle here.
// The card goes away once the handle is confirmed.
const START = { phrase: null, saved: false, tag: "", checked: null, busy: false, msg: "" };
async function drawStart() {
  const st = await api("start.status").catch(() => null);
  const box = $("h-start");
  if (!st || (st.tag && !START.phrase)) { box.hidden = true; drawStart.sig = null; return; }
  box.hidden = false;
  const c = st.claim && st.claim.status !== "failed" ? st.claim : null;
  const failed = st.claim && st.claim.status === "failed" ? st.claim : null;
  const coin = homeTicker();
  const walletDone = st.wallet && !START.phrase;
  const coinsHere = st.balance > 0;
  // Redrawn only when something a step shows changes (typing must survive the 4 s refresh);
  // the "asked … ago" line is updated in place.
  const sig = JSON.stringify([st.wallet, !!START.phrase, START.saved, coinsHere, !!st.faucet, st.faucet && st.faucet.status, c && [c.tag, c.status, c.note], failed && failed.tag, START.checked, START.busy, START.msg]);
  // A fresh install reads the chain first; a coin paid in a recent block shows once it's there.
  const catchUp = () => st.height != null && st.best != null && st.best > st.height + 2
    ? `Your arcade is still reading the ${HOME.name} chain (block ${num(st.height)} of ${num(st.best)}, ${Math.floor(st.height * 100 / st.best)}%); the coin shows once it gets there.`
    : "";
  const used = !!c && (c.status === "sent" || c.status === "confirmed");
  const askedText = () => !st.wallet ? `It's asked for by itself as soon as your wallet exists.`
    : used ? `Received, and used to publish your @handle.`
    : coinsHere ? `Here: ${homeCoins(st.balance)}. That pays for publishing your @handle.`
    : st.faucet && st.faucet.status === "paid" ? `Another arcade says it sent it${st.faucet.txid ? ` (transaction ${st.faucet.txid.slice(0, 12)}…)` : ""}. ${catchUp() || "It shows here once that payment arrives; if it doesn't, your arcade asks again by itself."}`
    : st.faucet ? `Asked the arcade mesh for it ${ago(st.faucet.at) === "just now" ? "just now" : ago(st.faucet.at) + " ago"}; another arcade sends it, usually within a minute or two. Nothing to do: it's used as soon as it arrives, even before a block confirms it.${catchUp() ? " " + catchUp() : ""}`
    : `It's asked for by itself in a moment.`;
  if (sig === drawStart.sig) {
    const el = $("h-asked");
    if (el) el.textContent = askedText();
    return;
  }
  drawStart.sig = sig;

  // 1. The wallet.
  let s1;
  if (START.phrase) {
    const done = h("button", { class: "primary", disabled: !START.saved, onclick: () => { START.phrase = null; START.saved = false; drawStart.sig = null; refresh(); } }, "Done");
    s1 = [h("div", { class: "hint" }, "Write these 12 words down, in order, and keep them somewhere safe. They are shown once, and they're the only way to get your wallet and @handle back."),
      h("div", { class: "phrase start-phrase" }, START.phrase),
      h("label", { class: "start-check" }, h("input", { type: "checkbox", checked: START.saved, onchange: (e) => { START.saved = e.target.checked; done.disabled = !START.saved; } }), " I wrote them down"),
      h("div", { class: "row" }, done)];
  } else if (st.wallet) {
    s1 = [h("div", { class: "hint" }, "Done. Your wallet is kept on this PC; its 12 words are the only backup.")];
  } else {
    const make = h("button", { class: "primary", onclick: async () => {
      make.disabled = true;
      try { const r = await api("wallet.create"); START.phrase = r.phrase; START.saved = false; drawStart.sig = null; refresh(); }
      catch (e) { make.disabled = false; alert(e.message); }
    } }, "Create a new wallet");
    s1 = [h("div", { class: "hint" }, "New here: create a wallet; you'll get 12 words to write down. Had one before: type its 12 words below instead to restore it, your @handle and coins included."),
      h("div", { class: "row" }, make), restoreBox()];
  }

  // 2. One test coin.
  const s2 = [h("div", { class: "hint", id: "h-asked" }, askedText()),
    !coinsHere && !used ? h("div", { class: "hint" }, `Why: your @handle is published on the ${HOME.name} chain, and publishing pays a small network fee. ${coin} is the test coin of that chain; it has no money value.`) : null];

  // 3. The @handle, claimed here.
  let s3;
  if (c) {
    const step = c.status === "sent" ? "published; it's yours once the next block confirms it (about a minute)" : c.status === "funding" ? `waiting for the test coin, then it's published by itself` : c.note || c.status;
    s3 = [h("div", { class: "hint" }, `@${c.tag}: ${step}.`)];
  } else if (!st.wallet || START.phrase) {
    s3 = [h("div", { class: "hint" }, "Your name on the arcade (2–24 characters: a–z, 0–9 and _), seen by every arcade. Pick it here once your wallet is made.")];
  } else {
    const input = h("input", { placeholder: "yourname", maxlength: "24", autocomplete: "off", value: START.tag, oninput: (e) => { START.tag = e.target.value.toLowerCase(); if (START.checked) { START.checked = null; START.msg = ""; drawStart.sig = null; drawStart(); } } });
    const check = h("button", { disabled: START.busy, onclick: async () => {
      START.busy = true;
      try { const r = await api("tag.check", { tag: START.tag }); START.checked = r.available ? r.tag : null; START.msg = r.available ? `@${r.tag} is free.` : `@${r.tag} isn't available: ${r.reason}`; }
      catch (e) { START.checked = null; START.msg = e.message; }
      START.busy = false; drawStart.sig = null; drawStart();
    } }, "Check");
    const claim = h("button", { class: "primary", disabled: !START.checked || START.busy, onclick: async () => {
      START.busy = true; drawStart.sig = null; drawStart();
      try { await api("tag.claim", { tag: START.checked }); START.msg = ""; START.checked = null; }
      catch (e) { START.msg = e.message; }
      START.busy = false; drawStart.sig = null; refresh();
    } }, "Claim");
    s3 = [h("div", { class: "hint" }, failed ? `@${failed.tag} didn't work (${failed.note}); pick another.` : "Your name on the arcade (2–24 characters: a–z, 0–9 and _). It's published on the chain, so every arcade sees it."),
      h("div", { class: "row start-claim" }, h("span", { class: "at" }, "@"), input, check, claim),
      START.msg ? h("p", { class: "msg " + (START.checked ? "ok" : "bad") }, START.msg) : null];
  }

  const steps = [
    { done: walletDone, title: START.phrase ? "Your 12 words" : st.wallet ? "Your wallet" : "Make your wallet, or restore yours", body: s1 },
    { done: coinsHere || used, title: coinsHere || used ? `1 ${coin} for your @handle` : `Waiting for 1 ${coin}`, body: s2 },
    { done: false, title: "Claim your @handle", body: s3 },
  ];
  $("h-steps").replaceChildren(...steps.map((x, i) => h("li", { class: x.done ? "done" : "" },
    h("span", { class: "n" }, x.done ? "✓" : String(i + 1)),
    h("div", { class: "start-body" }, h("b", {}, x.title), ...x.body))));
}

// Restore a wallet from its words right on Home (the same as the Wallet tab's Restore).
let restoring = false;
function restoreBox() {
  const ta = h("textarea", { rows: "2", placeholder: "or: your 12 words, separated by spaces", autocomplete: "off", spellcheck: "false", class: "start-words" });
  const m = h("p", { class: "msg" });
  const b = h("button", { onclick: async () => {
    if (restoring) return;
    restoring = true; b.disabled = true; msg(m, "Restoring…");
    try {
      await api("wallet.create", { phrase: ta.value.trim().toLowerCase().split(/\s+/).join(" ") });
      ta.value = "";
      msg(m, "Restored. Your addresses, @handle and coins show up as the chain is read.", "ok");
      drawStart.sig = null;
      refresh();
    } catch (e) { msg(m, e.message, "bad"); }
    finally { restoring = false; b.disabled = false; }
  } }, "Restore my wallet");
  return h("div", { class: "start-restore" }, ta, h("div", { class: "row" }, b), m);
}

// Home: every arcade linked or heard lately, who runs it, and what it offers per chain.
const ago = (t) => { const d = Math.max(0, Date.now() / 1000 - t); return d < 90 ? "just now" : d < 5400 ? `${Math.round(d / 60)} min` : d < 172800 ? `${Math.round(d / 3600)} h` : `${Math.round(d / 86400)} days`; };
const num = (n) => (n == null ? "?" : Number(n).toLocaleString());
const gb = (b) => (b ? `${(b / 1e9).toFixed(b < 1e10 ? 1 : 0)} GB` : "");
function chip(text, kind = "", tip) { return h("span", { class: "chip " + kind, title: tip }, text); }
function chainChips(c) {
  const out = [];
  if (c.holds) out.push(chip(`block ${num(c.holds.h)}`), ...(c.holds.ix || []).map((x) => chip(IX_NAMES[x] || x, "on")));
  if (c.lends != null) out.push(chip(`lends its Core · block ${num(c.lends)}`, "on"));
  if (c.blocks != null) out.push(chip(`serves blocks to ${num(c.blocks)}`));
  const cc = c.chain_copy;
  if (cc && cc.ready) out.push(chip(`chain copy ready · block ${num(cc.ready.h)}${cc.ready.bytes ? " · " + gb(cc.ready.bytes) : ""}`, "on"));
  else if (cc && cc.can) out.push(chip("can make a chain copy", "on"));
  else if (cc) out.push(chip("no chain copies", "off", cc.why || "not offered"));
  const o = c.ord;
  if (o && o.mode === "walk") out.push(chip(`walks ord to ${num(o.walk)}`));
  else if (o && o.mode === "help") out.push(chip("helps walk ord"));
  if (o && o.snapshot) out.push(chip(`ord snapshot · block ${num(o.snapshot.h)}${o.snapshot.whole ? "" : " (partial)"}`, "on"));
  const l = c.ledger;
  if (l && l.copy) out.push(chip(`ledger copy · block ${num(l.copy.h)}${l.copy.whole ? "" : " (partial)"}`, "on"));
  const x = c.ledger_check;
  if (x) out.push(x.same === true ? chip(`ledger matches ours to ${num(x.at)}`, "on") : x.same === false ? chip(`ledger differs from ours at ${num(x.at)}`, "bad") : chip(`ledger at ${num(x.h)}`));
  return out;
}
const IX_NAMES = { addresses: "addresses", ord: "inscriptions", tokens: "tokens", names: "names", maps: "maps", ledger: "arcade ledger", profiles: "profiles", messages: "messages" };
function drawMesh(ov) {
  const all = ov.arcades || [];
  const linked = all.filter((a) => a.linked).length;
  // 1. This arcade: its chains and the indexes it shares. 2. The mesh at a glance.
  const me = ov.me || {}, mesh = ov.mesh || { linked, heard: all.length - linked, chains: [] };
  const ixChips = (ix) => (ix || []).map((x) => chip(IX_NAMES[x] || x, "on"));
  const meCard = h("div", { class: "arcade me" },
    h("div", { class: "top" },
      h("span", { class: "who" }, me.tag ? "@" + me.tag : "This arcade"),
      h("span", { class: "pill ok" }, "this arcade"),
      me.version ? h("span", { class: "pill" }, me.version) : null),
    h("div", { class: "meta" }, `node ${short(me.node || "", 16)} · ${me.links ?? 0} link${me.links === 1 ? "" : "s"}`),
    (me.chains || []).length
      ? h("div", { class: "lines" }, ...me.chains.map((c) => h("div", { class: "line" },
          coinLogo(c.key, c.name, "span", "logo"), h("span", { class: "cn" }, c.name),
          h("span", { class: "chips" },
            c.home ? chip("home chain") : null,
            c.borrowed ? chip("syncing; borrowed meanwhile", "off") : chip(`block ${num(c.tip)}`),
            c.mode === "shared" ? chip("shared storage") : null,
            ...ixChips(c.indexes),
            ...(c.copies || []).map((k) => chip(`copy: ${k.fam} · block ${num(k.cp)}${k.bytes ? " · " + gb(k.bytes) : ""}`, "on"))))))
      : h("div", { class: "meta" }, "No chains yet."));
  const meshCard = h("div", { class: "arcade" },
    h("div", { class: "top" },
      h("span", { class: "who" }, "The mesh"),
      h("span", { class: "pill " + (mesh.linked ? "ok" : "") }, `${mesh.linked} linked arcade${mesh.linked === 1 ? "" : "s"}`),
      mesh.heard ? h("span", { class: "pill" }, `${mesh.heard} more heard`) : null),
    h("div", { class: "meta" }, "Coins and indexes other arcades share; the Explorer and Chains & Coins use them."),
    (mesh.chains || []).length
      ? h("div", { class: "lines" }, ...mesh.chains.map((c) => h("div", { class: "line" },
          coinLogo(c.key, c.name, "span", "logo"), h("span", { class: "cn" }, c.name),
          h("span", { class: "chips" },
            c.arcades ? chip(`kept by ${c.arcades} arcade${c.arcades === 1 ? "" : "s"}`) : null,
            c.lends ? chip("lends blocks") : null,
            ...ixChips(c.indexes)))))
      : h("div", { class: "meta" }, "Nothing shared yet."));
  $("h-top").replaceChildren(meCard, meshCard);
  $("h-arcades").replaceChildren(...all.map((a) => {
    const who = a.tag ? "@" + a.tag : "arcade " + short(a.node, 8);
    const chains = (a.chains || []).filter((c) => chainChips(c).length);
    return h("div", { class: "arcade" + (a.linked ? "" : " heard") },
      h("div", { class: "top" },
        h("span", { class: "who" }, who),
        a.version ? h("span", { class: "pill" }, a.version) : null,
        h("span", { class: "pill " + (a.linked ? "ok" : "") }, a.linked ? `linked ${ago(a.since)}` : "heard, not linked")),
      h("div", { class: "meta" }, `node ${short(a.node, 16)}${a.addr ? " · " + a.addr : ""}`),
      chains.length
        ? h("div", { class: "lines" }, ...chains.map((c) => h("div", { class: "line" },
            coinLogo(c.key, c.name, "span", "logo"), h("span", { class: "cn" }, c.name), h("span", { class: "chips" }, ...chainChips(c)))))
        : h("div", { class: "meta" }, "offers nothing to other arcades"));
  }));
}

async function drawAccounts() {
  const accs = await api("account.list").catch(() => []);
  $("w-accs").replaceChildren(...accs.map((a) => {
    const tr = document.createElement("tr");
    const name = (a.tag ? "@" + a.tag : (a.own ? "This arcade's wallet" : a.id)) + (a.active ? " (in use)" : "");
    for (const t of [name, a.address || (a.own ? "this PC" : ""), a.own ? "" : (a.from || "")]) {
      const td = document.createElement("td"); td.textContent = t; tr.append(td);
    }
    const td = document.createElement("td");
    if (!a.active) {
      const u = document.createElement("button"); u.textContent = "Use";
      u.addEventListener("click", async () => {
        try { await api("account.switch", { id: a.id }); msg($("w-msg"), `Now using ${name.replace(" (in use)", "")}.`, "ok"); refresh(); }
        catch (e) { msg($("w-msg"), e.message, "bad"); }
      });
      td.append(u);
    }
    if (!a.own) {
      const b = document.createElement("button"); b.textContent = "Sign out";
      b.addEventListener("click", async () => {
        if (b.dataset.sure !== "1") { b.dataset.sure = "1"; b.textContent = "Sign out: sure?"; return; }
        try { await api("account.signout", { id: a.id }); msg($("w-msg"), `Signed out; its words, keys and messages are gone from this arcade.`, "ok"); refresh(); }
        catch (e) { msg($("w-msg"), e.message, "bad"); }
      });
      td.append(b);
    }
    tr.append(td); return tr;
  }));
}

async function drawLinks(ch) {
  if (!$("w-ltag").value) {
    const home = await api("wallet.keys", { chain: HOME.key }).catch(() => []);
    const t = home.find((k) => k.tag);
    if (t) $("w-ltag").value = t.tag;
  }
  const links = await api("wallet.links", { chain: ch.key }).catch(() => []);
  $("w-links").replaceChildren(...(links.length ? links : [null]).map((l) => {
    const tr = document.createElement("tr");
    if (!l) { const td = document.createElement("td"); td.colSpan = 4; td.className = "note"; td.textContent = "None on this chain yet."; tr.append(td); return tr; }
    for (const [text, cls] of [[l.address], ["@" + l.tag], [l.balance == null ? "not installed" : coins(l.balance), "num"]]) {
      const td = document.createElement("td"); td.textContent = text; if (cls) td.className = cls; tr.append(td);
    }
    const td = document.createElement("td"); const b = document.createElement("button"); b.textContent = "Remove";
    b.addEventListener("click", async () => {
      try { await api("wallet.unlink", { chain: ch.key, address: l.address, tag: l.tag }); msg($("w-msg"), `Removed ${l.address}; other arcades are told too.`, "ok"); refreshWallet(); }
      catch (e) { msg($("w-msg"), e.message, "bad"); }
    });
    td.append(b); tr.append(td); return tr;
  }));
}

// The Wallet tab, as Dogecoin Arcade's: one card for the wallet picked (this arcade's own
// addresses, then imported wallets, then "+ Import a wallet"), Send and Receive, and the
// wallet's Activity, Tokens and NFTs underneath.
const usd = (c) => "$" + (c / 100).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const isDD = (k) => k === "dgb" || k === "dgbtest";
const W = { chain: null, sel: null, list: [], pickSig: "", tab: "activity", view: null, viewKey: "", viewAt: 0, dd: null, ddAt: 0 };
const cur = () => W.list.find((w) => w.id === W.sel) || W.list[0];
const shortAddr = (a) => (a && a.length > 20 ? a.slice(0, 10) + "…" + a.slice(-6) : a || "");
// Base units ("1234500", 3 decimals) as "1,234.5".
function units(s, dec) {
  s = String(s || "0").replace(/^0+(?=\d)/, "");
  if (!dec) return BigInt(s).toLocaleString();
  const p = s.padStart(dec + 1, "0");
  const whole = BigInt(p.slice(0, -dec)).toLocaleString(), frac = p.slice(-dec).replace(/0+$/, "");
  return frac ? `${whole}.${frac}` : whole;
}
function openExplorer(page, id) {
  const b = document.querySelector('nav button[data-tab="explorer"]');
  if (!b || !window.arcadeExplorer) return;
  b.click(); window.arcadeExplorer.go(page, id);
}

async function refreshWallet() {
  const ch = picked();
  $("w-none").hidden = state.wallet; $("w-main").hidden = !state.wallet;
  if ($("w-more").open) { drawAccounts(); drawLinks(ch); }
  if (!state.wallet) return;
  if (W.chain !== ch.key) {
    Object.assign(W, { chain: ch.key, sel: null, view: null, viewKey: "", dd: null, ddAt: 0 });
    for (const p of ["w-send-panel", "w-recv-panel", "w-imp-panel"]) $(p).hidden = true;
    $("w-confirm").hidden = true; msg($("w-send-msg"), "");
  }
  const [keys, imps] = await Promise.all([api("wallet.keys", { chain: ch.key }), api("wallet.imports").catch(() => [])]);
  state.keys = keys;
  W.list = [
    ...keys.map((k) => ({ id: "own:" + k.index, own: true, index: k.index, address: k.address, legacy: k.legacy, tag: k.tag, balance: k.balance, pending: k.pending || 0,
      label: k.tag ? "@" + k.tag : k.index ? `This arcade's wallet · address ${k.index}` : "This arcade's wallet" })),
    ...imps.filter((w) => w.chain === ch.key).map((w) => ({ id: "imp:" + w.address, own: false, address: w.address, balance: w.balance, name: w.name,
      kind: w.kind, addr_kind: w.addr_kind, spendable: w.addr_kind !== "p2sh-p2wpkh",
      label: `${w.name || "Imported"} · ${shortAddr(w.address)}` })),
  ];
  if (!W.list.some((w) => w.id === W.sel)) W.sel = W.list[0] && W.list[0].id;
  // The picker is rebuilt only when its wallets change, so an open list isn't closed under you.
  const sig = JSON.stringify(W.list.map((w) => [w.id, w.label]));
  if (sig !== W.pickSig) {
    W.pickSig = sig;
    const own = W.list.filter((w) => w.own), imp = W.list.filter((w) => !w.own);
    $("w-pick").replaceChildren(
      ...own.map((w) => h("option", { value: w.id }, w.label)),
      imp.length ? h("optgroup", { label: "Imported" }, ...imp.map((w) => h("option", { value: w.id }, w.label))) : null,
      h("option", { value: "+import" }, "+ Import a wallet…"));
  }
  $("w-pick").value = W.sel;
  // DigiDollar: this arcade's DigiByte key (address 0) only.
  if (isDD(ch.key) && Date.now() - W.ddAt > 20000) {
    W.ddAt = Date.now();
    W.dd = await api("dd.wallet", { chain: ch.key }).catch((e) => ({ error: e.message }));
  }
  drawCard(ch);
  const w = cur();
  const key = `${ch.key}|${w && w.address}`;
  if (w && (key !== W.viewKey || Date.now() - W.viewAt > 30000)) {
    if (key !== W.viewKey) { W.view = null; drawPane(ch); }
    W.viewKey = key; W.viewAt = Date.now();
    W.view = await api("explorer.address", { chain: ch.key, address: w.address }).catch((e) => ({ error: e.message }));
    if (W.viewKey === key) { drawPane(ch); drawCard(ch); }
  }
  drawTokenSends(ch);
}

// PRC-20 / DRC-20 sends under way (tokensend.rs): each one's note, then its send.
async function drawTokenSends(ch) {
  const jobs = (await api("token20.sends").catch(() => [])).filter((j) => j.chain === ch.key && (j.step !== "done" || Date.now() / 1000 - j.at < 3600));
  const says = { noted: "transfer note inscribed; sending once a block holds it", sent: "sent; waiting for a block", done: "done" };
  $("w-tsends").replaceChildren(...jobs.map((j) => h("div", { class: "wl-row" }, h("span", { class: "ic" }, (j.tick || "?")[0].toUpperCase()),
    h("div", { class: "mid" }, h("div", {}, `${j.amount} ${j.tick} to ${shortAddr(j.to)}`), h("div", { class: "s" }, says[j.step] || j.step, j.last ? ` · ${j.last}` : "")),
    h("span", { class: "amt" }, j.step === "done" ? "✓" : "…"))));
  $("w-tsends").hidden = !jobs.length;
}

function drawCard(ch) {
  const w = cur(); if (!w) return;
  const tk = TICKERS[ch.key] || ch.name;
  $("w-logo").replaceChildren(coinLogo(ch.key, ch.name, "span", "wl-logo"));
  $("w-who").textContent = w.own ? (w.tag ? "@" + w.tag : "This arcade's wallet") : `${w.name || "Imported wallet"} · imported ${w.kind === "phrase" ? "from words" : "key"}`;
  $("w-bal").textContent = w.balance == null ? "—" : coinNum(w.balance);
  $("w-unit").textContent = tk;
  const ddOn = w.own && w.index === 0 && isDD(ch.key) && W.dd && !W.dd.error;
  const sub = [];
  if (w.balance == null) sub.push(`${ch.name} isn't installed or borrowed with addresses here: add it in Chains & Coins to see balances.`);
  if (w.pending > 0) sub.push(`+${coinNum(w.pending)} ${tk} arriving (in the mempool, spendable now)`);
  if (ddOn) sub.push(`${usd(W.dd.cents)} DigiDollar`);
  if (!w.own && !w.spendable) sub.push("Wrapped SegWit: receive only for now.");
  $("w-sub").textContent = sub.join(" · ");
  $("w-addr").textContent = w.address;
  $("w-forget").hidden = w.own;
  $("w-btn-send").disabled = !w.own && !w.spendable;
  // The Send panel's assets: the chain's coin, DigiDollar on DigiByte, and the wallet's tokens: arcade tokens on the
  // home chain (any of this arcade's addresses holding them), PRC-20 / DRC-20 from its address 0.
  const assets = [[ch.key, tk]].concat(ddOn ? [["dd", "DigiDollar ($)"]] : []);
  const v = W.viewKey === `${ch.key}|${w.address}` ? W.view : null;
  if (v && w.own) {
    if (ch.key === HOME.key) for (const t of v.ledger_tokens || []) if (Number(t.units) > 0) assets.push([`lt:${t.propertyid}`, `${t.name || "Arcade token #" + t.propertyid} (arcade token)`]);
    if (w.index === 0) for (const t of v.tokens || []) if (t.overall !== "0") assets.push([`t20:${t.tick}`, `${t.tick} (${ch.key.startsWith("doge") ? "DRC-20" : "PRC-20"})`]);
  }
  // An inscription picked with its Send button (pickOrd): sent on its own.
  if (W.ordPick && W.ordPick.chain === ch.key && v && w.own && w.index === 0) assets.push([`ord:${W.ordPick.id}`, `Inscription #${num(W.ordPick.n)}`]);
  W.tokens = v || {};
  const was = $("w-asset").value;
  const asig = JSON.stringify(assets);
  if (asig !== W.assetSig) {
    W.assetSig = asig;
    $("w-asset").replaceChildren(...assets.map(([v, t]) => h("option", { value: v }, t)));
    if (assets.some((a) => a[0] === was)) $("w-asset").value = was;
  }
  $("w-asset").hidden = assets.length < 2;
  assetChanged();
  // Receive: the address, and on DigiByte the DigiDollar one and the older D... form.
  const rows = [[`${tk} address`, w.address]];
  if (ddOn) rows.push(["DigiDollar address", W.dd.address]);
  $("w-recv-list").replaceChildren(...[...rows.map(([l, a]) => h("div", { class: "wl-recv" }, h("span", { class: "lbl" }, l),
      h("div", {}, h("code", {}, a), " ", h("button", { class: "small", onclick: (e) => copyText(a, e.target) }, "Copy")))),
    w.legacy ? h("p", { class: "note" }, `Coins sent to ${w.legacy} (the older D... form of the same key) are counted too. Give people the dgb1... address: D... addresses look the same on Dogecoin.`) : null,
    ddOn && !W.dd.whole ? h("p", { class: "note" }, "DigiByte is borrowed here, so DigiDollar coins received before this arcade began following it aren't seen. Installing DigiByte finds them all.") : null].filter(Boolean));
}

/** An inscription's Send: the Send panel with it picked; the amount is the inscription itself. */
async function pickOrd(i) {
  W.ordPick = { chain: picked().key, id: i.id, n: i.n };
  W.assetSig = "";
  await refreshWallet().catch(() => {});
  $("w-asset").value = `ord:${i.id}`;
  assetChanged();
  if ($("w-send-panel").hidden) showPanel("w-send-panel");
  $("w-to").focus();
}

function assetChanged() {
  const a = $("w-asset").value, dd = a === "dd";
  // An inscription goes whole: no amount.
  $("w-amt").hidden = $("w-amt-unit").hidden = a.startsWith("ord:");
  const lt = a.startsWith("lt:") && (W.tokens.ledger_tokens || []).find((t) => `lt:${t.propertyid}` === a);
  $("w-amt-unit").textContent = dd ? "USD" : a.startsWith("t20:") ? a.slice(4) : lt ? (lt.name || "units") : TICKERS[picked().key] || "";
  $("w-to").placeholder = dd ? (picked().key === "dgbtest" ? "TD... address" : "DD... address") : `${picked().name} address`;
}

function drawPane(ch) {
  document.querySelectorAll(".wl-tabs button").forEach((b) => b.classList.toggle("on", b.dataset.wt === W.tab));
  const v = W.view, w = cur();
  const pane = $("w-pane");
  $("w-note").textContent = (v && v.note) || ""; $("w-note").hidden = !(v && v.note);
  if (!v) { pane.replaceChildren(h("div", { class: "x-empty" }, "Loading…")); return; }
  if (v.error) { pane.replaceChildren(h("div", { class: "x-empty bad" }, v.error)); return; }
  if (W.tab === "activity") {
    // One row per transaction: what it did to this address in all.
    const by = new Map();
    for (const r of v.history || []) {
      const t = by.get(r.txid) || { txid: r.txid, height: r.height, net: 0 };
      t.net += (r.received || 0) - (r.sent || 0); by.set(r.txid, t);
    }
    const txs = [...by.values()];
    pane.replaceChildren(txs.length ? h("div", { class: "wl-rows" }, txs.map((t) => h("div", { class: "wl-row click", onclick: () => openExplorer("tx", t.txid) },
      h("span", { class: "ic " + (t.net >= 0 ? "in" : "out") }, t.net >= 0 ? "↓" : "↑"),
      h("div", { class: "mid" }, h("div", {}, t.net >= 0 ? "Received" : "Sent"), h("div", { class: "s" }, `block ${num(t.height)} · ${t.txid}`)),
      h("span", { class: "amt " + (t.net >= 0 ? "in" : "") }, (t.net >= 0 ? "+" : "−") + coins(Math.abs(t.net), ch.key)))))
      : h("div", { class: "x-empty" }, v.history ? "No activity yet." : "Activity needs the Addresses index for this chain (Chains & Coins)."));
  } else if (W.tab === "tokens") {
    const list = [];
    if (w && w.own && w.index === 0 && isDD(ch.key) && W.dd && !W.dd.error)
      list.push(h("div", { class: "wl-row" }, h("span", { class: "ic in" }, "$"), h("div", { class: "mid" }, h("div", {}, "DigiDollar"), h("div", { class: "s" }, `${W.dd.coins.length} coin${W.dd.coins.length === 1 ? "" : "s"} · ${W.dd.address}`)), h("span", { class: "amt" }, usd(W.dd.cents))));
    for (const t of v.tokens || [])
      list.push(h("div", { class: "wl-row click", onclick: () => openExplorer("token", t.tick) }, h("span", { class: "ic" }, (t.tick || "?")[0].toUpperCase()),
        h("div", { class: "mid" }, h("div", {}, t.tick), h("div", { class: "s" }, t.transferable !== "0" ? `${units(t.transferable, t.dec)} transferable` : "")),
        h("span", { class: "amt" }, units(t.overall, t.dec))));
    // Arcade tokens (the arcade ledger): kept here, or from an arcade on the mesh that keeps it.
    for (const t of v.ledger_tokens || [])
      list.push(h("div", { class: "wl-row click", onclick: () => openExplorer("ltoken", String(t.propertyid)) }, h("span", { class: "ic" }, (t.name || "#")[0].toUpperCase()),
        h("div", { class: "mid" }, h("div", {}, t.name || `Arcade token #${t.propertyid}`), h("div", { class: "s" }, `arcade token #${t.propertyid}`)),
        h("span", { class: "amt" }, t.balance)));
    // Chain tokens (PRC-20, DRC-20...): from the tokens index here, else from an arcade keeping it.
    const none = !v.tokens && !v.ledger_tokens;
    const note = v.tokens ? "" : `${ch.name}'s chain tokens aren't shown: no arcade keeping its tokens index answered (add the index in Chains & Coins to keep it here).`;
    pane.replaceChildren(list.length ? h("div", { class: "wl-rows" }, list) : h("div", { class: "x-empty" }, none ? note : "No tokens in this wallet."),
      note && !none ? h("p", { class: "note" }, note) : "");
  } else {
    const ins = v.inscriptions;
    if (!ins) { pane.replaceChildren(h("div", { class: "x-empty" }, `No NFTs shown: ${ch.name} needs its ord index here (Chains & Coins).`)); return; }
    if (!ins.length) { pane.replaceChildren(h("div", { class: "x-empty" }, "No NFTs in this wallet.")); return; }
    pane.replaceChildren(
      v.inscription_count > ins.length ? h("p", { class: "note" }, `Showing ${ins.length} of ${num(v.inscription_count)}.`) : "",
      h("div", { class: "x-grid" }, ins.map((i) => {
        const c = i.content || {}, t = h("div", { class: "x-thumb" });
        if (c.kind === "image") t.append(h("img", { src: c.data, alt: `inscription ${i.n}`, loading: "lazy" }));
        else if (c.kind === "text") t.append(h("pre", {}, c.text.slice(0, 400)));
        else t.append(h("div", { class: "x-ph" }, c.kind === "html" ? "HTML" : c.kind === "missing" ? "content not stored" : (c.type || "binary")));
        // Send (2026-11-12, ord.send): from this wallet's own address 0, the coin whole to the recipient.
        const sendable = w && w.own && w.index === 0;
        const send = sendable ? h("button", { class: "small", onclick: (e) => { e.stopPropagation(); pickOrd(i); } }, "Send") : "";
        return h("div", { class: "x-ins", onclick: () => openExplorer("inscription", i.id) }, t, h("div", { class: "cap" }, h("b", {}, `#${num(i.n)}`), " ", send));
      })));
  }
}

function copyText(text, btn) {
  navigator.clipboard.writeText(text).then(() => { const o = btn.textContent; btn.textContent = "Copied"; setTimeout(() => { btn.textContent = o; }, 1200); }).catch(() => {});
}

async function refreshMine() {
  const ch = picked();
  const kot = ch.key === "kot";
  $("k-body").hidden = !kot;
  $("m-note").hidden = ch.home || kot; $("m-body").hidden = !ch.home;
  $("m-note").textContent = ch.home || kot ? "" : `Mining here is for Pepecoin testnet only, and staking for Kotia. ${ch.name} is mined by its own miners and pools.`;
  document.querySelector("#mine h1").textContent = kot ? "Stake Kotia" : "Mine testnet";
  if (kot) return refreshStake();
  if (!ch.home) return;
  const m = await api("mine.status");
  $("m-state").textContent = m.running ? `mining (${m.threads} threads)` : "stopped";
  $("m-rate").textContent = m.hash_rate ? Math.round(m.hash_rate).toLocaleString() + " H/s" : "—";
  $("m-found").textContent = m.found;
}

async function refreshStake() {
  const k = await api("stake.status");
  const kot = (v) => (v / 1e8).toLocaleString(undefined, { maximumFractionDigits: 4 }) + " KOT";
  $("k-state").textContent = !k.installed ? "Kotia not installed" : k.on ? "on" : "off";
  $("k-coins").textContent = k.coins ? `${kot(k.coins.mature)} ready` : "—";
  $("k-found").textContent = String((k.staked || []).length);
  $("k-addr").textContent = k.address ? `Staking address: ${k.address}${k.coins && k.coins.total > k.coins.mature ? ` · ${kot(k.coins.total - k.coins.mature)} still maturing (20 confirmations)` : ""}. Send KOT here to stake it.` : "";
  const c = k.self_check;
  $("k-check").textContent = !k.installed ? "Install Kotia in full (Chains & Coins) to stake: staking needs the whole chain on this PC."
    : (k.state ? k.state + ". " : "") + (c ? (c.ok ? `Stake rules checked against the newest ${c.checked} blocks: all match.` : c.why ? c.why : `Stake rules: ${c.passed} of ${c.checked} newest blocks match; staking waits until all do.`) : "");
  $("k-start").disabled = !k.installed || k.on; $("k-stop").disabled = !k.on;
  $("k-list").replaceChildren(...(k.staked || []).slice().reverse().map((b) => h("li", {}, `Block ${num(b.height)} · ${new Date(b.at * 1000).toLocaleString()} · ${b.block.slice(0, 16)}…`)));
}

async function refreshActivity() {
  const ch = picked();
  const all = await api("events", { n: 300 });
  // This chain's events: those naming it; on the home chain also the wallet, tags, faucet, mining and mesh.
  const names = ["Pepecoin testnet", "Pepecoin", "Dogecoin", "DigiByte", "Kotia", "Flopcoin", "Litecoin", "Bitcoin", "Wojak"];
  const mentions = (t) => names.filter((n) => t.includes(n));
  const ev = all.filter((e) => {
    const m = mentions(e.text);
    if (ch.home) return m.length === 0 || m.includes("Pepecoin testnet");
    return m.some((n) => n === ch.name || (ch.name.startsWith(n) && !m.includes("Pepecoin testnet")));
  }).slice(0, 60);
  if (!ev.length) { $("a-list").replaceChildren(Object.assign(document.createElement("li"), { textContent: `Nothing about ${ch.name} yet.` })); return; }
  $("a-list").replaceChildren(...ev.map((e) => {
    const li = document.createElement("li");
    const t = document.createElement("time"); t.textContent = new Date(e.at * 1000).toLocaleTimeString();
    const k = document.createElement("span"); k.className = "kind"; k.textContent = e.kind;
    li.append(t, k, document.createTextNode(e.text));
    return li;
  }));
}

async function refresh() {
  loadHome();
  try {
    await refreshHome();
    const tab = document.querySelector(".tab.on").id;
    if (tab === "wallet") await refreshWallet();
    if (tab === "mine") await refreshMine();
    if (tab === "activity") await refreshActivity();
  } catch (e) {
    $("conn").textContent = "service not running"; $("conn").className = "pill bad";
  }
}

// Wallet actions
$("w-create").addEventListener("click", async () => {
  try {
    const r = await api("wallet.create");
    $("w-phrase-words").textContent = r.phrase;
    $("w-none").hidden = true; $("w-phrase").hidden = false;
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("w-saved").addEventListener("change", (e) => { $("w-done").disabled = !e.target.checked; });
$("w-done").addEventListener("click", () => { $("w-phrase-words").textContent = ""; $("w-phrase").hidden = true; refresh(); });
$("w-restore").addEventListener("click", async () => {
  try { await api("wallet.create", { phrase: $("w-words").value }); $("w-words").value = ""; msg($("w-msg"), "Wallet restored.", "ok"); refresh(); }
  catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("w-more").addEventListener("toggle", () => { if ($("w-more").open) { drawAccounts(); drawLinks(picked()); } });
$("w-link").addEventListener("click", async () => {
  try {
    const r = await api("wallet.link", { chain: picked().key, wif: $("w-wif").value, tag: $("w-ltag").value });
    $("w-wif").value = "";
    msg($("w-msg"), `Linked ${r.a} to @${r.tag}, signed by both keys and sent to the mesh.`, "ok");
    drawLinks(picked());
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("w-signin").addEventListener("click", async () => {
  const b = $("w-signin");
  b.disabled = true; msg($("w-msg"), "Finding the arcade that holds your account and opening it (this takes a few seconds)…");
  try {
    const r = await api("account.signin", { tag: $("w-stag").value, password: $("w-spw").value, from: $("w-sfrom").value });
    $("w-spw").value = "";
    const im = r.imports || {}, mb = r.mailbox || {};
    const bits = [`signed in as @${r.account.tag} from ${r.account.from}`];
    bits.push(im.error ? `imported wallets: ${im.error}` : `${im.brought || 0} imported wallet${im.brought === 1 ? "" : "s"}${(im.failed || []).length ? `, ${im.failed.length} not brought` : ""}`);
    bits.push(mb.error ? `mailbox: ${mb.error}` : `${mb.groups || 0} groups, ${mb.book || 0} contacts, ${mb.blocked || 0} blocked`);
    msg($("w-msg"), bits.join(" · "), im.error || mb.error ? "" : "ok");
    $("w-signin-box").open = false;
    refresh();
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});

// The wallet picker; its last entry opens the import panel.
const showPanel = (id) => { for (const p of ["w-send-panel", "w-recv-panel", "w-imp-panel"]) $(p).hidden = p === id ? !$(p).hidden : true; };
$("w-pick").addEventListener("change", () => {
  const v = $("w-pick").value;
  if (v === "+import") { $("w-pick").value = W.sel; $("w-imp-panel").hidden = false; $("w-send-panel").hidden = $("w-recv-panel").hidden = true; $("w-isecret").focus(); return; }
  W.sel = v; $("w-confirm").hidden = true; pendingSend = null; msg($("w-send-msg"), "");
  drawCard(picked()); refreshWallet();
});
$("w-imp-cancel").addEventListener("click", () => { $("w-imp-panel").hidden = true; $("w-isecret").value = ""; });
$("w-import").addEventListener("click", async () => {
  try {
    const r = await api("wallet.import", { chain: picked().key, secret: $("w-isecret").value, addr_kind: $("w-ikind").value, name: $("w-iname").value });
    $("w-isecret").value = ""; $("w-iname").value = ""; $("w-imp-panel").hidden = true;
    W.sel = "imp:" + r.wallet.address;
    msg($("w-msg"), r.imported ? `Imported ${r.wallet.address}.` : `${r.wallet.address} was already here.`, "ok");
    refreshWallet();
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("w-forget").addEventListener("click", async () => {
  const b = $("w-forget"), w = cur();
  if (!w || w.own) return;
  if (b.dataset.sure !== "1") { b.dataset.sure = "1"; b.textContent = "Remove: sure? Its key is deleted here"; setTimeout(() => { b.dataset.sure = ""; b.textContent = "Remove this wallet"; }, 5000); return; }
  b.dataset.sure = ""; b.textContent = "Remove this wallet";
  try { await api("wallet.import_remove", { chain: picked().key, address: w.address }); W.sel = null; msg($("w-msg"), `Forgot ${w.address} and its key.`, "ok"); refreshWallet(); }
  catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("w-copy").addEventListener("click", (e) => copyText($("w-addr").textContent, e.target));
$("w-btn-send").addEventListener("click", () => { showPanel("w-send-panel"); if (!$("w-send-panel").hidden) $("w-to").focus(); });
$("w-btn-recv").addEventListener("click", () => showPanel("w-recv-panel"));
$("w-asset").addEventListener("change", () => { assetChanged(); $("w-confirm").hidden = true; pendingSend = null; });
document.querySelectorAll(".wl-tabs button").forEach((b) => b.addEventListener("click", () => { W.tab = b.dataset.wt; drawPane(picked()); }));

// Send: review first (fee, and a warning when the address could be meant for another chain),
// then send. The coin goes from the wallet picked; DigiDollar from this arcade's DigiByte key.
let pendingSend = null;
const toSats = (t) => {
  const m = String(t).trim().match(/^(\d+)(?:\.(\d{1,8}))?$/);
  if (!m) return null;
  return Number(m[1]) * 1e8 + Number((m[2] || "").padEnd(8, "0"));
};
$("w-review").addEventListener("click", async () => {
  const ch = picked(), w = cur(), sm = $("w-send-msg");
  const to = $("w-to").value.trim();
  if ($("w-asset").value === "dd") {
    const m = $("w-amt").value.trim().match(/^(\d+)(?:\.(\d{1,2}))?$/);
    if (!m) { msg(sm, "Enter dollars, for example 12.50", "bad"); return; }
    const cents = Number(m[1]) * 100 + Number((m[2] || "").padEnd(2, "0"));
    try {
      const r = await api("dd.send", { chain: ch.key, to, cents, send: false });
      pendingSend = { cmd: "dd.send", args: { chain: ch.key, to, cents } };
      $("w-confirm-text").replaceChildren(
        h("div", {}, `Send ${usd(r.cents)} DigiDollar to ${r.to}`),
        h("div", { class: "s" }, `network fee ${coins(r.fee, ch.key)}${r.change_cents ? ` · ${usd(r.change_cents)} back to you` : ""}`));
      $("w-confirm").hidden = false; msg(sm, "");
    } catch (e) { msg(sm, e.message, "bad"); }
    return;
  }
  const asset = $("w-asset").value;
  if (asset.startsWith("ord:")) {
    const inscription = asset.slice(4);
    try {
      const r = await api("ord.send", { chain: ch.key, inscription, to, send: false });
      pendingSend = { cmd: "ord.send", args: { chain: ch.key, inscription, to } };
      $("w-confirm-text").replaceChildren(
        h("div", {}, `Send inscription ${W.ordPick ? "#" + num(W.ordPick.n) : r.inscription} to ${r.to}`),
        h("div", { class: "s" }, `its coin (${coins(r.value, ch.key)}) goes with it · network fee ${coins(r.fee, ch.key)}`));
      $("w-confirm").hidden = false; msg(sm, "");
    } catch (e) { msg(sm, e.message, "bad"); }
    return;
  }
  if (asset.startsWith("t20:")) {
    // PRC-20 / DRC-20: a transfer note to this wallet's address, then its coin to the recipient.
    const tick = asset.slice(4), amount = $("w-amt").value.trim();
    try {
      const r = await api("token20.send", { chain: ch.key, tick, amount, to, send: false });
      pendingSend = { cmd: "token20.send", args: { chain: ch.key, tick, amount, to } };
      $("w-confirm-text").replaceChildren(
        h("div", {}, `Send ${r.amount} ${r.tick} to ${r.to}`),
        h("div", { class: "s" }, `about ${coins(r.cost, ch.key)} in fees and postage · two steps: ${r.steps}`));
      $("w-confirm").hidden = false; msg(sm, "");
    } catch (e) { msg(sm, e.message, "bad"); }
    return;
  }
  if (asset.startsWith("lt:")) {
    // An arcade token: one transaction on the home chain.
    const t = (W.tokens.ledger_tokens || []).find((x) => `lt:${x.propertyid}` === asset);
    const amt = $("w-amt").value.trim();
    const units = t && t.divisible ? toSats(amt) : (/^\d+$/.test(amt) ? Number(amt) : null);
    if (!units) { msg(sm, t && t.divisible ? "Enter an amount, for example 12.5" : "This token comes in whole units: enter a whole number", "bad"); return; }
    try {
      const r = await api("token.send", { propertyid: t.propertyid, units, to, send: false });
      pendingSend = { cmd: "token.send", args: { propertyid: t.propertyid, units, to } };
      $("w-confirm-text").replaceChildren(
        h("div", {}, `Send ${amt} ${t.name || "of arcade token #" + t.propertyid} to ${to}`),
        h("div", { class: "s" }, `fee ${coins(r.fee, ch.key)}`));
      $("w-confirm").hidden = false; msg(sm, "");
    } catch (e) { msg(sm, e.message, "bad"); }
    return;
  }
  const sats = toSats($("w-amt").value);
  if (!sats) { msg(sm, "Enter an amount, for example 12.5", "bad"); return; }
  // A DigiByte D... address is also a Dogecoin address: say so before anything else.
  const dogeLike = ch.key === "dgb" && /^D[1-9A-HJ-NP-Za-km-z]{25,34}$/.test(to);
  try {
    const args = { chain: ch.key, to, amount: sats, from: w.address };
    const r = await api("wallet.send", { ...args, send: false });
    pendingSend = { cmd: "wallet.send", args };
    $("w-confirm-text").replaceChildren(
      h("div", {}, `Pay ${coins(r.amount, ch.key)} to ${r.to}`),
      h("div", { class: "s" }, `fee ${coins(r.fee, ch.key)} · ${r.vsize || r.size} bytes · from ${r.from}`),
      r.warning ? h("p", { class: "msg bad" }, r.warning) : null);
    $("w-confirm").hidden = false;
    msg(sm, "");
  } catch (e) {
    // "not enough coins: have X, need Y" in satoshis -> in the coin.
    const m = e.message.match(/have (\d+), need (\d+)/);
    const text = m ? `Not enough ${TICKERS[ch.key] || "coins"}: you have ${coins(+m[1], ch.key)}, this needs ${coins(+m[2], ch.key)} (the amount and its fee).` : e.message;
    msg(sm, text + (dogeLike ? " Also: D... addresses look the same on DigiByte and Dogecoin, so make sure whoever you're paying uses DigiByte." : ""), "bad");
  }
});
$("w-cancel").addEventListener("click", () => { pendingSend = null; $("w-confirm").hidden = true; });
$("w-sendnow").addEventListener("click", async () => {
  if (!pendingSend) return;
  const b = $("w-sendnow"); b.disabled = true;
  try {
    const r = await api(pendingSend.cmd, { ...pendingSend.args, send: true });
    $("w-confirm").hidden = true; pendingSend = null; $("w-to").value = ""; $("w-amt").value = "";
    if (r.inscription) W.ordPick = null;
    msg($("w-send-msg"), r.id ? `Transfer note inscribed (${r.note_tx}); the tokens go once a block holds it.` : `Sent: ${r.txid}`, "ok");
    W.viewAt = 0; W.ddAt = 0; refreshWallet();
  } catch (e) { msg($("w-send-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});

// Mining
$("m-start").addEventListener("click", async () => { try { await api("mine.start"); refresh(); } catch (e) { alert(e.message); } });
$("m-stop").addEventListener("click", async () => { await api("mine.stop"); refresh(); });
$("k-start").addEventListener("click", async () => { try { await api("stake.set", { on: true }); refresh(); } catch (e) { alert(e.message); } });
$("k-stop").addEventListener("click", async () => { await api("stake.set", { on: false }); refresh(); });
$("k-recheck").addEventListener("click", async () => { await api("stake.check"); refresh(); });

refresh();
setInterval(refresh, 4000);

// Open a tab from the address (#explorer, #chains, ...), e.g. from a link or a shortcut.
(() => {
  const t = (location.hash || "").slice(1).split("/")[0];
  const b = t && document.querySelector(`nav button[data-tab="${t}"]`);
  if (b) b.click();
})();
