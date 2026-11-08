// Arcade V1.0 desktop: everything goes through arcaded's local API (one JSON request each).
const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__.core.invoke;

async function api(cmd, args = {}) {
  const r = await invoke("api", { req: { cmd, ...args } });
  if (!r.ok) throw new Error(r.error || "failed");
  return r.result;
}

// The chain picked at the top (explorer.js owns the picker); every tab follows it.
const TICKER = { peptest: "PEPtest", pep: "PEP", doge: "DOGE", dgb: "DGB", dgbtest: "DGBtest", kot: "KOT", flop: "FLOP", ltc: "LTC", ltctest: "LTCtest", btc: "BTC", wjk: "WJK" };
const picked = () => (window.arcadeChain && window.arcadeChain()) || { key: "peptest", name: "Pepecoin testnet", home: true };
const coins = (sats, key) => (sats / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 }) + " " + (TICKER[key || picked().key] || "");
const pep = (sats) => coins(sats, "peptest");
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
  const ch = picked();
  state.wallet = s.wallet;
  $("conn").textContent = "connected"; $("conn").className = "pill ok";
  $("h-title").textContent = ch.name;
  if (ch.home) {
    $("h-sync").textContent = s.sync;
    $("h-tip").textContent = s.tip ? `block ${s.tip.height.toLocaleString()} · ${short(s.tip.hash, 16)}` : "";
  } else {
    const st = await api("chains.status", { key: ch.key });
    $("h-sync").textContent = st.installed ? st.state : "not installed";
    const src = (st.stages || []).find((x) => x.name === "Source");
    $("h-tip").textContent = st.installed ? `block ${(st.tip || 0).toLocaleString()}${src ? " · " + src.detail : ""}` : "Install it in Chains & Coins";
  }
  $("h-mesh").textContent = `${s.mesh.links} arcade${s.mesh.links === 1 ? "" : "s"} linked`;
  $("h-node").textContent = "node " + short(s.node_id, 16);
  if (ch.home) {
    $("h-tags-k").textContent = "Tags";
    $("h-tags").textContent = `${s.tags.count} tags`;
    const same = s.mesh.tips.filter((t) => t.names_digest === s.tags.names_digest).length;
    const told = s.mesh.tips.filter((t) => t.names_digest).length;
    $("h-match").textContent = told ? `matches ${same} of ${told} arcades` : "no arcade has reported yet";
  } else {
    const st = await api("chains.status", { key: ch.key });
    $("h-tags-k").textContent = "Indexes";
    $("h-tags").textContent = st.installed ? `${(st.indexes || []).length} on` : "—";
    $("h-match").textContent = st.installed ? (st.indexes || []).join(", ") || "blocks and addresses only" : "";
  }
  $("h-wallet").textContent = s.wallet ? "ready" : "not created";
  if (s.wallet) {
    state.keys = await api("wallet.keys", { chain: ch.key });
    const bal = state.keys.reduce((a, k) => a + (k.balance || 0), 0);
    const tag = state.keys.find((k) => k.tag)?.tag;
    $("h-bal").textContent = (state.keys.some((k) => k.balance != null) ? coins(bal) : "chain not installed") + (tag ? ` · @${tag}` : "");
  } else { $("h-bal").textContent = ""; }
  state.claims = await api("tag.list");
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
  // Test coins from the mesh are Pepecoin testnet coins.
  $("w-faucet").hidden = !ch.home;
  if (!$("w-ltag").value) {
    const home = await api("wallet.keys", { chain: "peptest" }).catch(() => []);
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
