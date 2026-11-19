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
  $("w-loading").hidden = true;
  if (!$("home").classList.contains("on")) return;
  // Side by side: the mesh doesn't wait for the getting-started card.
  await Promise.all([drawStart(), api("mesh.overview").then(drawMesh)]);
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
  // The chain the @handle is claimed on (ARCADE when it runs here; docs/HOMECHAIN.md phase 3) and its coin.
  const coin = (st.chain && st.chain.unit) || homeTicker();
  const chainName = (st.chain && st.chain.name) || HOME.name;
  const onArcade = !!st.chain && st.chain.key !== HOME.key;
  const need = onArcade ? "0.01" : "1";
  const walletDone = st.wallet && !START.phrase;
  const coinsHere = st.balance > 0;
  // Redrawn only when something a step shows changes (typing must survive the 4 s refresh);
  // the "asked … ago" line is updated in place.
  const sig = JSON.stringify([st.wallet, !!START.phrase, START.saved, coinsHere, !!st.faucet, st.faucet && st.faucet.status, c && [c.tag, c.status, c.note], failed && failed.tag, START.checked, START.busy, START.msg]);
  // A fresh install reads the chain first; a coin paid in a recent block shows once it's there.
  const catchUp = () => st.height != null && st.best != null && st.best > st.height + 2
    ? `Your arcade is still reading the ${chainName} chain (block ${num(st.height)} of ${num(st.best)}, ${Math.floor(st.height * 100 / st.best)}%); the coin shows once it gets there.`
    : "";
  const used = !!c && (c.status === "sent" || c.status === "confirmed");
  const askedText = () => !st.wallet ? `It's asked for by itself as soon as your wallet exists.`
    : used ? `Received, and used to publish your @handle.`
    : coinsHere ? `Here: ${coinNum(st.balance)} ${coin}. That pays for publishing your @handle.`
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
    !coinsHere && !used ? h("div", { class: "hint" }, `Why: your @handle is published on the ${chainName} chain, and publishing pays a small network fee. ${coin} is the test coin of that chain; it has no money value.`) : null];

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
      START.msg ? h("p", { class: "msg " + (START.checked ? "ok" : "bad") }, START.msg) : null,
      catchUp() ? h("div", { class: "hint" }, "Restored a wallet that already has a name? It comes back here by itself once it's found on the chain; no need to claim a new one.") : null];
  }

  const steps = [
    { done: walletDone, title: START.phrase ? "Your 12 words" : st.wallet ? "Your wallet" : "Make your wallet, or restore yours", body: s1 },
    { done: coinsHere || used, title: coinsHere || used ? `${need} ${coin} for your @handle` : `Waiting for ${need} ${coin}`, body: s2 },
    { done: false, title: "Claim your @handle", body: s3 },
  ];
  $("h-steps").replaceChildren(...steps.map((x, i) => h("li", { class: x.done ? "done" : "" },
    h("span", { class: "n" }, x.done ? "✓" : String(i + 1)),
    h("div", { class: "start-body" }, h("b", {}, x.title), ...x.body))));
}

