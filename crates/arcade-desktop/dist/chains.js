// Chains & Coins: install, follow, pause and remove chains by hand (arcaded chains.* API).
(() => {
  const MODES = [
    ["full", "Full", "Blocks, coins, and your chosen indexes. Serves the mesh.", true],
    ["shared", "Shared", "Coins, plus your share of the blocks, ord records and token ticks; the rest is held by other arcades and fetched when needed.", true],
    ["chain", "Blockchain only", "Blocks and coins (and Addresses, if you tick it). No protocol indexes.", true],
    ["index", "Index only", "Coming: chosen indexes, blocks borrowed from the mesh.", false],
    ["light", "Light", "Coming: headers only; everything else borrowed.", false],
  ];
  // Every index is opt-in. Names, maps and tokens ride on the ord index (ticking one ticks ord);
  // collections needs ord too but is read from it afterwards.
  const INDEXES = [
    ["addresses", "Addresses", "every address's history and balance, and where each coin went (address pages, balances); built after catching up", true],
    ["ledger", "Arcade ledger", "arcade tokens, exchange, arcade NFTs, @tags", true],
    ["profiles", "Profiles", "published keys, names and linked wallets", true],
    ["ord", "Ord index", "inscriptions (Pepinals, Doginals, ...), content served from the chain", true, "ord"],
    ["names", "Names", ".pepe / .doge names and \"p\":\"dns\" claims (needs ord)", true, "ord"],
    ["maps", "Maps", "block maps: <block>.pepemap, .dogemap, .bitmap (needs ord)", true, "ord"],
    ["tokens", "Tokens", "PRC-20 on Pepecoin, DRC-20 on Dogecoin: deploys, mints, transfers, balances (needs ord)", true, "ord"],
    ["collections", "Collections", "NFT collections published on the chain, and Sub 1K (needs ord; read from it, no rebuild)", true, "ord"],
    ["runes", "Runes", "coming: Bitcoin's fungible tokens", false, "runes"],
    ["dunes", "Dunes", "coming: Dogecoin's runes-style tokens", false, "dunes"],
  ];
  const RIDERS = ["names", "maps", "tokens"];
  // Indexes that need ord: ticking one ticks ord, unticking ord unticks them.
  const NEEDS_ORD = [...RIDERS, "collections"];
  let chains = [], open = null, mode = "full", watching = null, poll = null;

  const msg = (text, bad) => say("c-msg", text, bad);

  // ---------- borrowed coins (borrow.* API) ----------
  const coinAmt = (sat, key) => `${coinNum(Number(sat || 0))} ${TICKERS[key] || key}`;
  const logoOf = (key, name) => coinLogo(key, name, "div", "logo");
  const bmsg = (text, bad) => say("c-bmsg", text, bad);
  async function setBorrow(args, what) {
    try { await api("borrow.set", args); bmsg(what); refresh(); } catch (e) { bmsg(e.message, true); }
  }
  // One row per coin: what it is here (installed, borrowed, neither), its balance, the borrow
  // switch, and Install… or Manage….
  function coinRow(c, bc, globalOn) {
    bc = bc || {};
    const borrowed = !!bc.borrowed;
    const el = document.createElement("div");
    el.className = "brow" + (c.installed || borrowed ? "" : " off");
    const info = document.createElement("div"); info.className = "bi";
    const nm = document.createElement("div"); nm.className = "bn"; nm.textContent = `${c.name} `;
    const tk = document.createElement("span"); tk.className = "bt"; tk.textContent = TICKERS[c.key] || c.key; nm.append(tk);
    const d = document.createElement("div"); d.className = "bd";
    const l = document.createElement("div"); l.className = "bl";
    info.append(nm, d);
    if (c.arcade && c.installed) {
      // The ARCADE chain: installed, removed and borrowed as any coin; its blocks come over
      // the arcade mesh, so there is no storage mode or index to choose.
      d.textContent = `Installed · block ${(c.tip || 0).toLocaleString()}`;
      if (borrowed) {
        l.textContent = `Until it catches up, the borrowed copy answers balances and sends (${String(bc.state || "").replace(/^Borrowed · /, "")}).`;
        info.append(l);
      }
    } else if (c.installed) {
      const pct = Math.floor(c.overall || 0);
      d.textContent = c.home ? `Installed · home chain · ${c.mode} · ${(c.indexes || []).join(", ")}` : `Installed · ${pct}% · ${String(c.state || "").split(" [")[0]} · ${c.mode}`;
      if (!c.home && pct < 100) { const bar = document.createElement("div"); bar.className = "bar"; const i = document.createElement("i"); i.style.width = pct + "%"; bar.append(i); info.append(bar); }
      if (borrowed) {
        const nl = (bc.lenders || []).length;
        l.textContent = `Until it catches up, the borrowed copy answers balances and sends (${nl} lender${nl === 1 ? "" : "s"}, ${String(bc.state || "").replace(/^Borrowed · /, "")}).`;
        info.append(l);
      }
    } else if (borrowed) {
      d.textContent = `${bc.state || "Borrowed"} · from block ${(bc.floor ?? 0).toLocaleString()} · ${bc.coins || 0} coin${bc.coins === 1 ? "" : "s"}${bc.scan ? " · " + bc.scan : ""}`;
      l.textContent = (bc.lenders || []).length ? "Lenders: " + bc.lenders.map((x) => `${x.addr} (tip ${(x.tip || 0).toLocaleString()}${x.fails ? `, ${x.fails} recent failure${x.fails === 1 ? "" : "s"}` : ""})`).join(" · ") : "No lender right now";
      info.append(l);
    } else {
      d.textContent = "Not installed" + (bc.on === false || !globalOn ? " · borrowing off" : bc.lendable ? " · borrowing starts soon" : " · no arcade lends it right now");
    }
    if (c.arcade && c.local) d.textContent += " · local test chain";
    if (bc.mismatch) { const w = document.createElement("div"); w.className = "bw"; w.textContent = bc.mismatch; info.append(w); }
    const bal = document.createElement("div"); bal.className = "bb"; bal.textContent = borrowed ? coinAmt(bc.balance, c.key) : "";
    const act = document.createElement("div"); act.className = "ba";
    if (!c.installed && bc.key) {
      const sw = document.createElement("label"); sw.className = "sw";
      const cb = document.createElement("input"); cb.type = "checkbox"; cb.checked = !!bc.on; cb.disabled = !globalOn;
      cb.onchange = () => setBorrow({ chain: c.key, on: cb.checked }, cb.checked ? `Borrowing ${c.name}.` : `Stopped borrowing ${c.name}; its borrowed data is removed.`);
      sw.append(cb, document.createTextNode(" Borrow"));
      act.append(sw);
    }
    const btn = document.createElement("button");
    if (c.installed) { btn.textContent = c.home ? "Manage indexes…" : "Manage…"; btn.onclick = () => { manage(c.key); reveal(); }; }
    else if (c.queued) {
      // One install at a time: this one waits its turn.
      btn.textContent = `Queued #${c.queued} · Cancel`;
      btn.onclick = async () => { try { await api("chains.unqueue", { key: c.key }); msg(`${c.name} is out of the install queue.`); refresh(); } catch (e) { msg(e.message, true); } };
    }
    else { btn.textContent = "Install…"; btn.className = "primary"; btn.onclick = () => { choose(c); reveal(); }; }
    act.append(btn);
    el.append(logoOf(c.key, c.name), info, bal, act);
    return el;
  }
  $("c-borrow-on").onchange = () => setBorrow({ on: $("c-borrow-on").checked }, $("c-borrow-on").checked ? "Borrowing every coin other arcades lend." : "Borrowing is off; borrowed data is removed.");

  async function refresh() {
    try { chains = await api("chains.list"); } catch (e) { return; }
    let b = { on: false, chains: [] };
    try { b = await api("borrow.status"); } catch (e) { /* borrowing unavailable: rows still show */ }
    $("c-borrow-on").checked = !!b.on;
    const by = Object.fromEntries((b.chains || []).map((x) => [x.key, x]));
    $("c-list").replaceChildren(...chains.map((c) => coinRow(c, by[c.key], b.on)));
    if (watching) {
      try { showProgress(await api("chains.status", { key: watching })); } catch (e) { msg(e.message, true); }
    }
  }

  // Sizes as other arcades report them (median; this arcade counts too when it has the chain).
  const size = (b) => b == null ? null : b >= 1e12 ? (b / 1e12).toFixed(2) + " TB" : b >= 1e9 ? (b / 1e9).toFixed(1) + " GB" : b >= 1e6 ? Math.max(1, Math.round(b / 1e6)) + " MB" : "under 1 MB";
  const who = (n) => n === 1 ? "1 arcade" : `${n} arcades`;
  function modeSize(c, k) {
    const z = c.sizes || {};
    const b = z.modes?.[k];
    if (b == null) return "Size: no arcade has reported this chain yet.";
    const at = z.height ? ` at block ${z.height.toLocaleString()}` : "";
    const shareNote = k === "shared" ? ` · ${z.sharing} arcade${z.sharing === 1 ? "" : "s"} would share it, each block range kept by ${z.copies}` : "";
    return `About ${size(b)}${k === "full" || k === "index" || k === "shared" ? " plus the indexes you pick" : ""} · reported by ${who(z.reports)}${at}${shareNote}`;
  }
  function ixSize(c, k) {
    const x = c.sizes?.indexes?.[k];
    if (!x) return "size not reported yet";
    const b = mode === "shared" && x.shared_bytes != null ? x.shared_bytes : x.bytes;
    return `+ ${size(b)}${mode === "shared" && b !== x.bytes ? " (your share)" : ""} · ${who(x.reports)}`;
  }
  function total(c) {
    const base = c.sizes?.modes?.[mode];
    if (base == null) return "";
    let sum = base, unknown = [];
    for (const i of $("c-idx").querySelectorAll("input:checked")) {
      const x = c.sizes?.indexes?.[i.dataset.k];
      if (x) sum += mode === "shared" && x.shared_bytes != null ? x.shared_bytes : x.bytes; else unknown.push(i.dataset.k);
    }
    return `This install: about ${size(sum)}${unknown.length ? ` plus ${unknown.join(", ")} (not reported yet)` : ""}. Chains grow, so leave room.`;
  }

  function choose(c) {
    open = c; watching = null;
    $("c-installer").hidden = false; $("c-choose").hidden = false; $("c-progress").hidden = true; msg("");
    $("c-title").textContent = `Install ${c.name}`;
    // The ARCADE chain is kept whole and comes over the mesh: nothing to choose.
    for (const id of ["c-modes", "c-idx"]) { $(id).hidden = !!c.arcade; $(id).previousElementSibling.hidden = !!c.arcade; }
    if (c.arcade) {
      $("c-modes").replaceChildren(); $("c-idx").replaceChildren();
      $("c-from").textContent = "From the arcades this one is linked to: ARCADE blocks travel over the arcade mesh itself.";
      $("c-est").textContent = "The whole chain is kept: its blocks and coins, with every address's history. Removing it later deletes its blocks from this arcade; coins stay on the chain and your wallet keeps its keys.";
      $("c-total").hidden = true;
      return;
    }
    $("c-modes").replaceChildren(...MODES.map(([k, t, d, ok]) => {
      const l = document.createElement("label");
      l.className = "mode" + (k === mode ? " sel" : "") + (ok ? "" : " off");
      l.innerHTML = `<input type="radio" name="mode" ${k === mode ? "checked" : ""} ${ok ? "" : "disabled"}><b></b><small></small><small class="sz"></small>`;
      l.querySelector("b").textContent = t; l.querySelector("small").textContent = d;
      l.querySelector(".sz").textContent = modeSize(c, k);
      if (ok) l.onclick = () => { mode = k; choose(c); };
      return l;
    }));
    const noIdx = mode === "chain";
    // Runes only on Bitcoin, dunes only on Dogecoin: other chains don't list them.
    $("c-idx").replaceChildren(...INDEXES.filter(([, , , , needs]) => (needs !== "runes" || c.runes) && (needs !== "dunes" || c.dunes)).map(([k, t, d, ok, needs]) => {
      const can = ok && (!noIdx || k === "addresses") && (needs !== "ord" || c.ord) && (k !== "tokens" || c.tokens);
      const l = document.createElement("label");
      l.className = can ? "" : "off";
      l.innerHTML = `<input type="checkbox" data-k="${k}" ${can ? "" : "disabled"}> <b></b> <small></small> <small class="sz"></small>`;
      if (can) l.querySelector(".sz").textContent = ixSize(c, k);
      l.querySelector("b").textContent = k === "tokens" && c.tokens ? `${c.tokens} tokens` : t;
      l.querySelector("small").textContent = can || !ok ? d : k === "tokens" && c.ord ? "no token protocol known on this chain yet" : "not available on this chain yet";
      const box = l.querySelector("input");
      box.onchange = () => {
        const all = $("c-idx");
        if (box.checked && NEEDS_ORD.includes(k)) all.querySelector('input[data-k="ord"]').checked = true;
        if (!box.checked && k === "ord") NEEDS_ORD.forEach((r) => { const x = all.querySelector(`input[data-k="${r}"]`); if (x) x.checked = false; });
        $("c-total").textContent = total(c);
        $("c-total").hidden = !$("c-total").textContent;
      };
      return l;
    }));
    $("c-est").textContent = noIdx ? "Blocks and coins; Addresses if you tick it, no protocol indexes." : "Nothing is ticked for you: choose the indexes you want. Each can be removed later. Addresses are built in one sorted pass after the chain catches up, which is much faster.";
    $("c-total").textContent = total(c);
    $("c-total").hidden = !$("c-total").textContent;
    const offers = c.offers || 0;
    $("c-from").textContent = offers
      ? `From the arcade mesh first: ${offers} arcade${offers === 1 ? "" : "s"} offer ${c.name} right now. Then the coin's own P2P network for anything left.`
      : `Arcades on the mesh are asked first (this waits up to a minute and a half for their answers), then the coin's own P2P network.`;
  }

  // Bring the install card into view (it sits below the list of coins).
  function reveal() {
    const el = $("c-installer");
    requestAnimationFrame(() => el.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" }));
  }

  async function install() {
    const withIdx = [...$("c-idx").querySelectorAll("input:checked")].map((i) => i.dataset.k);
    try {
      const st = await api("chains.add", open.arcade ? { key: open.key } : { key: open.key, mode, with: withIdx });
      if (st.queued) {
        // Another chain is still being installed: this one starts by itself after it.
        $("c-installer").hidden = true;
        msg(`${open.name} waits its turn (#${st.position} in the queue); it installs by itself once ${st.behind.toUpperCase()} has caught up. One install runs at a time.`);
        refresh();
        return;
      }
      watching = open.key; showProgress(st); refresh();
    } catch (e) { msg(e.message, true); }
  }

  async function manage(key) {
    watching = key; $("c-installer").hidden = false; $("c-choose").hidden = true; msg("");
    try { showProgress(await api("chains.status", { key })); } catch (e) { msg(e.message, true); }
  }

  // A Core-data copy (chains.copy_*): offered far behind, fetched only when the operator picks it.
  const gb = (b) => `${(Number(b || 0) / 1e9).toFixed(b >= 1e10 ? 0 : 1)} GB`;
  function copyBox(st) {
    let box = document.getElementById("c-copy");
    if (!box) { box = document.createElement("div"); box.id = "c-copy"; box.className = "addix"; $("c-stages").before(box); }
    const cp = st.copy || {};
    const parts = [];
    const btn = (text, cls, fn) => { const b = document.createElement("button"); b.textContent = text; if (cls) b.className = cls; b.onclick = async () => { b.disabled = true; try { await fn(); refresh(); } catch (e) { b.disabled = false; msg(e.message, true); } }; return b; };
    const maker = (m) => `arcade ${(m || "").slice(0, 8)}`;
    if (cp.job) {
      const row = document.createElement("div"); row.className = "addrow";
      const t = document.createElement("div"); t.innerHTML = "<b></b><small></small>";
      t.querySelector("b").textContent = cp.job.phase === "failed" ? "Bootstrap from a copy failed" : `Bootstrapping from a copy made by ${maker(cp.job.maker)} at block ${Number(cp.job.height).toLocaleString()}`;
      t.querySelector("small").textContent = cp.job.phase === "failed" ? cp.job.detail : `${cp.job.pieces_done} of ${cp.job.pieces} pieces · ${gb(cp.job.bytes_done)} of ${gb(cp.job.bytes)} · ${cp.job.mb_s} MB/s · ${cp.job.holders} holder(s)${cp.job.prep ? " · " + cp.job.prep : ""}`;
      row.append(t);
      if (cp.job.phase !== "coins") row.append(btn(cp.job.phase === "failed" ? "Dismiss" : "Cancel and sync it myself", "small", () => api("chains.copy_cancel", { key: st.key })));
      parts.push(row);
    } else if (cp.offer && cp.worth && !cp.declined) {
      const row = document.createElement("div"); row.className = "addrow";
      const t = document.createElement("div"); t.innerHTML = "<b></b><small></small><label class=\"small\"><input type=\"checkbox\"> fetch its blocks too (instead of from the network)</label>";
      t.querySelector("b").textContent = `Bootstrap from a copy (≈ ${gb(cp.offer.bytes)} in all, from ${maker(cp.offer.maker)})`;
      t.querySelector("small").textContent = `${(cp.behind || 0).toLocaleString()} blocks behind. A Core node's coin set at block ${Number(cp.offer.height).toLocaleString()}, offered by ${cp.offer.holders} linked arcade(s): only its coin set is fetched (the blocks this arcade lacks come from the network and are stored without replaying them). The tip is checked against this arcade's own headers; the coin set is trusted from its maker until it is re-checked from the blocks.`;
      const all = t.querySelector("input");
      const go = document.createElement("div"); go.className = "row";
      go.append(btn("Bootstrap from a copy", "primary", () => api("chains.copy_start", { key: st.key, what: all.checked ? "all" : "coins" })),
        btn("Sync it myself instead", "", () => api("chains.copy_cancel", { key: st.key })));
      row.append(t, go);
      parts.push(row);
    }
    if (cp.trusted) {
      const n = document.createElement("p"); n.className = "small";
      n.textContent = `Trusted coin set: taken from a copy made by ${maker(cp.trusted.maker)} at block ${Number(cp.trusted.height).toLocaleString()}; it is being re-checked from the blocks.`;
      parts.push(n);
    } else if (cp.verified && cp.verified.replaced) {
      const n = document.createElement("p"); n.className = "small";
      n.textContent = `The copy's coin set from ${maker(cp.verified.maker)} did not match the blocks; this arcade's own replaced it at block ${Number(cp.verified.height).toLocaleString()}.`;
      parts.push(n);
    }
    box.replaceChildren(...parts);
    box.hidden = parts.length === 0;
  }

  // Earn ARCADE (docs/ARCADECHAIN.md §14.17): one switch per coin. On, the coin's whole
  // database is sealed from block 0 to the tip, then each new block as it settles; the sealed
  // copy is the only copy and earns ARCADE. One line says where it stands.
  const fmtN = (n) => Number(n || 0).toLocaleString();
  const mib = (b) => `${(Number(b || 0) / 1048576).toFixed(b >= 1073741824 ? 0 : 1)} MiB`;
  async function earnBox(st) {
    let box = document.getElementById("c-earn");
    if (!box) { box = document.createElement("div"); box.id = "c-earn"; box.className = "earn"; $("c-stages").after(box); }
    if (st.arcade) { box.hidden = true; return; }
    box.hidden = false;
    let e;
    try { e = await api("arc.earn", { chain: st.key }); } catch (err) { box.hidden = true; return; }
    const full = st.mode === "full";
    const sw = document.createElement("label"); sw.className = "switch";
    const cb = document.createElement("input"); cb.type = "checkbox"; cb.checked = !!e.on; cb.disabled = !full;
    sw.append(cb, Object.assign(document.createElement("span"), { className: "knob" }));
    cb.onchange = async () => {
      cb.disabled = true;
      try { await api("arc.earn", { chain: st.key, on: cb.checked }); msg(cb.checked ? `Earn ARCADE is on for ${st.name}: sealing starts at block 0.` : `Earn ARCADE is off for ${st.name}.`); }
      catch (err) { cb.checked = !cb.checked; msg(err.message, true); }
      cb.disabled = !full; earnBox(st);
    };
    const title = document.createElement("div"); title.className = "earn-h";
    title.append(Object.assign(document.createElement("img"), { src: "coins/arctest.png", alt: "" }), Object.assign(document.createElement("b"), { textContent: "Earn ARCADE" }), sw);
    const line = document.createElement("div"); line.className = "earn-line";
    const bar = document.createElement("div"); bar.className = "bar"; const fill = document.createElement("i"); bar.append(fill);
    let pct = 0;
    if (!full) line.textContent = `Needs ${st.name} kept in full: its whole database is sealed.`;
    else if (!e.on) line.textContent = `Seal ${st.name}'s whole database (one sealed copy, read by unsealing) and earn ARCADE for holding it.`;
    else {
      // The blocks and every index together: one percent, one bar.
      const ix = e.indexes || [];
      const done = e.sealed_to + ix.reduce((a, i) => a + i.sealed_to, 0), all = e.of + ix.reduce((a, i) => a + i.of, 0);
      pct = all ? Math.min(100, (done * 100) / all) : 0;
      const share = e.total_weight ? ((e.weight * 100) / e.total_weight).toFixed(2) : "0";
      const sealing = e.sealing ? ` · sealing a segment: ${Math.floor((e.sealing.done * 100) / Math.max(1, e.sealing.of))}%` : "";
      const ixText = ix.map((i) => ` · ${i.name} to ${fmtN(i.sealed_to)}`).join("");
      line.textContent = e.note ? e.note
        : `Sealed to block ${fmtN(e.sealed_to)} of ${fmtN(e.of)}${ixText} · ${pct.toFixed(1)}%${sealing} · earning with ${mib(e.weight)} (${share}% of all weight)`;
    }
    fill.style.width = pct + "%";
    box.replaceChildren(title, line, ...(e.on && full ? [bar] : []));
  }

  function showProgress(st) {
    $("c-progress").hidden = false;
    $("c-title").textContent = st.arcade ? `${st.name} · ${st.state}` : `${st.name} · ${st.mode} · ${st.state}`;
    $("c-stages").replaceChildren(...st.stages.map((s) => {
      const li = document.createElement("li");
      const p = s.pct || 0;
      li.className = (p >= 100 ? "done" : p > 0 ? "run" : "wait") + (s.current ? " now" : "") + (s.info ? " info" : "");
      li.innerHTML = `<div class="sh"><span class="sn"></span><span class="sd"></span></div><div class="bar"><i></i></div>`;
      li.querySelector(".sn").textContent = s.name;
      const per = s.unit === "rows" ? "rows/s" : "blocks/s";
      const rate = s.at == null && s.rate == null ? "" : s.rate == null ? "measuring…" : `${s.rate >= 10 ? Math.round(s.rate).toLocaleString() : s.rate.toFixed(1)} ${per}`;
      li.querySelector(".sd").textContent = s.info ? s.detail : p >= 100 ? `done · ${rate} · ${s.detail}` : `${p.toFixed(1)}% · ${rate}${s.at ? ` · ${s.at.toLocaleString()} of ${(s.of ?? 0).toLocaleString()}` : ""} · ${s.detail}`;
      if (s.current) li.querySelector(".sn").textContent = `${s.name} (now)`;
      li.querySelector(".bar > i").style.width = p + "%";
      // Remove-index buttons on index stages.
      const ix = s.index;
      if (ix && !(st.home && (ix === "ledger" || ix === "addresses"))) {
        const b = document.createElement("button"); b.textContent = "Remove index"; b.className = "small";
        b.onclick = async () => { try { await api("chains.index_remove", { key: st.key, index: ix }); msg(`Removing the ${s.name} index…`); refresh(); } catch (e) { msg(e.message, true); } };
        li.querySelector(".sh").append(b);
      }
      return li;
    }));
    copyBox(st);
    earnBox(st);
    // Indexes the chain doesn't have yet: a clear list at the top of the panel (opt-in, one at a time).
    let add = document.getElementById("c-addix");
    if (!add) { add = document.createElement("div"); add.id = "c-addix"; add.className = "addix"; $("c-stages").before(add); }
    const info = chains.find((x) => x.key === st.key) || {};
    const have = st.indexes || [];
    const can = st.arcade ? [] : st.mode === "chain" ? INDEXES.filter(([k]) => k === "addresses" && !have.includes(k))
      : st.mode === "full" || st.mode === "shared"
      ? INDEXES.filter(([k, , , ok, needs]) => ok && !have.includes(k) && (needs !== "ord" || info.ord) && (k !== "tokens" || info.tokens)
          && (needs !== "runes" || info.runes) && (needs !== "dunes" || info.dunes)) : [];
    add.replaceChildren(...(can.length ? [Object.assign(document.createElement("h3"), { textContent: `Indexes you can add to ${st.name}` })] : []),
      ...can.map(([k, t, d]) => {
        const row = document.createElement("div"); row.className = "addrow";
        const xs = info.sizes?.indexes?.[k];
        const name = k === "tokens" && info.tokens ? `${info.tokens} tokens` : t;
        const txt = document.createElement("div");
        txt.innerHTML = "<b></b><small></small>";
        txt.querySelector("b").textContent = name;
        txt.querySelector("small").textContent = d + (xs ? ` · about ${size(xs.bytes)}` : "");
        const b = document.createElement("button"); b.className = "primary"; b.textContent = "Add";
        b.onclick = async () => {
          b.disabled = true;
          try {
            const r = await api("chains.index_add", { key: st.key, index: k });
            msg(r.rebuilding ? `Adding ${name}: the ord index is rebuilt from its start with it.` : r.resync ? `Adding ${name}: the chain is fetched again with it built in.` : `Adding ${name}. Its progress shows below.`);
            refresh();
          } catch (e) { b.disabled = false; msg(e.message, true); }
        };
        row.append(txt, b);
        return row;
      }));
    $("c-pause").hidden = !!st.home || !!st.arcade; $("c-stop").hidden = !!st.home;
    $("c-pause").textContent = st.state === "paused" ? "Resume" : "Pause";
  }

  $("c-close").onclick = $("c-cancel").onclick = () => { $("c-installer").hidden = true; watching = null; };
  $("c-go").onclick = install;
  $("c-pause").onclick = async () => {
    if (!watching) return;
    const paused = $("c-pause").textContent === "Resume";
    try { showProgress(await api(paused ? "chains.resume" : "chains.pause", { key: watching })); } catch (e) { msg(e.message, true); }
  };
  let armed = false;
  $("c-stop").onclick = async () => {
    if (!watching) return;
    if (!armed) { armed = true; $("c-stop").textContent = "Click again to remove the chain and its data"; setTimeout(() => { armed = false; $("c-stop").textContent = "Stop and remove"; }, 4000); return; }
    armed = false;
    try { await api("chains.remove", { key: watching }); msg("Removing the chain and its data…"); watching = null; $("c-progress").hidden = true; refresh(); } catch (e) { msg(e.message, true); }
  };
  document.querySelector('nav button[data-tab="chains"]').addEventListener("click", refresh);
  clearInterval(poll); poll = setInterval(() => { if (!document.getElementById("chains").hidden && document.getElementById("chains").classList.contains("on")) refresh(); }, 2500);
  // #chains/install/<key> opens that coin's install card.
  (async () => {
    const h = (location.hash || "").slice(1).split("/");
    if (h[0] !== "chains" || !["install", "manage"].includes(h[1]) || !h[2]) return;
    await refresh();
    const c = chains.find((x) => x.key === h[2]);
    if (c && !c.installed) { choose(c); reveal(); }
    if (c && c.installed && h[1] === "manage") { manage(c.key); reveal(); }
  })();
  refresh();
})();
