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
async function drawStart() {
  const st = await api("start.status").catch(() => null);
  const box = $("h-start");
  if (!st || st.tag) { box.hidden = true; drawStart.sig = null; return; }
  box.hidden = false;
  const c = st.claim, f = st.faucet;
  const coin = homeTicker();
  const steps = [
    {
      done: st.wallet,
      title: "Make your wallet, or restore yours",
      text: st.wallet ? "Done. Your wallet is a 12-word recovery phrase kept on this PC; it's the only backup, so keep the words safe."
        : "New here: create a wallet on the Wallet tab and write its 12 words down. Had one before: type its 12 words below to restore it, your @handle and coins included.",
      go: st.wallet ? null : ["Create a new wallet", "wallet"],
      restore: !st.wallet,
    },
    {
      done: !!st.wallet && (st.balance > 0 || (c && c.status !== "failed")),
      title: "Test coins",
      text: !st.wallet ? `A little ${coin} pays for claiming a handle. It's asked for by itself once your wallet exists.`
        : st.balance > 0 ? `Here: ${homeCoins(st.balance)}.`
        : f ? `Asked the mesh for ${coin} ${ago(f.at) === "just now" ? "just now" : ago(f.at) + " ago"}; another arcade sends them, usually within a few minutes. Nothing to do.`
        : `${coin} is asked for by itself when you claim a handle.`,
    },
    {
      done: false,
      title: "Claim your @handle",
      text: c && c.status !== "failed" ? `@${c.tag}: ${c.note || c.status}. It's yours once a block confirms it.`
        : c ? `@${c.tag} didn't work (${c.note}). Pick another one on the Handle tab.`
        : "Open the Handle tab, pick a name (2–24 characters: a–z, 0–9, _), Check, then Claim. It's published on the chain, so every arcade sees it.",
      go: st.wallet && !(c && c.status !== "failed") ? ["Open Handle", "handle"] : null,
    },
  ];
  // Drawn again only when a step changes (the page refreshes every few seconds; typed words
  // in the restore box must stay).
  const sig = JSON.stringify(steps);
  if (sig === drawStart.sig) return;
  drawStart.sig = sig;
  $("h-steps").replaceChildren(...steps.map((x, i) => h("li", { class: x.done ? "done" : "" },
    h("span", { class: "n" }, x.done ? "✓" : String(i + 1)),
    h("div", {}, h("b", {}, x.title), h("div", { class: "hint" }, x.text),
      x.go ? h("button", { class: "primary", onclick: () => goTab(x.go[1]) }, x.go[0]) : null,
      x.restore ? restoreBox() : null))));
}