// Restore a wallet from its words right on Home (the same as the Wallet tab's Restore).
let restoring = false;
function restoreBox() {
  const ta = h("textarea", { rows: "2", placeholder: "or: your 12 words, separated by spaces", autocomplete: "off", spellcheck: "false", autocapitalize: "off", autocorrect: "off", class: "start-words" });
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
  const web = IN_BROWSER ? h("p", { class: "hint" }, "In a browser, your words go through your tunnel to this arcade only. Do it on a computer you trust: browser extensions can read what a page shows.") : null;
  return h("div", { class: "start-restore" }, ta, web, h("div", { class: "row" }, b), m);
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

// The accounts table (Wallet → Accounts, and Options); redrawn only when the list changes.
async function drawAccounts(tb = $("w-accs"), m = $("w-msg"), force = true) {
  const accs = await api("account.list").catch(() => []);
  const sig = JSON.stringify(accs);
  if (!force && tb.dataset.sig === sig) return;
  tb.dataset.sig = sig;
  tb.replaceChildren(...accs.map((a) => {
    const tr = document.createElement("tr");
    const name = (a.tag ? "@" + a.tag : (a.own ? "This arcade's wallet" : "Another account")) + (a.active ? " (in use)" : "");
    for (const t of [name, a.address || (a.own ? "this PC" : ""), a.own ? "" : (a.from || "")]) {
      const td = document.createElement("td"); td.textContent = t; tr.append(td);
    }
    const td = document.createElement("td");
    if (!a.active) {
      const u = document.createElement("button"); u.textContent = "Use";
      u.addEventListener("click", async () => {
        try { await api("account.switch", { id: a.id }); msg(m, `Now using ${name.replace(" (in use)", "")}.`, "ok"); refresh(); }
        catch (e) { msg(m, e.message, "bad"); }
      });
      td.append(u);
    }
    if (!a.own) {
      const b = document.createElement("button"); b.textContent = "Sign out";
      b.addEventListener("click", async () => {
        if (b.dataset.sure !== "1") { b.dataset.sure = "1"; b.textContent = "Sign out: sure?"; return; }
        try { await api("account.signout", { id: a.id }); msg(m, `Signed out; its words, keys and messages are gone from this arcade.`, "ok"); refresh(); }
        catch (e) { msg(m, e.message, "bad"); }
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
  if (W.tab === "functions") { drawAccounts(); drawLinks(ch); }
  if (!state.wallet) return;
  if (W.chain !== ch.key) {
    Object.assign(W, { chain: ch.key, sel: null, view: null, viewKey: "", dd: null, ddAt: 0 });
    for (const p of ["w-send-panel", "w-recv-panel", "w-imp-panel"]) $(p).hidden = true;
    $("w-confirm").hidden = true; msg($("w-send-msg"), "");
  }
  const [keys, imps, st] = await Promise.all([api("wallet.keys", { chain: ch.key }), api("wallet.imports").catch(() => []), api("chains.status", { key: ch.key }).catch(() => null)]);
  state.keys = keys;
  // The chain's status, for its trust dot (drawCard runs again without asking for it).
  W.st = st; W.stKey = ch.key;
  W.list = [
    ...keys.map((k) => {
      // Address types (SegWit chains): the one picked (remembered per chain) is the address shown, received at and sent from.
      const kinds = k.kinds || [], ak = kinds.find((x) => x.id === k.kind) || kinds[0];
      return { id: "own:" + k.index, own: true, index: k.index, address: ak ? ak.address : k.address, legacy: ak ? ak.legacy : k.legacy, tag: k.tag,
        balance: k.balance, pending: k.pending || 0, kinds, akind: ak || null,
        confirmed: k.confirmed, free: k.free, pendingCoins: k.pending_coins || 0, waiting: k.waiting || [], dropped: k.dropped || [],
        label: k.tag ? "@" + k.tag : k.index ? `This arcade's wallet · address ${k.index}` : "This arcade's wallet" };
    }),
    ...imps.filter((w) => w.chain === ch.key).map((w) => ({ id: "imp:" + w.address, own: false, address: w.address, balance: w.balance, name: w.name,
      kind: w.kind, addr_kind: w.addr_kind, spendable: w.spendable_here !== false, kinds: [],
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
  // Waiting transactions changed: Activity shows them now, and one that left the pool (into a
  // block, or dropped) has its block row read again.
  const wsig = key + JSON.stringify((w && w.waiting) || []) + JSON.stringify((w && w.dropped) || []);
  if (wsig !== W.waitSig) {
    const left = W.waitSig && W.waitSig.startsWith(key + "[") && (W.waitList || []).some((t) => !((w && w.waiting) || []).some((x) => x.txid === t.txid));
    W.waitSig = wsig; W.waitList = (w && w.waiting) || [];
    if (left) W.viewAt = 0; else if (key === W.viewKey) drawPane(ch);
  }
  if (w && (key !== W.viewKey || Date.now() - W.viewAt > 30000)) {
    if (key !== W.viewKey) { W.view = null; drawPane(ch); }
    W.viewKey = key; W.viewAt = Date.now();
    const [view, ...others] = await Promise.all([w.address, ...(w.own ? w.kinds.filter((k) => k.address !== w.address).map((k) => k.address) : [])]
      .map((a) => api("explorer.address", { chain: ch.key, address: a }).catch((e) => ({ error: e.message }))));
    // Activity and NFTs of every address type of this wallet, in one list (tokens: the address shown).
    for (const o of others) {
      if (!o || o.error) continue;
      if (o.history) view.history = (view.history || []).concat(o.history);
      if (o.inscriptions) {
        const seen = new Set((view.inscriptions || []).map((i) => i.id));
        view.inscriptions = (view.inscriptions || []).concat(o.inscriptions.filter((i) => !seen.has(i.id)).map((i) => ({ ...i, other: true })));
        view.inscription_count = (view.inscription_count || 0) + (o.inscription_count || 0);
      }
    }
    if (view && view.history && others.length) view.history.sort((a, b) => (b.height || 1e12) - (a.height || 1e12));
    W.view = view;
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
  // A borrowed chain: its trust colour beside the balance (red adds its note below), from the
  // status refreshWallet loaded. Kept here: a plain green dot; borrowed: its colour and count.
  const st = W.stKey === ch.key ? W.st : null;
  $("w-trust").replaceChildren();
  if (st && !st.borrowed && (st.installed || st.home)) $("w-trust").replaceChildren(trustDot("green", `${ch.name} is kept on this arcade: every block is checked here.`, null, true));
  else if (st && st.borrowed && st.trust) $("w-trust").replaceChildren(trustDot(st.trust, st.trust_note, st.trust_hosts));
  const ddOn = w.own && w.index === 0 && isDD(ch.key) && W.dd && !W.dd.error;
  const sub = [];
  if (st && st.borrowed && st.trust === "red" && st.trust_note) sub.push(st.trust_note);
  if (w.balance == null) sub.push(`${ch.name} isn't installed or borrowed with addresses here: add it in Chains & Coins to see balances.`);
  // The balance follows the mempool (wallet.keys): the blocks' figure beside it when they differ.
  if (w.confirmed != null && w.confirmed !== w.balance) sub.push(`${coinNum(w.confirmed)} ${tk} confirmed`);
  if (w.pending > 0) sub.push(`${coinNum(w.pending)} ${tk} of it arriving (in the mempool)`);
  if (w.own && w.free === 0 && w.pendingCoins > 0)
    sub.push(`every confirmed coin is spent by a waiting transaction: waiting for ${w.pendingCoins} coin${w.pendingCoins === 1 ? "" : "s"} to confirm before you can post`);
  if (ddOn) sub.push(`${usd(W.dd.cents)} DigiDollar`);
  if (!w.own && !w.spendable) sub.push("This key can't send from here (an uncompressed key's SegWit address).");
  // Address types: the balance counts them all; say what the type picked holds when that's different.
  const ak = w.own && w.kinds.length > 1 ? w.akind : null;
  if (ak && ak.balance != null && w.balance != null && ak.balance !== w.balance) sub.push(`${ak.label}: ${coinNum(ak.balance)} ${tk} of it`);
  $("w-sub").textContent = sub.join(" · ");
  $("w-kind-row").hidden = !ak;
  if (ak) {
    const ksig = JSON.stringify(w.kinds.map((k) => [k.id, k.label, k.balance]));
    if (ksig !== W.kindSig) {
      W.kindSig = ksig;
      $("w-kind").replaceChildren(...w.kinds.map((k) => h("option", { value: k.id }, k.balance ? `${k.label} · ${coinNum(k.balance)}` : k.label)));
    }
    $("w-kind").value = ak.id;
  }
  // Send says which address type it spends from; the other types' coins only when asked.
  const others = ak ? w.kinds.filter((k) => k.id !== ak.id && k.balance > 0) : [];
  $("w-from").hidden = !ak;
  if (ak) $("w-from").textContent = `Sends from your ${ak.label} address ${shortAddr(ak.address)} (${ak.balance == null ? "—" : coinNum(ak.balance)} ${tk}); change comes back to it.`;
  $("w-allkinds-row").hidden = !others.length;
  if (!others.length) $("w-allkinds").checked = false;
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
  const rows = [[ak ? `${tk} address · ${ak.label} (${ak.path})` : `${tk} address`, w.address]];
  if (ddOn) rows.push(["DigiDollar address", W.dd.address]);
  $("w-recv-list").replaceChildren(...[...rows.map(([l, a]) => h("div", { class: "wl-recv" }, h("span", { class: "lbl" }, l),
      h("div", {}, h("code", {}, a), " ", h("button", { class: "small", onclick: (e) => copyText(a, e.target) }, "Copy")))),
    // DigiByte's Legacy address is the D... form, the very same string as on Dogecoin (same key, same version byte).
    /^D/.test(w.address || "") && ch.key === "dgb" ? h("p", { class: "note warn" }, "This address is the same on Dogecoin and DigiByte. Check the chain before sending.") : null,
    w.legacy ? h("p", { class: "note" }, /^D/.test(w.address || "")
      ? `Coins sent to ${w.legacy} (the SegWit form of the same key) are counted too.`
      : `Coins sent to ${w.legacy} (the older D... form of the same key) are counted too. Give people the dgb1... address: D... addresses look the same on Dogecoin.`) : null,
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
  // Functions: ARCADE ID, accounts, sign-in and linked wallets (not a list of this wallet's rows).
  const fn = W.tab === "functions";
  $("w-functions").hidden = !fn; $("w-pane").hidden = fn;
  if (fn) { $("w-note").hidden = true; drawAccounts(); drawLinks(ch); aidSites(); return; }
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
    // This wallet's waiting transactions first (wallet.keys), unconfirmed until a block has them.
    const waiting = ((w && w.own && w.waiting) || []).filter((x) => !by.has(x.txid))
      .map((x) => ({ txid: x.txid, net: (x.received || 0) - (x.spent || 0), waiting: true }));
    // Own ones that left the mempool without a block (ownwait.rs): they didn't go through.
    const dropped = ((w && w.own && w.dropped) || []).filter((x) => !by.has(x.txid))
      .map((x) => ({ txid: x.txid, net: (x.received || 0) - (x.spent || 0), dropped: true }));
    const txs = [...waiting, ...dropped, ...by.values()];
    const where = (t) => t.waiting ? "unconfirmed (in the mempool)" : t.dropped ? "didn't go through: it left the mempool without a block, and its coins are free again" : "block " + num(t.height);
    pane.replaceChildren(txs.length ? h("div", { class: "wl-rows" }, txs.map((t) => h("div", { class: "wl-row" + (t.dropped ? "" : " click"), onclick: t.dropped ? null : () => openExplorer("tx", t.txid) },
      h("span", { class: "ic " + (t.dropped ? "" : t.net >= 0 ? "in" : "out") }, t.dropped ? "×" : t.net >= 0 ? "↓" : "↑"),
      h("div", { class: "mid" }, h("div", {}, (t.net >= 0 ? "Received" : "Sent") + (t.dropped ? " (didn't go through)" : "")), h("div", { class: "s" }, `${where(t)} · ${t.txid}`)),
      h("span", { class: "amt " + (t.net >= 0 && !t.dropped ? "in" : "") }, t.dropped ? h("s", {}, (t.net >= 0 ? "+" : "−") + coins(Math.abs(t.net), ch.key)) : (t.net >= 0 ? "+" : "−") + coins(Math.abs(t.net), ch.key)))))
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
        // (Legacy coins only: ord.send signs with the Legacy key.)
        const sendable = w && w.own && w.index === 0 && !i.other && (!w.akind || w.akind.id === "legacy");
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

// One refresh at a time: a tick while one is out runs once after it instead of piling up.
let refreshing = null, refreshAgain = false;
function refresh() {
  if (refreshing) { refreshAgain = true; return refreshing; }
  refreshing = refreshOnce().finally(() => {
    refreshing = null;
    if (refreshAgain) { refreshAgain = false; refresh(); }
  });
  return refreshing;
}

async function refreshOnce() {
  loadHome();
  try {
    await refreshHome();
    const tab = document.querySelector(".tab.on").id;
    if (tab === "wallet") await refreshWallet();
    if (tab === "mine") await refreshMine();
    if (tab === "activity") await refreshActivity();
    if (tab === "options") { await refreshOptions(); refreshWebUi(); drawAccounts($("o-accs"), $("o-acc-msg"), false); }
  } catch (e) {
    $("conn").textContent = e && e.starting ? "starting the service…" : "service not running"; $("conn").className = "pill bad";
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
  try { const r = await api("wallet.create", { phrase: $("w-words").value }); $("w-words").value = ""; msg($("w-msg"), r && r.tag ? `Wallet restored: you're @${r.tag} again.` : "Wallet restored. If it had a name, it shows here once this arcade or another can read it from the chain.", "ok"); refresh(); }
  catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("o-add").addEventListener("click", async () => {
  const b = $("o-add"), m = $("o-acc-msg");
  if (b.disabled) return;
  b.disabled = true; msg(m, "Bringing in the account…");
  try {
    const r = await api("account.add", { phrase: $("o-words").value.trim().toLowerCase().split(/\s+/).join(" ") });
    $("o-words").value = "";
    const who = r.account.tag ? `@${r.account.tag}` : r.own ? "this arcade's wallet" : r.account.address;
    msg(m, !r.added ? `That account is already here: now using ${who}.`
      : r.own ? `Restored as this arcade's wallet${r.account.tag ? `: you're @${r.account.tag} again` : ""}. Addresses and coins show up on every chain as it is read.`
      : `Added ${who} and using it now. Its addresses and coins show up on every chain as it is read${r.account.tag ? "" : "; its @handle too, once found on the chain"}.`, "ok");
    drawStart.sig = null;
    await drawAccounts($("o-accs"), m);
    refresh();
  } catch (e) { msg(m, e.message, "bad"); }
  finally { b.disabled = false; }
});
$("w-link").addEventListener("click", async () => {
  try {
    const r = await api("wallet.link", { chain: picked().key, wif: $("w-wif").value, tag: $("w-ltag").value });
    $("w-wif").value = "";
    msg($("w-msg"), `Linked ${r.a} to @${r.tag}, signed by both keys and sent to the mesh.`, "ok");
    drawLinks(picked());
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
});
// Your own addresses on your @tag's profile (profile.publish_addresses): shown with the fee first, sent on "Publish now".
$("w-pubaddr").addEventListener("click", async () => {
  const b = $("w-pubaddr"); b.disabled = true;
  try {
    const r = await api("profile.publish_addresses", { send: false });
    $("w-pubaddr-text").replaceChildren(
      h("div", {}, `List these on @${r.tag}'s profile:`),
      ...r.addresses.map((a) => h("div", { class: "s" }, `${a.name}: ${a.address}${a.listed ? " (already listed)" : ""}`)),
      h("div", { class: "s" }, `fee ${coins(r.fee, r.chain)} · costs ${coins(r.total, r.chain)} in all · ${r.size} bytes · from ${r.address}`),
      r.new ? null : h("p", { class: "msg" }, "Your profile already lists all of these."));
    $("w-pubaddr-send").disabled = !r.new;
    $("w-pubaddr-confirm").hidden = false;
    msg($("w-msg"), "");
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});
$("w-pubaddr-cancel").addEventListener("click", () => { $("w-pubaddr-confirm").hidden = true; });
$("w-pubaddr-send").addEventListener("click", async () => {
  const b = $("w-pubaddr-send"); b.disabled = true;
  try {
    const r = await api("profile.publish_addresses", { send: true });
    $("w-pubaddr-confirm").hidden = true;
    msg($("w-msg"), `Published your addresses on @${r.tag}'s profile (${r.txid}); the Explorer lists them once a block holds it.`, "ok");
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});
// ARCADE ID (Functions tab, and the ARCADE ID button by the wallet picker, which shows the same panel over the page).
// A code pasted or scanned: digiid:// signs in to that site (id.answer), arcadelink:// opens this account in a new
// browser (id.link_answer), arcadesign:// pairs a site that will ask this app to sign (id.sign_pair). Each shows what
// it would do first (no send), and goes on only on a second click. Paired sites' requests wait in id.sign_pending,
// each shown with what it does, signed or refused with id.sign_decide.
const aidKind = () => { const c = $("w-aid-code").value.trim(); return c.startsWith("arcadelink://") ? "link" : c.startsWith("arcadesign://") ? "sign" : "digiid"; };
const aidDoge = (k) => (k == null ? "?" : (Number(k) / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 })) + " DOGE";
$("w-aid-code").addEventListener("input", () => { $("w-aid-chain").hidden = aidKind() !== "digiid"; $("w-aid-confirm").hidden = true; });
$("w-aid-check").addEventListener("click", async () => {
  const b = $("w-aid-check");
  b.disabled = true; $("w-aid-confirm").hidden = true; msg($("w-aid-msg"), "");
  try {
    const kind = aidKind(), code = $("w-aid-code").value;
    if (kind === "link") {
      const r = await api("id.link_answer", { qr: code });
      $("w-aid-text").textContent = `Open ${r.tag ? "@" + r.tag : "this account"} in a new browser on ${r.host}? Only if that screen shows ${r.number}. Anyone you send this to can spend this account's coins, so only send it to your own browser.`;
      $("w-aid-send").textContent = `The number is ${r.number}: send`;
    } else if (kind === "sign") {
      const r = await api("id.sign_pair", { qr: code });
      $("w-aid-text").textContent = `Let ${r.host} ask this app to sign, as ${r.tag ? "@" + r.tag : "this account"} (Dogecoin ${r.address})? Only if that screen shows ${r.number}. Your words stay here: the site only gets your address, and you approve every signature in this app.`;
      $("w-aid-send").textContent = `The number is ${r.number}: pair`;
    } else {
      const r = await api("id.answer", { code, chain: $("w-aid-chain").value });
      $("w-aid-text").textContent = `Sign in to ${r.host} as ${r.address}${r.plain_http ? " (a test address over plain http)" : ""}? Only go on if ${r.host} is the site you're signing in to.`;
      $("w-aid-send").textContent = `Sign in to ${r.host}`;
    }
    $("w-aid-confirm").hidden = false;
  } catch (e) { msg($("w-aid-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});
$("w-aid-cancel").addEventListener("click", () => { $("w-aid-confirm").hidden = true; });
$("w-aid-send").addEventListener("click", async () => {
  const b = $("w-aid-send");
  const kind = aidKind();
  b.disabled = true; msg($("w-aid-msg"), kind === "link" ? "Sending…" : kind === "sign" ? "Pairing…" : "Signing in…");
  try {
    const code = $("w-aid-code").value;
    if (kind === "link") {
      const r = await api("id.link_answer", { qr: code, send: true });
      msg($("w-aid-msg"), `Sent: ${r.tag ? "@" + r.tag : "your account"} opens in the browser on ${r.host}.`, "ok");
    } else if (kind === "sign") {
      const r = await api("id.sign_pair", { qr: code, send: true });
      msg($("w-aid-msg"), `Paired with ${r.host}. When it asks you to sign, the request shows up here.`, "ok");
      aidSites();
    } else {
      const r = await api("id.answer", { code, chain: $("w-aid-chain").value, send: true });
      msg($("w-aid-msg"), `Signed in to ${r.host}: the site's page continues by itself.`, "ok");
    }
    $("w-aid-confirm").hidden = true; $("w-aid-code").value = ""; $("w-aid-chain").hidden = false;
  } catch (e) { msg($("w-aid-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});

// The camera: this device's (desktop, laptop, or a phone opening this window over the home network).
let aidStream = null, aidScanning = false, aidJsqr = null;
function aidCamStop() { aidScanning = false; if (aidStream) aidStream.getTracks().forEach((t) => t.stop()); aidStream = null; $("aid-cam").hidden = true; }
function aidLoadJsqr() {
  return aidJsqr || (aidJsqr = new Promise((ok, no) => { const s = document.createElement("script"); s.src = "jsqr.js"; s.onload = () => ok(window.jsQR); s.onerror = () => no(new Error("the QR reader didn't load")); document.head.append(s); }));
}
$("aid-cam-stop").addEventListener("click", aidCamStop);
$("w-aid-scan").addEventListener("click", async () => {
  msg($("w-aid-msg"), "");
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) { msg($("w-aid-msg"), "This window can't use a camera here: paste the code instead (a camera needs https or this computer's own address).", "bad"); return; }
  try {
    aidStream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" }, audio: false });
  } catch (e) { msg($("w-aid-msg"), "The camera wasn't allowed or isn't there: " + (e.message || e.name), "bad"); return; }
  const v = $("aid-video"); v.srcObject = aidStream; $("aid-cam").hidden = false; await v.play().catch(() => {});
  const detector = "BarcodeDetector" in window ? new window.BarcodeDetector({ formats: ["qr_code"] }) : null;
  const decode = detector ? null : await aidLoadJsqr().catch(() => null);
  const canvas = document.createElement("canvas"), ctx = canvas.getContext("2d", { willReadFrequently: true });
  aidScanning = true;
  const tick = async () => {
    if (!aidScanning) return;
    let text = "";
    try {
      if (v.videoWidth) {
        if (detector) { const got = await detector.detect(v); text = got[0] ? got[0].rawValue : ""; }
        else if (decode) { canvas.width = v.videoWidth; canvas.height = v.videoHeight; ctx.drawImage(v, 0, 0); const img = ctx.getImageData(0, 0, canvas.width, canvas.height); const got = decode(img.data, img.width, img.height); text = got ? got.data : ""; }
      }
    } catch (e) { /* the next frame */ }
    if (/^(digiid|arcadelink|arcadesign):\/\//.test(text)) { aidCamStop(); $("w-aid-code").value = text; $("w-aid-code").dispatchEvent(new Event("input")); $("w-aid-check").click(); return; }
    setTimeout(tick, 250);
  };
  tick();
});

// What paired sites ask: one card each, with what it does, and Sign / Refuse.
const aidShown = new Set();
function aidCard(r) {
  const head = h("div", { class: "aid-req-head" }, h("b", {}, r.site), ` asks ${r.account ? "@" + r.account : "this account"} to sign`);
  const body = [];
  if (r.kind === "message") {
    body.push(h("div", { class: "aid-sub" }, "A message (it proves you hold this address; it moves no coins):"), h("pre", { class: "aid-msgtext" }, r.message));
  } else {
    const net = Number(r.to_you || 0) - Number(r.from_you || 0);
    body.push(h("div", {}, `Spends ${aidDoge(r.from_you)} of your coins; ${aidDoge(r.to_you)} comes back to you; fee ${r.fee == null ? "unknown (not every coin's transaction is given)" : aidDoge(r.fee)}. You ${net < 0 ? "pay " + aidDoge(-net) : "receive " + aidDoge(net)} in all.`));
    if (r.inscriptions) body.push(h("div", { class: "msg bad" }, `${r.inscriptions} of the coins you sign carries an inscription (or no arcade could tell it doesn't). Only go on if this is meant to move or list it: a name or Doginal sent by mistake can't be taken back.`));
    const rows = r.outputs.map((o) => h("tr", {}, h("td", {}, String(o.index)), h("td", { class: "mono" }, o.to + (o.mine ? " (you)" : "")), h("td", { class: "num" }, aidDoge(o.value))));
    body.push(h("table", { class: "aid-outs" }, h("thead", {}, h("tr", {}, h("th", {}, "#"), h("th", {}, "Pays to"), h("th", { class: "num" }, "Amount"))), h("tbody", {}, rows)));
    const signs = r.inputs.filter((i) => i.sign).map((i) => `#${i.index} ${aidDoge(i.value)} (${i.hash_type})`).join(", ");
    body.push(h("div", { class: "aid-sub" }, `Signs your coin${r.inputs.filter((i) => i.sign).length === 1 ? "" : "s"} ${signs}.${r.finalize ? "" : " The site finishes the transaction."}`));
  }
  const out = h("p", { class: "msg" });
  const go = (ok) => async (ev) => {
    ev.target.disabled = true;
    try { await api("id.sign_decide", { id: r.id, ok }); msg(out, ok ? "Signed and sent to the site." : "Refused: the site was told no.", ok ? "ok" : ""); setTimeout(aidPending, 1500); }
    catch (e) { msg(out, e.message, "bad"); ev.target.disabled = false; }
  };
  return h("div", { class: "aid-req warn" }, head, body, h("div", { class: "row" }, h("button", { class: "primary", onclick: go(true) }, "Sign"), h("button", { onclick: go(false) }, "Refuse")), out);
}
async function aidPending() {
  let list = [];
  try { list = await api("id.sign_pending"); } catch (e) { return; }
  $("w-aid-dot").hidden = !list.length;
  const box = $("aid-pending");
  const ids = list.map((r) => r.id).join(",");
  if (box.dataset.ids !== ids) { box.dataset.ids = ids; box.replaceChildren(...list.map(aidCard)); }
  // A new request opens the ARCADE ID panel by itself.
  if (list.some((r) => !aidShown.has(r.id))) { list.forEach((r) => aidShown.add(r.id)); if ($("aid-modal").hidden && W.tab !== "functions") aidOpen(); }
}
async function aidSites() {
  let list = [];
  try { list = await api("id.sign_sites"); } catch (e) { return; }
  const box = $("aid-sites");
  if (!list.length) { box.replaceChildren("None yet."); return; }
  box.replaceChildren(...list.map((s) => h("div", { class: "row" }, h("span", {}, `${s.host} · ${s.tag ? "@" + s.tag : s.address}`),
    h("button", { class: "small", onclick: async () => { await api("id.sign_forget", { id: s.id }).catch(() => {}); aidSites(); } }, "Unpair"))));
}
function aidOpen() {
  $("aid-slot").append($("aid-panel"));
  $("aid-modal").hidden = false; aidSites();
}
function aidClose() { aidCamStop(); $("aid-home").append($("aid-panel")); $("aid-modal").hidden = true; }
$("w-aid-open").addEventListener("click", aidOpen);
$("aid-close").addEventListener("click", aidClose);
$("aid-modal").addEventListener("click", (e) => { if (e.target === $("aid-modal")) aidClose(); });
setInterval(aidPending, 3000);
aidPending();

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
// The address type: remembered per chain by the arcade (so the desktop app and the browser agree).
$("w-kind").addEventListener("change", async () => {
  const ch = picked(), kind = $("w-kind").value;
  $("w-confirm").hidden = true; pendingSend = null; msg($("w-send-msg"), "");
  try { await api("wallet.addr_kind", { chain: ch.key, kind }); } catch (e) { msg($("w-msg"), e.message, "bad"); }
  W.viewKey = ""; refreshWallet();
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
$("w-allkinds").addEventListener("change", () => { $("w-confirm").hidden = true; pendingSend = null; });
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
    if (w.own && w.akind && w.kinds.length > 1) Object.assign(args, { kind: w.akind.id, all_kinds: $("w-allkinds").checked });
    const r = await api("wallet.send", { ...args, send: false });
    pendingSend = { cmd: "wallet.send", args };
    const spent = r.kinds_spent || [];
    $("w-confirm-text").replaceChildren(
      h("div", {}, `Pay ${coins(r.amount, ch.key)} to ${r.to}`),
      h("div", { class: "s" }, `fee ${coins(r.fee, ch.key)} · ${r.vsize || r.size} bytes · from ${r.from}${r.kind_label ? ` (${r.kind_label})` : ""}`),
      spent.length > 1 || (spent.length === 1 && spent[0] !== r.kind_label) ? h("div", { class: "s" }, `spends coins of your ${spent.join(", ")} address${spent.length > 1 ? "es" : ""}; change to your ${r.kind_label} address`) : null,
      r.warning ? h("p", { class: "msg bad" }, r.warning) : null,
      cautionLine(r.caution));
    $("w-confirm").hidden = false;
    msg(sm, "");
  } catch (e) {
    // "not enough coins: have X, need Y" in satoshis -> in the coin.
    const m = e.message.match(/have (\d+), need (\d+)/);
    const text = m ? `Not enough ${TICKERS[ch.key] || "coins"}: you have ${coins(+m[1], ch.key)}, this needs ${coins(+m[2], ch.key)} (the amount and its fee).` : e.message;
    msg(sm, text + (dogeLike ? " Also: D... addresses look the same on DigiByte and Dogecoin, so make sure whoever you're paying uses DigiByte." : ""), "bad");
  }
});
// How well what a send relies on is backed, said before Send (crate ordsafe / shares::Trust): yellow when one
// arcade alone vouched, grey when none could, red when arcades disagree. Green says nothing.
function cautionLine(c) {
  if (!c || !c.note || c.level === "green") return null;
  return h("p", { class: "caution " + c.level, role: "alert" }, c.note);
}
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
