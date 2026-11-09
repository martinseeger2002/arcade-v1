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
  drawMesh(await api("mesh.overview"));
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

async function drawImports() {
  const ch = picked();
  const all = await api("wallet.imports").catch(() => []);
  const list = all.filter((w) => w.chain === ch.key);
  $("w-imps").replaceChildren(...(list.length ? list : [null]).map((w) => {
    const tr = document.createElement("tr");
    if (!w) { const td = document.createElement("td"); td.colSpan = 5; td.className = "note"; td.textContent = `None on this chain${all.length ? ` (${all.length} on other chains)` : ""}.`; tr.append(td); return tr; }
    for (const [text, cls] of [[w.name || "—"], [w.address], [`${w.kind === "phrase" ? "words" : "key"} · ${w.addr_kind}`], [w.balance == null ? "not installed" : coins(w.balance), "num"]]) {
      const td = document.createElement("td"); td.textContent = text; if (cls) td.className = cls; tr.append(td);
    }
    const td = document.createElement("td"); const b = document.createElement("button"); b.textContent = "Remove";
    b.addEventListener("click", async () => {
      if (b.dataset.sure !== "1") { b.dataset.sure = "1"; b.textContent = "Remove: sure?"; return; }
      try { await api("wallet.import_remove", { chain: ch.key, address: w.address }); msg($("w-msg"), `Forgot ${w.address} and its key.`, "ok"); refreshWallet(); }
      catch (e) { msg($("w-msg"), e.message, "bad"); }
    });
    td.append(b); tr.append(td); return tr;
  }));
}

async function refreshWallet() {
  const ch = picked();
  $("w-title").textContent = `Wallet · ${ch.name}`;
  await drawAccounts();
  $("w-none").hidden = state.wallet; $("w-keys").hidden = !state.wallet;
  if (!state.wallet) return;
  drawImports();
  state.keys = await api("wallet.keys", { chain: ch.key });
  rows($("w-rows"), state.keys, (k) => [[String(k.index)], [k.address], [k.tag ? "@" + k.tag : "—"], [k.balance == null ? "not installed" : coins(k.balance), "num"]]);
  // DigiByte receives at dgb1... (its D... addresses are the same strings as Dogecoin's).
  const legacy = state.keys.filter((k) => k.legacy).map((k) => k.legacy);
  $("w-legacy").hidden = !legacy.length;
  $("w-legacy").textContent = legacy.length ? `Coins sent to ${legacy.join(", ")} (the older D... form of the same key) are counted too. Give people the dgb1... address: D... addresses look the same on Dogecoin.` : "";
  $("w-send-h").textContent = `Send ${TICKERS[ch.key] || ch.name}`;
  $("w-amt-unit").textContent = TICKERS[ch.key] || "";
  // Test coins from the mesh are Pepecoin testnet coins.
  $("w-faucet").hidden = !ch.home;
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
$("w-link").addEventListener("click", async () => {
  try {
    const r = await api("wallet.link", { chain: picked().key, wif: $("w-wif").value, tag: $("w-ltag").value });
    $("w-wif").value = "";
    msg($("w-msg"), `Linked ${r.a} to @${r.tag}, signed by both keys and sent to the mesh.`, "ok");
    refreshWallet();
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
$("w-import").addEventListener("click", async () => {
  try {
    const r = await api("wallet.import", { chain: picked().key, secret: $("w-isecret").value, addr_kind: $("w-ikind").value, name: $("w-iname").value });
    $("w-isecret").value = ""; $("w-iname").value = "";
    msg($("w-msg"), r.imported ? `Imported ${r.wallet.address}.` : `${r.wallet.address} was already here.`, "ok");
    refreshWallet();
  } catch (e) { msg($("w-msg"), e.message, "bad"); }
});
$("w-faucet").addEventListener("click", async () => {
  try { await api("faucet.ask"); msg($("w-msg"), "Asked the mesh for test coins; any arcade with spare coins can send them.", "ok"); }
  catch (e) { msg($("w-msg"), e.message, "bad"); }
});

// Send this chain's coin: review first (fee, and a warning when the address could be meant for
// another chain), then send.
let pendingSend = null;
const toSats = (t) => {
  const m = String(t).trim().match(/^(\d+)(?:\.(\d{1,8}))?$/);
  if (!m) return null;
  return Number(m[1]) * 1e8 + Number((m[2] || "").padEnd(8, "0"));
};
$("w-review").addEventListener("click", async () => {
  const ch = picked();
  const sats = toSats($("w-amt").value);
  const sm = $("w-send-msg");
  if (!sats) { msg(sm, "Enter an amount, for example 12.5", "bad"); return; }
  // A DigiByte D... address is also a Dogecoin address: say so before anything else.
  const to = $("w-to").value.trim();
  const dogeLike = ch.key === "dgb" && /^D[1-9A-HJ-NP-Za-km-z]{25,34}$/.test(to);
  try {
    const r = await api("wallet.send", { chain: ch.key, to: $("w-to").value.trim(), amount: sats, send: false });
    pendingSend = { chain: ch.key, to: $("w-to").value.trim(), amount: sats };
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
    const r = await api("wallet.send", { ...pendingSend, send: true });
    $("w-confirm").hidden = true; pendingSend = null; $("w-to").value = ""; $("w-amt").value = "";
    msg($("w-send-msg"), `Sent: ${r.txid}`, "ok");
    refreshWallet();
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