// Restore a wallet from its words right on Home (the same as the Wallet tab's Restore).
let restoring = false;
function restoreBox() {
  const ta = h("textarea", { rows: "2", placeholder: "your 12 words, separated by spaces", autocomplete: "off", spellcheck: "false", class: "start-words" });
  const m = h("p", { class: "msg" });
  const b = h("button", { onclick: async () => {
    if (restoring) return;
    restoring = true; b.disabled = true; msg(m, "Restoring…");
    try {
      await api("wallet.create", { phrase: ta.value.trim().toLowerCase().split(/\s+/).join(" ") });
      ta.value = "";
      msg(m, "Restored. Your addresses, @handle and coins show up as the chain is read.", "ok");
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
function drawMesh(ov) {
  const all = ov.arcades || [];
  const linked = all.filter((a) => a.linked).length;
  const lend = new Set(all.flatMap((a) => (a.chains || []).filter((c) => c.lends != null).map((c) => c.key)));
  const copy = new Set(all.flatMap((a) => (a.chains || []).filter((c) => c.chain_copy && (c.chain_copy.can || c.chain_copy.ready)).map((c) => c.key)));
  $("h-sum").textContent = `This arcade (node ${short(ov.me.node, 12)}) is linked to ${linked} arcade${linked === 1 ? "" : "s"}` +
    (all.length > linked ? ` and has heard from ${all.length - linked} more` : "") +
    `. Cores lent: ${lend.size ? [...lend].map((k) => TICKERS[k] || k).join(", ") : "none"}. Chain copies on offer: ${copy.size ? [...copy].map((k) => TICKERS[k] || k).join(", ") : "none"}.`;
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
    ...keys.map((k) => ({ id: "own:" + k.index, own: true, index: k.index, address: k.address, legacy: k.legacy, tag: k.tag, balance: k.balance,
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
    if (W.viewKey === key) drawPane(ch);
  }
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
  if (ddOn) sub.push(`${usd(W.dd.cents)} DigiDollar`);
  if (!w.own && !w.spendable) sub.push("Wrapped SegWit: receive only for now.");
  $("w-sub").textContent = sub.join(" · ");
  $("w-addr").textContent = w.address;
  $("w-forget").hidden = w.own;
  $("w-btn-send").disabled = !w.own && !w.spendable;
  // The Send panel's assets: the chain's coin, and DigiDollar on DigiByte.
  const assets = [[ch.key, tk]].concat(ddOn ? [["dd", "DigiDollar ($)"]] : []);
  const was = $("w-asset").value;
  if ($("w-asset").options.length !== assets.length || $("w-asset").options[0].value !== ch.key) {
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

function assetChanged() {
  const dd = $("w-asset").value === "dd";
  $("w-amt-unit").textContent = dd ? "USD" : TICKERS[picked().key] || "";
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
    pane.replaceChildren(list.length ? h("div", { class: "wl-rows" }, list)
      : h("div", { class: "x-empty" }, v.tokens ? "No tokens in this wallet." : `No tokens shown: ${ch.name} needs its tokens index here (Chains & Coins).`));
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
        return h("div", { class: "x-ins", onclick: () => openExplorer("inscription", i.id) }, t, h("div", { class: "cap" }, h("b", {}, `#${num(i.n)}`)));
      })));
  }
}

function copyText(text, btn) {
  navigator.clipboard.writeText(text).then(() => { const o = btn.textContent; btn.textContent = "Copied"; setTimeout(() => { btn.textContent = o; }, 1200); }).catch(() => {});
}

async function refreshHandle() {
  const ch = picked();
  // @tags are claimed on Pepecoin testnet, whichever chain is picked.
  $("t-note").hidden = ch.home; $("t-body").hidden = !ch.home;
  $("t-note").textContent = ch.home ? "" : `@handles live on Pepecoin testnet, so they're the same on every chain. Pick Pepecoin testnet at the top to claim or check one.`;
  if (!ch.home) return;
  rows($("t-rows"), state.claims.slice().reverse(), (c) => [["@" + c.tag], [c.status], [c.note]]);
}

async function refreshMine() {
  const ch = picked();
  $("m-note").hidden = ch.home; $("m-body").hidden = !ch.home;
  $("m-note").textContent = ch.home ? "" : `Mining here is for Pepecoin testnet only. ${ch.name} is mined by its own miners and pools.`;
  if (!ch.home) return;
  const m = await api("mine.status");
  $("m-state").textContent = m.running ? `mining (${m.threads} threads)` : "stopped";
  $("m-rate").textContent = m.hash_rate ? Math.round(m.hash_rate).toLocaleString() + " H/s" : "—";
  $("m-found").textContent = m.found;
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
    if (tab === "handle") await refreshHandle();
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
    msg($("w-send-msg"), `Sent: ${r.txid}`, "ok");
    W.viewAt = 0; W.ddAt = 0; refreshWallet();
  } catch (e) { msg($("w-send-msg"), e.message, "bad"); }
  finally { b.disabled = false; }
});

// Handle actions
$("t-tag").addEventListener("input", () => { $("t-claim").disabled = true; msg($("t-msg"), ""); });
$("t-check").addEventListener("click", async () => {
  try {
    const r = await api("tag.check", { tag: $("t-tag").value });
    $("t-claim").disabled = !r.available;
    msg($("t-msg"), r.available ? `@${r.tag} is free.` : `@${r.tag} is not available: ${r.reason}`, r.available ? "ok" : "bad");
  } catch (e) { msg($("t-msg"), e.message, "bad"); }
});
$("t-claim").addEventListener("click", async () => {
  try {
    const c = await api("tag.claim", { tag: $("t-tag").value });
    $("t-claim").disabled = true;
    msg($("t-msg"), `Claim for @${c.tag}: ${c.note}`, "ok");
    refresh();
  } catch (e) { msg($("t-msg"), e.message, "bad"); }
});

// Mining
$("m-start").addEventListener("click", async () => { try { await api("mine.start"); refresh(); } catch (e) { alert(e.message); } });
$("m-stop").addEventListener("click", async () => { await api("mine.stop"); refresh(); });

refresh();
setInterval(refresh, 4000);

// Open a tab from the address (#explorer, #chains, ...), e.g. from a link or a shortcut.
(() => {
  const t = (location.hash || "").slice(1).split("/")[0];
  const b = t && document.querySelector(`nav button[data-tab="${t}"]`);
  if (b) b.click();
})();
