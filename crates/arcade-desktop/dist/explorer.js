// Explorer tab + the chain selector in the header. Everything is read from this arcade's own
// chain.mdbx through arcaded's explorer.* API: no outside service.
(() => {
  // A chain's coin: the original arcade's icon, or a letter on the chain's colour.
  const coinEl = (c) => coinLogo(c.key, c.name, "span", "coin");
  const SECTIONS = [
    ["home", "Overview"], ["blocks", "Blocks"], ["mempool", "Mempool"], ["inscriptions", "Inscriptions", "ord"],
    ["collections", "Collections", "collections"], ["tokens", "Tokens", "tokens"], ["names", "Names", "names"], ["maps", "Maps", "maps"], ["dogefeed", "DogeFeed", "dogefeed"], ["ledger", "Arcade ledger", "ledger"],
    // Runes and dunes join here (with their index keys) when those indexes exist.
  ];
  // Pages and the index each needs; a page comes online only when its index is on the chain.
  const NEEDS = { collections: "collections", collection: "collections", inscriptions: "ord", inscription: "ord", tokens: "tokens", token: "tokens", names: "names", name: "names", maps: "maps", map: "maps", dogefeed: "dogefeed", dfpost: "dogefeed", dfaddr: "dogefeed", ledger: "ledger", ltokens: "ledger", linscs: "ledger", ltoken: "ledger", linsc: "ledger" };
  const DETAIL_PARENT = { collection: "collections", block: "blocks", tx: "blocks", address: "home", inscription: "inscriptions", token: "tokens", name: "names", map: "maps", dfpost: "dogefeed", dfaddr: "dogefeed", ltokens: "ledger", linscs: "ledger", ltoken: "ledger", linsc: "ledger", tag: "home", results: "home" };

  let chains = [], chain = null, sum = null, page = "home", arg = null, seq = 0;

  // ---------- helpers ----------
  const fmt = (n) => (n ?? 0).toLocaleString("en-US");
  const short = (s, a = 10, b = 8) => (s && s.length > a + b + 1 ? s.slice(0, a) + "…" + s.slice(-b) : s || "");
  const coins = (sats) => coinNum(sats, "en-US") + " " + (sum?.unit || "");
  const ago = (t) => {
    if (!t) return "";
    const s = Math.max(0, Math.floor(Date.now() / 1000 - t));
    if (s < 90) return `${s}s ago`; if (s < 5400) return `${Math.floor(s / 60)}m ago`; if (s < 172800) return `${Math.floor(s / 3600)}h ago`;
    return new Date(t * 1000).toLocaleDateString();
  };
  const units = (v, dec) => { // base-unit decimal string -> display
    try {
      const b = BigInt(v || "0"), d = 10n ** BigInt(dec ?? 18), w = b / d, f = (b % d).toString().padStart(Number(dec ?? 18), "0").replace(/0+$/, "");
      return w.toLocaleString("en-US") + (f ? "." + f.slice(0, 8) : "");
    } catch { return String(v); }
  };
  // Indexes kept here, or by another arcade on the mesh that answers for them.
  const has = (ix) => !!sum && (sum.indexes.includes(ix) || (sum.mesh_indexes || []).includes(ix));
  // Pages (or parts) answered by another arcade say so.
  let meshFrom = null;
  // This arcade's HTTP server: inscription lists ask for images as links to it (img: "links")
  // and each picture loads on its own, so the list itself comes back at once.
  let httpBase = null;
  const imgArgs = async () => {
    if (httpBase === null) {
      try { httpBase = (await api("rpc.info")).url.replace(/\/rpc$/, ""); } catch { httpBase = ""; }
    }
    return httpBase ? { img: "links" } : {};
  };
  const xapi = async (cmd, args) => {
    const r = await api(cmd, args);
    if (r && r._mesh) meshFrom = r._mesh;
    else if (r && r.mesh_parts) meshFrom = Object.values(r.mesh_parts)[0];
    return r;
  };
  const link = (text, p, a, extra) => h("span", { class: "link" + (extra ? " " + extra : ""), onclick: (ev) => { ev.stopPropagation(); go(p, a); } }, text);
  const addrLink = (a, tag) => a ? h("span", {}, link(short(a, 10, 6), "address", a, "mono"), tag ? h("span", { class: "chip arc" }, "@" + tag) : "") : h("span", { class: "faint" }, "no address");

  // ---------- chain selector ----------
  async function loadChains() {
    try { chains = await api("chains.list"); } catch { return; }
    const inst = chains.filter((c) => c.installed || c.borrowed);
    const first = !chain;
    // ?chain=<key> picks the starting chain (links, shortcuts).
    const want = first ? new URLSearchParams(location.search).get("chain") : null;
    if (want && inst.find((c) => c.key === want.toLowerCase())) chain = inst.find((c) => c.key === want.toLowerCase());
    else if (!chain || !inst.find((c) => c.key === chain.key)) chain = inst.find((c) => c.home) || inst[0];
    else chain = inst.find((c) => c.key === chain.key);
    paintSelector();
    if (first) announce();
  }
  function paintSelector() {
    if (!chain) return;
    $("cs-coin").replaceWith(Object.assign(coinEl(chain), { id: "cs-coin" }));
    $("cs-name").textContent = chain.name; $("cs-sub").textContent = "";
  }
  function menu() {
    const m = $("cs-menu");
    // Just the coins you can open (installed or borrowed): icon and name.
    const row = (c) => h("button", { class: c.key === chain?.key ? "on" : "", onclick: () => pick(c) }, coinEl(c), h("span", {}, c.name));
    m.replaceChildren(...chains.filter((c) => c.installed || c.borrowed).map(row));
  }
  function pick(c) { remember(); chain = c; close(); paintSelector(); sum = null; go("home", null, false); announce(); }
  // Every tab follows the picked chain.
  function announce() { window.dispatchEvent(new CustomEvent("arcade:chain", { detail: chain })); }
  function close() { $("cs-menu").hidden = true; $("cs-btn").setAttribute("aria-expanded", "false"); }
  $("cs-btn").onclick = async (ev) => {
    ev.stopPropagation();
    const m = $("cs-menu");
    if (m.hidden) { await loadChains(); menu(); }
    m.hidden = !m.hidden; $("cs-btn").setAttribute("aria-expanded", String(!m.hidden));
  };
  document.addEventListener("click", (ev) => { if (!ev.target.closest(".chainsel")) close(); });
  document.addEventListener("keydown", (ev) => { if (ev.key === "Escape") close(); });

  // ---------- pieces ----------
  function preview(i, big) {
    const t = h("div", { class: big ? "x-big" : "x-thumb" });
    const c = i.content || {};
    if (c.kind === "image") {
      const img = h("img", { src: c.src && httpBase ? httpBase + c.src : c.data, alt: `inscription ${i.n}`, loading: "lazy", decoding: "async" });
      img.onerror = () => img.replaceWith(h("div", { class: "x-ph" }, "image not available"));
      t.append(img);
    }
    else if (c.kind === "html" && c.src && httpBase) {
      // Run sandboxed: an opaque origin, scripts only (no forms, popups or top navigation),
      // loading only from this arcade (its CSP). An inscription's own page runs at once; a
      // thumbnail (drawn at 4x and scaled down, not taking clicks) waits for one of the
      // HTML_MAX places while it is on screen.
      if (big) t.append(htmlFrame(c.src, i.n, false));
      else { t._src = c.src; t._n = i.n; t.append(htmlWaiting()); htmlObs.observe(t); }
    }
    else if (c.kind === "text" || c.kind === "html") t.append(h("pre", {}, (c.text || "").slice(0, big ? 20000 : 400)));
    else t.append(h("div", { class: "x-ph" }, c.kind === "missing" ? "content not stored" : (c.type || "binary")));
    if (!big) t.append(h("span", { class: "tag" }, (i.ct || "").split(";")[0] || "no type"));
    return t;
  }
  function blockRows(list) {
    return h("div", { class: "x-list" }, list.map((b) => h("div", { class: "x-blk", onclick: () => go("block", String(b.height)) },
      h("div", { class: "x-hbox" }, h("b", {}, fmt(b.height)), h("small", {}, ago(b.time))),
      h("div", { class: "x-mid" }, h("div", { class: "x-hash" }, b.hash),
        h("div", { class: "chips" }, b.txs != null ? h("span", { class: "chip" }, `${b.txs} tx`) : "", b.inscriptions ? h("span", { class: "chip ins" }, `${b.inscriptions} inscription${b.inscriptions > 1 ? "s" : ""}`) : "")))));
  }
  // HTML thumbnails take turns (Martin 2026-11-10: nine at a time, more overwhelms the page):
  // at most HTML_MAX run at once, only those on screen; one that scrolls away or leaves the
  // page stops, and the next waiting one starts.
  const HTML_MAX = 9;
  const htmlLive = new Set(), htmlWait = [];
  const htmlFrame = (src, n, mini) => h("iframe", { class: mini ? "x-html mini" : "x-html", src: httpBase + src, sandbox: "allow-scripts allow-pointer-lock", referrerpolicy: "no-referrer", title: `inscription ${n}` });
  const htmlWaiting = () => h("div", { class: "x-ph x-wait" }, "HTML · waits its turn");
  function htmlStart(t) {
    t.querySelector(".x-wait")?.replaceWith(htmlFrame(t._src, t._n, true));
    htmlLive.add(t);
  }
  function htmlStop(t) {
    if (!htmlLive.delete(t)) return;
    t.querySelector("iframe.x-html")?.replaceWith(htmlWaiting());
  }
  function htmlPump() {
    for (const t of [...htmlLive]) if (!t.isConnected) { htmlLive.delete(t); htmlObs.unobserve(t); }
    for (let k = htmlWait.length - 1; k >= 0; k--) if (!htmlWait[k].isConnected) { htmlObs.unobserve(htmlWait[k]); htmlWait.splice(k, 1); }
    while (htmlLive.size < HTML_MAX && htmlWait.length) htmlStart(htmlWait.shift());
  }
  const htmlObs = new IntersectionObserver((entries) => {
    for (const e of entries) {
      const t = e.target, k = htmlWait.indexOf(t);
      if (e.isIntersecting) { if (!htmlLive.has(t) && k < 0) htmlWait.push(t); }
      else { if (k >= 0) htmlWait.splice(k, 1); htmlStop(t); }
    }
    htmlPump();
  });
  // A new page drops the frames of the old one (their elements leave the document).
  new MutationObserver(() => htmlPump()).observe(document.body, { childList: true, subtree: true });

  // The content on its own, full size, in a new arcade window (path on this arcade's server).
  function contentLink(path, title) {
    if (!httpBase || !path) return "";
    return h("button", { class: "x-open", title: "Open the content in a new window", onclick: (ev) => {
      ev.stopPropagation();
      invoke("open_content", { url: httpBase + path, title }).catch((e) => alert(String(e)));
    } }, "↗");
  }
  function insGrid(list) {
    if (!list.length) return h("div", { class: "x-empty" }, "No inscriptions yet.");
    return h("div", { class: "x-grid" }, list.map((i) => h("div", { class: "x-ins", onclick: () => go("inscription", i.id) }, preview(i),
      h("div", { class: "cap" }, h("b", {}, `#${fmt(i.n)}`), h("span", { class: "faint" }, fmt(i.h)), i.from_arcade ? "" : contentLink(`/content/${i.id}`, `Inscription #${fmt(i.n)}`)))));
  }
  // An arcade-ledger inscription's HTML runs from /app/<chain>/<txid> (its sandboxed page).
  const ledgerItem = (i) => {
    const c = { ...(i.content || {}) };
    if (c.kind === "html" && chain) c.src = `/app/${chain.key}/${i.txid}?pass=1`;
    return { content: c, ct: i.content_type, n: i.number };
  };
  function table(head, rows) {
    return h("div", { class: "x-tbl" }, h("table", {}, h("thead", {}, h("tr", {}, head.map(([t, num]) => h("th", { class: num ? "num" : "" }, t)))),
      h("tbody", {}, rows.map((r) => h("tr", { class: r.go ? "click" : "", onclick: r.go || null }, r.cells.map(([v, num]) => h("td", { class: num ? "num" : "" }, v)))))));
  }
  // Pages: the page number lives in the address arg as "p<N>" (1 = newest).
  const pageOf = () => (arg && /^p\d+$/.test(arg) ? Math.max(1, Number(arg.slice(1))) : 1);
  // Pages of a list (Martin 2026-11-10: buttons must not change size or place). Seven number
  // slots of one width (the widest page number of this list), the current page in the middle
  // one; First, Prev, Next and Last always in place (greyed at the ends); the count and the Go
  // box on their own line. Go takes a page, or with gotoPage "#" and an inscription number.
  function windowOf(page, pages) {
    if (pages <= 7) return Array.from({ length: 7 }, (_, k) => (k < pages ? k + 1 : null));
    if (page <= 4) return [1, 2, 3, 4, 5, "…", pages];
    if (page >= pages - 3) return [1, "…", pages - 4, pages - 3, pages - 2, pages - 1, pages];
    return [1, "…", page - 1, page, page + 1, "…", pages];
  }
  // `argOf`: a detail view's own arg for page p (a token's holders), instead of "p<n>".
  function pager(total, size, page, where, gotoLabel, gotoPage, argOf) {
    const pages = Math.max(1, Math.ceil(total / size));
    page = Math.min(page, pages);
    const to = (p) => { const n = Math.min(pages, Math.max(1, p)); go(where, argOf ? argOf(n) : `p${n}`); };
    const nav = (cls, label, p, title, short) => h("button", { type: "button", class: cls, disabled: p < 1 || p > pages || p === page, title, "aria-label": title, "data-short": short || "", onclick: () => to(p) }, label);
    const slots = windowOf(page, pages).map((v) =>
      v === null ? h("button", { type: "button", class: "slot empty", tabindex: "-1", "aria-hidden": "true" }, "0")
      : v === "…" ? h("button", { type: "button", class: "slot gap", tabindex: "-1", "aria-hidden": "true" }, "…")
      : h("button", { type: "button", class: "slot" + (v === page ? " on" : ""), disabled: v === page, "aria-current": v === page ? "page" : false, onclick: () => to(v) }, fmt(v)));
    const box = h("input", { type: "text", inputmode: "numeric", placeholder: gotoPage ? `page or #${(gotoLabel || "number").replace(/\s*#$/, "")}` : "page", "aria-label": gotoPage ? "Go to a page, or # and a number" : "Go to a page" });
    const err = h("span", { class: "pg-err", "aria-live": "polite" });
    const jump = () => {
      const raw = String(box.value).trim(), v = Number(raw.replace(/[,#\s]/g, ""));
      if (!raw || !Number.isFinite(v) || v < 0) { err.textContent = gotoPage ? "Type a page number, or # and a number." : "Type a page number."; return; }
      to(gotoPage && raw.startsWith("#") ? gotoPage(v) : v);
    };
    box.onkeydown = (e) => { if (e.key === "Enter") jump(); };
    return h("div", { class: "pgr", style: `--slot: calc(${fmt(pages).length + 1.5}ch + 10px)` },
      h("div", { class: "pgr-bar" },
        nav("edge", "|‹", 1, "First page"), nav("step", "‹ Prev", page - 1, "Previous page", "‹"),
        h("div", { class: "pgr-slots" }, ...slots),
        nav("step", "Next ›", page + 1, "Next page", "›"), nav("edge", "›|", pages, "Last page")),
      h("div", { class: "pgr-foot" },
        h("span", { class: "pgr-where" }, `Page ${fmt(page)} of ${fmt(pages)} · ${fmt(total)} in all`), err,
        h("span", { class: "pgr-go" }, box, h("button", { type: "button", onclick: jump }, "Go"))));
  }
  const sect = (title, more, body) => h("section", { class: "x-sect" }, h("header", {}, h("h2", {}, title), more ? link("View all →", more) : ""), body);
  const kv = (rows) => h("div", { class: "x-kv" }, rows.filter(Boolean).flatMap(([k, v]) => [h("div", { class: "k" }, k), h("div", {}, v)]));
  const crumbs = (...parts) => h("div", { class: "x-crumbs" }, parts.flatMap((p, i) => (i ? [" / ", p] : [p])));
  const head = (title, ...pills) => h("div", { class: "x-dhead" }, h("h1", {}, title), ...pills);
  const pill = (t, ok) => h("span", { class: "pill" + (ok ? " ok" : "") }, t);

  // ---------- views ----------
  // Overviews kept per coin: switching back shows the last one at once, then the fresh one.
  const SUMS = {};
  let fresh = false;
  function refreshHome(key, delay) {
    setTimeout(async () => {
      try {
        const v = await xapi("explorer.summary", { chain: key });
        SUMS[key] = { v, at: Date.now() };
        if (page === "home" && chain && chain.key === key) { fresh = true; render(); }
      } catch {}
    }, delay);
  }
  async function vHome() {
    const key = chain.key, kept = SUMS[key];
    if (fresh || !kept || Date.now() - kept.at > 120000) {
      if (!fresh) SUMS[key] = { v: await xapi("explorer.summary", { chain: key }), at: Date.now() };
      fresh = false;
      // Parts other arcades answer still on their way: ask again shortly.
      if (SUMS[key].v.mesh_pending) refreshHome(key, 2000);
    } else {
      refreshHome(key, 0);
    }
    sum = SUMS[key].v;
    const tok = sum.protocol ? sum.protocol.toUpperCase() : "";
    const st = [
      ["Height", fmt(sum.tip), sum.tip_time ? `last block ${ago(sum.tip_time)}` : ""],
      ["Mempool", `${fmt(sum.mempool.count)} tx`, sum.mempool.bytes ? `${fmt(sum.mempool.bytes)} bytes` : "waiting for a block"],
      has("ord") && ["Inscriptions", fmt(sum.inscriptions), "ord index"],
      has("tokens") && [`${tok} tokens`.trim(), fmt(sum.ticks), "deployed ticks"],
      has("names") && ["Names", fmt(sum.names?.[0] ?? 0), sum.names ? `json ${fmt(sum.names[1])} · doge2023 ${fmt(sum.names[2])}` : ""],
      has("maps") && ["Maps", fmt(sum.maps), "first claim wins"],
    ].filter(Boolean);
    const lower = [
      has("tokens") && sect(`${tok} tokens`.trim(), "tokens", tokTable(sum.tokens || [])),
      has("names") && sect("Names", "names", (sum.name_rows || []).length ? table([["Name"], ["Inscription", 1]], sum.name_rows.map((r) => ({ go: () => go("name", r.name), cells: [[h("span", { class: "mono" }, r.name)], [`#${fmt(r.number)}`, 1]] }))) : h("div", { class: "x-empty" }, "No names claimed yet.")),
      has("maps") && sect("Maps", "maps", (sum.map_rows || []).length ? table([["Claim"], ["Inscription", 1]], sum.map_rows.map((r) => ({ go: () => go("map", r.claim), cells: [[h("span", { class: "mono" }, r.claim)], [`#${fmt(r.number)}`, 1]] }))) : h("div", { class: "x-empty" }, "No block maps claimed yet.")),
    ].filter(Boolean);
    return h("div", {},
      sum.borrowed ? h("p", { class: "x-note" }, chain.installed
        ? `This arcade is still catching up on ${chain.name} (block ${fmt(sum.syncing_at)}): the newest blocks and the mempool come from a lender meanwhile.`
        : `${chain.name} is borrowed here, not installed: its blocks and the mempool come from a lender.`) : "",
      h("div", { class: "x-stats" }, st.map(([k, v, s]) => h("div", { class: "x-stat" }, h("div", { class: "k" }, k), h("div", { class: "v" }, v), h("div", { class: "s" }, s)))),
      h("div", { class: has("ord") ? "x-cols" : "x-cols one" },
        sect("Latest blocks", "blocks", blockRows(sum.blocks)),
        has("ord") ? sect("Latest inscriptions", "inscriptions", insGrid(sum.latest || [])) : ""),
      lower.length ? h("div", { class: `x-cols3 n${lower.length}` }, lower) : "");
  }
  function tokTable(list) {
    if (!list.length) return h("div", { class: "x-empty" }, "No tokens deployed yet.");
    return table([["Tick"], ["Minted"], ["Supply", 1]], list.map((t) => {
      const pct = Number(BigInt(t.minted || "0") * 1000n / (BigInt(t.supply || "1") || 1n)) / 10;
      return { go: () => go("token", t.tick), cells: [[h("b", {}, t.shown || t.tick)], [h("div", { class: "x-meter", title: `${pct}%` }, h("i", { style: `width:${Math.max(1, pct)}%` }))], [units(t.supply, t.dec), 1]] };
    }));
  }
  async function vBlocks() {
    const size = 25, total = (sum?.tip ?? 0) + 1, page = pageOf();
    const before = Math.max(0, total - (page - 1) * size);
    const list = await xapi("explorer.blocks", { chain: chain.key, before, n: size });
    // Jump to a height: the page that holds it (page 1 = newest).
    const toPage = (height) => Math.floor((total - 1 - height) / size) + 1;
    return sect("Blocks", null, h("div", {}, pager(total, size, page, "blocks", "block height", toPage), blockRows(list), pager(total, size, page, "blocks", "block height", toPage)));
  }
  async function vMempool() {
    const m = await xapi("explorer.mempool", { chain: chain.key });
    // Lender pools give fee and size, not output totals.
    const lent = m.txs.some((t) => t.out == null);
    const body = m.txs.length ? table([["Txid"], [lent ? "Fee" : "Out", 1], ["Size", 1], ["Kind"]], m.txs.map((t) => ({ go: () => go("tx", t.txid),
      cells: [[h("span", { class: "mono" }, short(t.txid, 16, 8))], [t.out != null ? coins(t.out) : t.fee != null ? coins(t.fee) : "", 1], [t.size != null ? `${fmt(t.size)} B` : "", 1], [h("span", { class: "chip" + (t.kind === "inscription" ? " ins" : "") }, (t.mine ? "yours · " : "") + t.kind + (t.rbf ? " · RBF" : ""))]] })))
      : h("div", { class: "x-empty" }, "No unconfirmed transactions right now.");
    return h("div", {}, sect(`Mempool · ${fmt(m.count ?? m.txs.length)}`, null, body),
      m.pending_tags.length ? h("p", { class: "x-note" }, `Tag claims waiting for a block: ${m.pending_tags.map((t) => "@" + (t.tag || t)).join(", ")}`) : "",
      h("p", { class: "x-note" }, m.lender_pool !== undefined ? "The whole mempool, read from a lender's node (newest 500 shown)." : "This arcade's mempool, shared with the other V1.0 arcades on the mesh: a transaction any of them hears shows here within seconds. Spending with zero confirmations is fine on chains without RBF."));
  }
  // Filter by content type (MIME type): groups, newest first; "Show older" goes on.
  const INS_TYPES = [["", "All"], ["image", "Images"], ["svg", "SVG"], ["text", "Text"], ["html", "HTML"], ["json", "JSON"], ["audio", "Audio"], ["video", "Video"], ["model", "3D"], ["other", "Other"]];
  let insType = "";
  // Every type's total, counted ahead by the arcade keeping the ord index (kept a minute).
  const TYPE_TOTALS = {};
  function typeChips() {
    const key = chain.key;
    const box = h("div", { class: "x-types" }, INS_TYPES.map(([v, label]) => h("button", { class: "chip-btn" + (insType === v ? " on" : ""), "data-t": v || "all", onclick: () => { remember(); insType = v; oldKey = ""; go("inscriptions", null, false); } }, label)));
    const paint = (r) => {
      for (const b of box.querySelectorAll("button")) {
        const t = b.dataset.t, n = t === "all" ? r.total : r.types?.[t];
        const label = INS_TYPES.find((x) => (x[0] || "all") === t)[1];
        b.textContent = n != null ? `${label} · ${fmt(n)}${r.counting && t !== "all" ? "+" : ""}` : label;
      }
    };
    const c = TYPE_TOTALS[key];
    if (c) paint(c.r);
    if (!c || Date.now() - c.at > 60000) {
      xapi("explorer.inscription_types", { chain: key }).then((r) => { TYPE_TOTALS[key] = { r, at: Date.now() }; if (chain.key === key) paint(r); }).catch(() => {});
    }
    return box;
  }
  // An arcade on an older version answered: no per-type totals, only "newest first, looked
  // down to #N". Shown as before: a growing list with Show older.
  let oldAcc = [], oldNext = null, oldKey = "";
  function oldTyped(r, label) {
    const key = `${chain.key}|${insType}`;
    if (oldKey !== key) { oldKey = key; oldAcc = r.items; oldNext = r.next_before ?? null; }
    const more = oldNext != null ? h("div", { class: "row" }, h("button", { onclick: async (ev) => {
      ev.target.disabled = true; ev.target.textContent = "Looking…";
      const rr = await xapi("explorer.inscriptions", { chain: chain.key, type: insType, before: oldNext, n: 36, ...(await imgArgs()) });
      oldAcc = oldAcc.concat(rr.items); oldNext = rr.next_before ?? null;
      render();
    } }, `Show older (looked down to #${fmt(oldNext)})`)) : h("p", { class: "x-note" }, "That's all of them.");
    return sect(`${label} · ${fmt(oldAcc.length)} found so far, newest first`, null, h("div", {}, typeChips(),
      h("p", { class: "x-note" }, "Answered by an arcade on an older version, which can't count them by type; it looks back a part at a time."),
      oldAcc.length ? insGrid(oldAcc) : h("div", { class: "x-empty" }, oldNext != null ? "None in the newest ones looked at yet." : "None."), more));
  }
  async function vInscriptions() {
    // HTML pages run live: nine to a page (Martin 2026-11-10), the rest 36.
    const size = insType === "html" ? 9 : 36, page = pageOf();
    const first = await xapi("explorer.inscriptions", { chain: chain.key, n: 1 });
    const total = first.total;
    if (insType) {
      // Paged like the full list (newest first), with the type's own total.
      const r = await xapi("explorer.inscriptions", { chain: chain.key, type: insType, offset: (page - 1) * size, n: size, ...(await imgArgs()) });
      const label = INS_TYPES.find((t) => t[0] === insType)[1];
      if (r.type_total == null) return oldTyped(r, label);
      const tt = r.type_total;
      const note = r.counting ? h("p", { class: "x-note" }, `Still counting the older ones: ${fmt(tt)} found so far; the total grows as it counts.`) : "";
      return sect(`${label} · ${fmt(tt)}${r.counting ? "+" : ""}`, null, h("div", {}, typeChips(), note, pager(tt, size, page, "inscriptions"),
        r.items.length ? insGrid(r.items) : h("div", { class: "x-empty" }, "None."), pager(tt, size, page, "inscriptions")));
    }
    const r = await xapi("explorer.inscriptions", { chain: chain.key, before: Math.max(0, total - (page - 1) * size), n: size, ...(await imgArgs()) });
    const toPage = (num) => Math.floor((total - 1 - num) / size) + 1;
    return sect(`Inscriptions · ${fmt(total)}`, null, h("div", {}, typeChips(), pager(total, size, page, "inscriptions", "inscription #", toPage), insGrid(r.items), pager(total, size, page, "inscriptions", "inscription #", toPage)));
  }
  async function vTokens() {
    const size = 50, page = pageOf();
    const r = await xapi("explorer.tokens", { chain: chain.key, offset: (page - 1) * size, n: size });
    return sect(`${r.protocol.toUpperCase()} tokens · ${fmt(r.total)}`, null, h("div", {}, pager(r.total, size, page, "tokens"), tokTable(r.items), pager(r.total, size, page, "tokens")));
  }
  async function vNames() {
    const size = 50, page = pageOf();
    const r = await xapi("explorer.names", { chain: chain.key, offset: (page - 1) * size, n: size });
    const total = r.total ?? r.counts[0];
    return sect(`Names · ${fmt(total)}`, null, h("div", {}, pager(total, size, page, "names"),
      r.items.length ? table([["Name"], ["Winning inscription", 1]], r.items.map((x) => ({ go: () => go("name", x.name), cells: [[h("span", { class: "mono" }, x.name)], [`#${fmt(x.number)}`, 1]] })))
        : h("div", { class: "x-empty" }, "No names claimed yet."), pager(total, size, page, "names")));
  }
  async function vMaps() {
    const size = 50, page = pageOf();
    const r = await xapi("explorer.maps", { chain: chain.key, offset: (page - 1) * size, n: size });
    const total = r.total ?? r.items.length;
    return sect(`Maps · ${fmt(total)}`, null, h("div", {}, pager(total, size, page, "maps"),
      r.items.length ? table([["Claim"], ["Winning inscription", 1]], r.items.map((x) => ({ go: () => go("map", x.claim), cells: [[h("span", { class: "mono" }, x.claim)], [`#${fmt(x.number)}`, 1]] })))
        : h("div", { class: "x-empty" }, "No block maps claimed yet."), pager(total, size, page, "maps"),
      h("p", { class: "x-note" }, "A map claim is an inscription whose whole text is <block>.<name>map (pepemap, dogemap, bitmap, …). The block can't be above the one it was inscribed in, and the first claim wins.")));
  }
  // Arcade ledger: the arcade's own tokens (Omni-style properties), inscriptions and collections.
  function ledgerGrid(list) {
    if (!list.length) return h("div", { class: "x-empty" }, "No arcade inscriptions yet.");
    return h("div", { class: "x-grid" }, list.map((i) => h("div", { class: "x-ins", onclick: () => go("linsc", i.txid) },
      preview(ledgerItem(i)), h("div", { class: "cap" }, h("b", {}, `#${fmt(i.number)}`), h("span", { class: "faint" }, fmt(i.block_height)), chain ? contentLink(`/app/${chain.key}/${i.txid}`, `Arcade inscription #${fmt(i.number)}`) : ""))));
  }
  function ledgerTokens(list) {
    if (!list.length) return h("div", { class: "x-empty" }, "No arcade tokens yet.");
    return table([["Token"], ["Id", 1], ["Supply", 1], ["Issuer"]], list.map((t) => ({ go: () => go("ltoken", String(t.id)),
      cells: [[h("b", {}, t.name || "(no name)")], [String(t.id), 1], [t.managed ? "managed" : t.total_shown, 1], [addrLink(t.issuer, t.issuer_tag)]] })));
  }
  async function vLedger() {
    const L = await xapi("explorer.ledger", { chain: chain.key });
    const st = [["Arcade tokens", fmt(L.tokens)], ["Arcade inscriptions", fmt(L.inscriptions)], ["Collections", fmt(L.collections)],
      ["@tags", fmt(L.tags)], ["Book orders", fmt(L.orders)], ["Trades", fmt(L.trades)]];
    return h("div", {},
      h("div", { class: "x-stats" }, st.map(([k, v]) => h("div", { class: "x-stat" }, h("div", { class: "k" }, k), h("div", { class: "v" }, v)))),
      h("div", { class: "x-cols" },
        sect("Newest arcade tokens", "ltokens", ledgerTokens(L.token_rows || [])),
        sect("Newest arcade inscriptions", "linscs", ledgerGrid(L.latest || []))),
      h("p", { class: "x-note" }, `Indexed to block ${fmt(L.indexed_to)}. Arcade tokens, NFTs and inscriptions are the arcade's own protocol (class B/C payloads), separate from ord.`));
  }
  async function vLTokens() {
    const size = 50, page = pageOf();
    const r = await xapi("explorer.ledger_tokens", { chain: chain.key, offset: (page - 1) * size, n: size });
    return sect(`Arcade tokens · ${fmt(r.total)}`, null, h("div", {}, pager(r.total, size, page, "ltokens"), ledgerTokens(r.items), pager(r.total, size, page, "ltokens")));
  }
  async function vLInscs() {
    const size = 36, page = pageOf();
    const first = await xapi("explorer.ledger_inscriptions", { chain: chain.key, n: 1 });
    const total = first.total;
    const r = await xapi("explorer.ledger_inscriptions", { chain: chain.key, before: Math.max(0, total - (page - 1) * size), n: size });
    const toPage = (num) => Math.floor((total - 1 - num) / size) + 1;
    return sect(`Arcade inscriptions · ${fmt(total)}`, null, h("div", {}, pager(total, size, page, "linscs", "inscription #", toPage), ledgerGrid(r.items), pager(total, size, page, "linscs", "inscription #", toPage)));
  }
  async function vLToken() {
    const t = await xapi("explorer.ledger_token", { chain: chain.key, id: arg });
    return h("div", {}, crumbs(link("Arcade ledger", "ledger"), link("Tokens", "ltokens"), t.name || `#${t.id}`),
      head(t.name || `Token ${t.id}`, h("span", { class: "chip arc" }, t.type === 2 ? "divisible" : "indivisible"), t.managed ? pill("managed", true) : pill("fixed supply", true)),
      kv([["Id", String(t.id)], ["Supply", t.total_shown], ["Issuer", addrLink(t.issuer, t.issuer_tag)], ["Category", [t.category, t.subcategory].filter(Boolean).join(" · ") || "—"],
        t.url && ["Url", t.url], t.data && ["Data", t.data], ["Created", link(`block ${fmt(t.creation_block)}`, "block", String(t.creation_block))], ["Creation tx", link(short(t.creation_txid, 12, 8), "tx", t.creation_txid, "mono")], ["Holders", fmt(t.holders)]]),
      h("h2", { class: "x-h2" }, "Top holders"),
      t.top.length ? table([["Holder"], ["Balance", 1]], t.top.map((x) => ({ go: () => go("address", x.address), cells: [[addrLink(x.address, x.tag)], [x.balance, 1]] }))) : h("div", { class: "x-empty" }, "Nobody holds any."));
  }
  async function vLInsc() {
    const i = await xapi("explorer.ledger_inscription", { chain: chain.key, id: arg });
    return h("div", {}, crumbs(link("Arcade ledger", "ledger"), link("Inscriptions", "linscs"), `#${fmt(i.number)}`),
      head(`Arcade inscription #${fmt(i.number)}`, h("span", { class: "chip arc" }, (i.content_type || "").split(";")[0] || "no type"), contentLink(`/app/${chain.key}/${i.txid}`, `Arcade inscription #${fmt(i.number)}`)),
      h("div", { class: "x-split" }, preview(ledgerItem(i), true),
        kv([["Txid", link(short(i.txid, 14, 8), "tx", i.txid, "mono")], ["Owner", addrLink(i.owner, i.owner_tag)], ["Creator", addrLink(i.creator, i.creator_tag)],
          ["Inscribed", link(`block ${fmt(i.block_height)}`, "block", String(i.block_height))], ["Content type", i.content_type || "—"], ["Size", `${fmt(i.content_len)} bytes · ${i.chunks} chunk${i.chunks === 1 ? "" : "s"}`],
          ["sha256", h("span", { class: "mono" }, i.sha256 || "")], i.json && ["JSON", h("span", { class: "mono" }, JSON.stringify(i.json).slice(0, 300))]])));
  }
  async function vBlock() {
    const b = await xapi("explorer.block", { chain: chain.key, id: arg });
    return h("div", {}, crumbs(link("Blocks", "blocks"), fmt(b.height)),
      head(`Block ${fmt(b.height)}`, pill(b.best ? `${fmt(b.confirmations)} confirmations` : "not on the best chain", b.best)),
      kv([["Hash", h("span", { class: "mono" }, b.hash)], ["Time", `${new Date(b.time * 1000).toLocaleString()} · ${ago(b.time)}`],
        ["Transactions", fmt(b.tx_count)], ["Size", `${fmt(b.size)} bytes`], b.inscriptions != null && ["New inscription pieces", fmt(b.inscriptions)],
        ["Previous", link(short(b.prev, 16, 8), "block", b.prev, "mono")], b.next && ["Next", link(short(b.next, 16, 8), "block", b.next, "mono")],
        // An ARCADE block has a writer and a proof of time where a coin's has proof of work.
        ...(b.arcade ? [["Written by", h("span", { class: "mono" }, b.arcade.writer)], ["Difficulty", h("span", { class: "mono" }, b.arcade.difficulty)],
          ["Proof of time", `${fmt(b.arcade.steps)} steps`]]
          : [["Bits · nonce · version", h("span", { class: "mono" }, `${b.bits} · ${b.nonce} · ${b.version}`)], ["Merkle root", h("span", { class: "mono" }, b.merkle_root)]])]),
      h("h2", { class: "x-h2" }, "Transactions"),
      table([["Txid"], [""], ["In · out"], ["Output", 1]], b.txs.map((t) => ({ go: () => go("tx", t.txid), cells: [[h("span", { class: "mono" }, short(t.txid, 18, 8))],
        [t.coinbase ? h("span", { class: "chip" }, "coinbase") : t.inscription ? h("span", { class: "chip ins" }, "inscription") : ""], [`${t.inputs} · ${t.outputs}`], [coins(t.out), 1]] }))),
      b.tx_count > b.txs.length ? h("p", { class: "x-note" }, `Showing the first ${b.txs.length} of ${fmt(b.tx_count)} transactions.`) : "");
  }
  async function vTx() {
    const t = await xapi("explorer.tx", { chain: chain.key, txid: arg });
    const io = (rows) => h("div", { class: "x-io" }, rows);
    return h("div", {}, crumbs(link("Blocks", "blocks"), t.height != null ? link(fmt(t.height), "block", String(t.height)) : "mempool", "transaction"),
      head("Transaction", pill(t.height != null ? `${fmt(t.confirmations)} confirmation${t.confirmations === 1 ? "" : "s"}` : "unconfirmed", t.height != null), pill(t.rbf ? "signals RBF" : "no RBF", !t.rbf),
        t.genesis ? h("span", { class: "chip ins" }, `inscribes ${t.genesis.split(";")[0]}`) : ""),
      kv([["Txid", h("span", { class: "mono" }, t.txid)], ["Size", `${fmt(t.size)} bytes`], t.fee != null && ["Fee", coins(t.fee)], ["Total out", coins(t.out)]]),
      h("div", { class: "x-flow" },
        io(t.inputs.map((i) => i.coinbase ? h("div", {}, h("span", { class: "chip" }, "new coins (coinbase)")) :
          h("div", {}, h("span", { class: "a" }, addrLink(i.address, i.tag)), h("span", {}, link(i.value != null ? coins(i.value) : "?", "tx", i.txid))))),
        h("div", { class: "arrow" }, "→"),
        io(t.outputs.map((o) => h("div", {}, h("span", { class: "a" }, o.op_return ? h("span", { class: "chip" }, "OP_RETURN data") : addrLink(o.address, o.tag)),
          h("span", { class: "r" }, o.inscriptions.map((id) => link("◆", "inscription", id, "ins-on")), coins(o.value), " ",
            o.spent_by ? link("spent", "tx", o.spent_by.txid, "faint") : h("span", { class: "faint" }, o.op_return ? "" : "unspent")))))));
  }
  async function vAddress() {
    const a = await xapi("explorer.address", { chain: chain.key, address: arg });
    return h("div", {}, crumbs("Address"),
      head(short(a.address, 14, 8), a.tag ? h("span", { class: "chip arc" }, "@" + a.tag) : ""),
      a.note ? h("p", { class: "note" }, a.note) : "",
      kv([["Address", h("span", { class: "mono" }, a.address)], a.balance != null && ["Balance", coins(a.balance)], a.tx_count != null && ["Transactions", fmt(a.tx_count)],
        a.inscription_count != null && ["Inscriptions held", fmt(a.inscription_count)]]),
      a.tokens && a.tokens.length ? h("div", {}, h("h2", { class: "x-h2" }, "Tokens"), table([["Tick"], ["Balance", 1], ["Transferable", 1]],
        a.tokens.map((t) => ({ go: () => go("token", t.tick), cells: [[h("b", {}, t.tick)], [units(t.overall, t.dec), 1], [units(t.transferable, t.dec), 1]] })))) : "",
      a.inscriptions && a.inscriptions.length ? h("div", {}, h("h2", { class: "x-h2" }, "Inscriptions"), insGrid(a.inscriptions)) : "",
      // The arcade ledger: arcade tokens and arcade inscriptions this address holds.
      a.ledger_tokens && a.ledger_tokens.length ? h("div", {}, h("h2", { class: "x-h2" }, "Arcade tokens"), table([["Token"], ["Id", 1], ["Balance", 1]],
        a.ledger_tokens.map((t) => ({ go: () => go("ltoken", String(t.propertyid)), cells: [[h("b", {}, t.name || `#${t.propertyid}`)], [fmt(t.propertyid), 1], [t.balance, 1]] })))) : "",
      a.ledger_inscriptions && a.ledger_inscriptions.length ? h("div", {}, h("h2", { class: "x-h2" }, `Arcade inscriptions (${fmt(a.ledger_inscription_count)})`),
        table([["Number"], ["Type"], ["Txid"]], a.ledger_inscriptions.map((i) => ({ go: () => go("linsc", i.txid),
          cells: [[`#${fmt(i.number)}`], [(i.content_type || "").split(";")[0]], [h("span", { class: "mono" }, short(i.txid || "", 16, 8))]] })))) : "",
      a.history ? h("h2", { class: "x-h2" }, "History") : "",
      !a.history ? "" : a.history.length ? table([["Block"], ["Txid"], ["Received", 1], ["Sent", 1]], a.history.map((r) => ({ go: r.txid ? () => go("tx", r.txid) : null,
        cells: [[fmt(r.height)], [h("span", { class: "mono" }, short(r.txid || "", 16, 8))], [r.received ? coins(r.received) : "", 1], [r.sent ? coins(r.sent) : "", 1]] })))
        : h("div", { class: "x-empty" }, "No transactions."));
  }
  async function vInscription() {
    const i = await xapi("explorer.inscription", { chain: chain.key, id: arg, ...(await imgArgs()) });
    const ct = (i.ct || "").split(";")[0];
    // "In collection: Name #k" (collections index, here or on the mesh).
    const inColl = has("collections") ? (await xapi("explorer.collection_of", { chain: chain.key, id: i.id }).catch(() => null))?.collections || [] : [];
    const collRow = inColl.length ? ["In collection", h("span", {}, ...inColl.flatMap((x, k) => [k ? " · " : "", link(x.number != null ? `${x.name} #${fmt(x.number)}` : x.name, "collection", x.kind === "known" ? `known:${x.slug}` : x.id)]))] : null;
    return h("div", {}, crumbs(link("Inscriptions", "inscriptions"), `#${fmt(i.n)}`),
      head(`Inscription #${fmt(i.n)}`, h("span", { class: "chip ins" }, ct || "no type"), i.burned != null ? pill("burned", false) : "", i.from_arcade || i._mesh ? "" : contentLink(`/content/${i.id}`, `Inscription #${fmt(i.n)}`)),
      h("div", { class: "x-split" }, preview(i, true),
        kv([["Id", h("span", { class: "mono" }, i.id)], collRow, ["Holder", i.owner ? addrLink(i.owner, i.owner_tag) : h("span", { class: "faint" }, i.burned != null ? `burned at ${fmt(i.burned)}` : "—")],
          i.at && ["Location", link(short(i.at, 12, 10), "tx", i.at.split(":")[0], "mono")], ["Inscribed", link(`block ${fmt(i.h)}`, "block", String(i.h))],
          ["Content type", i.ct || "—"], ["Size", `${fmt(i.size)} bytes · ${i.pieces} piece${i.pieces === 1 ? "" : "s"}`], i.proto && ["Protocol", i.proto],
          i.delegate && ["Delegate", link(short(i.delegate, 12, 6), "inscription", i.delegate, "mono")], i.parent && ["Parent", link(short(i.parent, 12, 6), "inscription", i.parent, "mono")],
          i.re && ["Reinscription", "yes"], ["Reveal", link(short(i.txs?.[i.txs.length - 1] || "", 12, 8), "tx", i.txs?.[i.txs.length - 1], "mono")]])));
  }
  async function vToken() {
    // The arg is the tick, with "|p<n>" for a page of its holders past the first.
    const m = /^(.*)\|p(\d+)$/s.exec(arg || "");
    const tick = m ? m[1] : arg, size = 25, hpage = m ? Math.max(1, Number(m[2])) : 1;
    const t = await xapi("explorer.token", { chain: chain.key, tick, offset: (hpage - 1) * size, n: size });
    const total = t.total ?? t.top.length;
    const hpager = () => total > size ? pager(total, size, hpage, "token", null, null, (p) => (p > 1 ? `${tick}|p${p}` : tick)) : "";
    const pct = Number(BigInt(t.minted || "0") * 1000n / (BigInt(t.supply || "1") || 1n)) / 10;
    return h("div", {}, crumbs(link("Tokens", "tokens"), t.shown || t.tick),
      head(t.shown || t.tick, pct >= 100 ? pill("minted out") : pill(`${pct}% minted`, true)),
      kv([["Supply", units(t.supply, t.dec)], ["Minted", units(t.minted, t.dec)], ["Mint limit", units(t.lim, t.dec)], ["Decimals", String(t.dec)],
        ["Deploy", link(`inscription #${fmt(t.n)}`, "inscription", t.id)], ["Deployer", addrLink(t.deployer)], ["Holders", t.holders]]),
      h("h2", { class: "x-h2" }, "Holders"),
      t.top.length ? h("div", {}, hpager(),
        table([["#", 1], ["Holder"], ["Balance", 1], ["Transferable", 1]], t.top.map((x, i) => ({ go: () => go("address", x.address), cells: [[fmt((hpage - 1) * size + i + 1), 1], [addrLink(x.address, x.tag)], [units(x.overall, t.dec), 1], [units(x.transferable, t.dec), 1]] }))),
        hpager())
        : h("div", { class: "x-empty" }, hpage > 1 ? "No holders on this page." : "Nobody holds any yet."));
  }
  // ---------- collections ----------
  // A collection's cover (an inscription on this chain): its image from this arcade.
  function cover(id, alt) {
    const t = h("div", { class: "x-thumb" });
    if (id && httpBase && chain) {
      const img = h("img", { src: `${httpBase}/ord/${chain.key}/${id}`, alt, loading: "lazy", decoding: "async" });
      img.onerror = () => img.replaceWith(h("div", { class: "x-ph" }, "no cover"));
      t.append(img);
    } else t.append(h("div", { class: "x-ph" }, "no cover"));
    return t;
  }
  let collSort = "newest";
  async function vCollections() {
    const size = 24, page = pageOf();
    const r = await xapi("explorer.collections", { chain: chain.key, sort: collSort, offset: (page - 1) * size, n: size, ...(await imgArgs()) });
    const sorts = h("div", { class: "x-types" }, [["newest", "Newest"], ["oldest", "Oldest"], ["name", "Name"], ["size", "Size"]].map(([k, t]) =>
      h("button", { type: "button", class: "chip-btn" + (collSort === k ? " on" : ""), onclick: () => { collSort = k; go("collections", null); } }, t)));
    const note = r.reading ? h("p", { class: "x-note" }, `Still reading: ${r.note}`) : h("p", { class: "x-note" }, r.note || "");
    const grid = r.items.length ? h("div", { class: "x-grid" }, r.items.map((c) => h("div", { class: "x-ins", onclick: () => go("collection", c.kind === "known" ? `known:${c.slug}` : c.id) },
      c.cover_content ? preview({ content: c.cover_content, ct: c.cover_ct, n: c.name }) : cover(c.cover, c.name),
      h("div", { class: "cap" }, h("b", {}, c.name), h("span", { class: "faint" }, c.kind === "known" && c.state !== "held" ? `${fmt(c.supply)} · Nintondo` : `${fmt(c.count)}${c.supply != null && c.count !== c.supply ? ` of ${fmt(c.supply)}` : ""}`)))))
      : h("div", { class: "x-empty" }, "No collections on this chain yet.");
    // Nintondo's known collections (Dogecoin, Pepecoin, Bellscoin): asked only when switched on here.
    const known = r.known ? h("p", { class: "x-note" }, h("label", {}, h("input", { type: "checkbox", checked: r.known.asks_nintondo, onchange: async (ev) => {
        try { await api("collections.known_set", { on: ev.target.checked }); } catch (e) { alert(e.message); } render(); } }),
      " Ask Nintondo for its known collections (the list once a day; a collection's pieces when it is opened)"), r.known.note ? ` · ${r.known.note}` : "") : "";
    return sect(`Collections · ${fmt(r.total)}`, null, h("div", {}, sorts, note, known, pager(r.total, size, page, "collections"), grid, pager(r.total, size, page, "collections")));
  }
  async function vCollection() {
    // The arg is the collection's id, with "|p<n>" for a page of its items past the first.
    const m = /^(.*)\|p(\d+)$/s.exec(arg || "");
    const id = m ? m[1] : arg, size = 48, cpage = m ? Math.max(1, Number(m[2])) : 1;
    const which = id.startsWith("known:") ? { slug: id.slice(6) } : { id };
    const c = await xapi("explorer.collection", { chain: chain.key, ...which, offset: (cpage - 1) * size, n: size, ...(await imgArgs()) });
    const cpager = () => pager(c.total || 0, size, cpage, "collection", null, null, (p) => (p > 1 ? `${id}|p${p}` : id));
    const items = c.items.length ? h("div", { class: "x-grid" }, c.items.map((it) => h("div", { class: "x-ins", onclick: () => go("inscription", it.id) },
      it.content ? preview(it) : h("div", { class: "x-thumb" }, h("div", { class: "x-ph" }, "not held here")),
      h("div", { class: "cap" }, h("b", {}, `#${fmt(it.number)}`), h("span", { class: "faint" }, it.n != null ? `insc. ${fmt(it.n)}` : "")),
      Object.keys(it.traits || {}).length ? h("div", { class: "cap faint", title: Object.entries(it.traits).map(([k, v]) => `${k}: ${v}`).join("\n") }, Object.values(it.traits).join(" · ")) : "")))
      : h("div", { class: "x-empty" }, c.loading ? (c.loading.state === "skipped" ? `Not kept: ${c.loading.why}` : !c.loading.asks_nintondo ? "Not held here: this arcade doesn't ask Nintondo (switch it on in the Collections list)."
          : `Loading from Nintondo: ${fmt(c.loading.have || 0)} of ${fmt(c.loading.of || 0)} pieces so far. Come back in a moment.`) : "No items on this page.");
    const status = c.status !== "active" ? pill(c.status, false) : "";
    return h("div", {}, crumbs(link("Collections", "collections"), c.name), head(c.name, status, h("span", { class: "chip" }, c.kind === "sub1k" ? "built in" : c.kind === "known" ? "Nintondo's list" : "on the chain")),
      kv([c.description && ["About", c.description], ["Items", c.kind === "known" ? `${fmt(c.count || 0)} held · ${fmt(c.supply)} listed` : `${fmt(c.count)} of ${fmt(c.supply)}${c.missing ? ` · ${fmt(c.missing)} numbers not listed` : ""}`],
        c.kind === "known" && c.owners != null && ["Holders", fmt(c.owners)], c.kind === "known" && c.fp && ["Fingerprint", h("span", { class: "mono" }, short(c.fp, 12, 6))],
        c.kind === "registry" && ["Published by", c.publisher ? addrLink(c.publisher) : "—"], c.kind === "registry" && ["Root", link(`inscription #${fmt(c.number)}`, "inscription", c.id)],
        c.height != null && ["Block", link(fmt(c.height), "block", String(c.height))], c.replaces && ["Replaces", link(short(c.replaces, 12, 6), "collection", c.replaces, "mono")],
        (c.traits || []).length && ["Traits", c.traits.join(", ")], c.reason && ["Why", c.reason]]),
      cpager(), items, cpager());
  }
  let lastResults = null;
  async function vResults() {
    const r = lastResults && lastResults.id === arg ? lastResults : await xapi("explorer.search", { chain: chain.key, q: arg });
    if (r.kind !== "results") { go(r.kind, r.id); return h("div"); }
    return sect(`Results for "${r.id}" on ${chain.name}`, null, table([["Match"], ["What"]], r.items.map((it) => ({ go: () => go(it.kind, it.id),
      cells: [[h("span", { class: "mono" }, it.label)], [it.what]] }))));
  }
  async function vTag() {
    const t = await xapi("explorer.tag", { chain: chain.key, tag: arg });
    const body = t.addresses.length
      ? table([["Address"], ["Why"], ["Balance", 1]], t.addresses.map((a) => ({ go: () => go("address", a.address),
          cells: [[h("span", { class: "mono" }, a.address)], [a.why], [a.balance == null ? "—" : coins(a.balance), 1]] })))
      : h("div", { class: "x-empty" }, `@${t.tag} hasn't published any ${chain.name} address in their profile. Their @tag lives on Pepecoin testnet at ${t.home_address}.`);
    return h("div", {}, crumbs("@tags", "@" + t.tag), head("@" + t.tag, h("span", { class: "chip arc" }, `claimed at block ${fmt(t.claimed_at)}`)),
      t.profile && (t.profile.name || t.profile.bio) ? h("p", { class: "x-note" }, [t.profile.name, t.profile.bio, t.profile.url].filter(Boolean).join(" · ")) : "",
      h("h2", { class: "x-h2" }, `Addresses on ${chain.name}`), body);
  }
  async function vName() {
    const n = await xapi("explorer.name", { chain: chain.key, name: arg });
    return h("div", {}, crumbs(link("Names", "names"), n.name), head(n.name),
      table([["Reading"], ["Winning inscription"], ["Holder"]], n.readings.map((r) => ({ cells: [[r.rule], [link(`#${fmt(r.number)}`, "inscription", r.id)], [r.owner ? addrLink(r.owner, r.owner_tag) : "—"]] }))),
      h("p", { class: "x-note" }, "Each reading has its own winner: the lowest inscription number with that name. The holder is whoever holds the claim inscription now."));
  }
  async function vMap() {
    const m = await xapi("explorer.map", { chain: chain.key, claim: arg });
    return h("div", {}, crumbs(link("Maps", "maps"), m.claim), head(m.claim),
      kv([["Winning inscription", link(`#${fmt(m.number)}`, "inscription", m.id)], ["Holder", m.owner ? addrLink(m.owner, m.owner_tag) : "—"], ["Block", link(m.claim.split(".")[0], "block", m.claim.split(".")[0])]]));
  }

  // ---------- DogeFeed (D://) ----------
  // Every D:// record (DogeFeed's social protocol on Dogecoin) as the dogefeed index reads and checks it: posts,
  // comments and quotes with their counts, likes, branches, follows, tips, and who stamped each (the registrar).
  const DF_OPS = ["all", "post", "comment", "quote", "like", "branch", "follow", "tip", "reveal", "delete", "edit", "keylink", "keyrevoke"];
  const DF_PLURAL = { all: "All", branch: "Branches", reveal: "Reveals", keylink: "Key links", keyrevoke: "Key revokes" };
  const dfDoge = (k) => coinNum(k || 0, "en-US") + " DOGE";
  const dfWhen = (ev) => ev.ts ? new Date(ev.ts).toLocaleString() : "";
  const contentUrl = (id) => (httpBase || "") + "/content/" + id;
  const dfShort = (a) => short(a || "", 6, 4);
  const dfAvatar = (a) => { let x = 0; for (const c of a || "") x = (x * 31 + c.charCodeAt(0)) % 360; return h("span", { class: "df-av", style: `background:hsl(${x} 45% 45%)` }, (a || "?").slice(1, 2).toUpperCase()); };
  const dfStamp = (r) => r ? h("span", { class: "chip df-reg", title: `${r.kid} · ${r.address}` }, (r.source === "dogefeed" ? "DogeFeed" : r.source === "this arcade" ? "this arcade" : r.name || r.kid) + " · " + r.kid) : "";
  const dfText = (t) => {
    const out = [];
    String(t || "").split(/(#[\w]+|@[\w.]+)/g).forEach((p) => out.push(/^[#@]/.test(p) ? h("b", { class: "df-tag" }, p) : p));
    return h("div", { class: "df-text" }, ...out);
  };
  const dfTarget = (ev) => {
    const t = ev.target;
    if (!t) return "";
    const isPost = /i\d+$/.test(t);
    const verb = { comment: "↳ replying to", quote: "❝ quoting", like: "♥ liked", branch: "⑂ branched", tip: "🪙 tipped", reveal: "🔓 revealed", delete: "✕ deleted", edit: "✎ edited", follow: "+ followed", keylink: "🔑 linked key", keyrevoke: "🔑 revoked key" }[ev.type] || "→";
    return h("div", { class: "df-ref" }, verb, " ", isPost ? link(short(t, 8, 6), "dfpost", t, "mono") : link(dfShort(t), "dfaddr", t, "mono"));
  };
  function dfCard(ev) {
    const media = (ev.media || []).length ? h("div", { class: "df-media" }, ...(ev.media || []).map((m) => h("img", { src: contentUrl(m), loading: "lazy", alt: "" }))) : "";
    const c = ev.counts;
    const foot = h("div", { class: "df-foot" },
      c ? h("span", {}, "♥ ", h("b", {}, fmt(c.likes)), "  💬 ", h("b", {}, fmt(c.comments)), "  ❝ ", h("b", {}, fmt(c.quotes)), c.tips && c.tips.n ? ["  🪙 ", h("b", {}, c.tips.doge)] : "") : "",
      ev.valid ? h("span", { class: "df-ok" }, "✓ valid") : h("span", { class: "df-bad", title: ev.reason || "" }, "✕ " + (ev.reason || "invalid")),
      dfStamp(ev.registrar),
      ev.amount ? h("span", {}, dfDoge(ev.amount), " to ", dfShort(ev.recipient)) : "",
      ev.status === "pending" ? h("span", { class: "chip" }, `pending · ${ev.confirmations} conf`) : "");
    return h("div", { class: "df-card" + (ev.valid ? "" : " bad"), onclick: () => go(ev.inscription ? "dfpost" : "dfpost", ev.inscription || ev.txid) },
      h("div", { class: "df-who" }, dfAvatar(ev.account || ev.author), link(dfShort(ev.account || ev.author), "dfaddr", ev.account || ev.author, "mono df-h"),
        h("span", { class: "df-op " + ev.type }, ev.type), h("span", { class: "df-t" }, `#${fmt(ev.height)}`, ev.deleted ? " · deleted" : "")),
      dfTarget(ev), ev.text ? dfText(ev.text) : "", media,
      (ev.tags || []).length ? h("div", { class: "df-tags" }, ...(ev.tags || []).map((t) => h("b", { class: "df-tag" }, "#" + t))) : "", foot);
  }
  let dfMore = null;
  async function dfPage(cmd, args, into, empty) {
    const r = await xapi(cmd, args);
    const list = r.activity || r;
    const box = into || h("div", { class: "df-feed" });
    (list.items || []).forEach((ev) => box.append(dfCard(ev)));
    if (!(list.items || []).length && !into) box.append(h("div", { class: "x-empty" }, empty));
    const more = list.next ? h("div", { class: "row" }, h("button", { onclick: async (e) => { e.target.disabled = true; e.target.textContent = "Loading…"; const m = await dfPage(cmd, { ...args, cursor: list.next }, box); e.target.parentNode.replaceWith(m.more || ""); } }, "Load more")) : "";
    return { box, more, r };
  }
  async function vDogeFeed() {
    await imgArgs();
    const type = DF_OPS.includes(arg) ? arg : "all";
    const keys = await xapi("explorer.dogefeed_keys", { chain: chain.key }).catch(() => ({ registrars: [] }));
    const chips = h("div", { class: "df-chips" }, ...DF_OPS.map((o) => h("button", { class: "df-chip" + (o === type ? " on" : ""), onclick: () => go("dogefeed", o === "all" ? null : o) }, DF_PLURAL[o] || o[0].toUpperCase() + o.slice(1) + "s")));
    const { box, more } = await dfPage("explorer.dogefeed", { chain: chain.key, limit: 30, ...(type === "all" ? {} : { type }) }, null, "No D:// records yet.");
    const regs = (keys.registrars || []).map((r) => h("span", { class: "chip df-reg", title: r.address }, (r.source === "dogefeed" ? "DogeFeed" : r.source === "this arcade" ? "This arcade" : r.name || r.kid) + " · " + r.kid));
    return h("div", {},
      sect("DogeFeed", null, h("div", {},
        h("p", { class: "x-note" }, "D://, DogeFeed's social protocol on Dogecoin, checked here as DogeFeed's guide says: the author's signature, a registrar's stamp, and the fees. Registrars counted here: ", ...regs),
        chips, box, more)));
  }
  async function vDfPost() {
    await imgArgs();
    const id = String(arg || "");
    const isInsc = /i\d+$/.test(id);
    const ev = isInsc ? await xapi("explorer.dogefeed_inscription", { chain: chain.key, id, activity: true, limit: 50 })
      : await xapi("explorer.dogefeed_tx", { chain: chain.key, txid: id });
    const title = { post: "Post", comment: "Comment", quote: "Quote", edit: "Edit", keylink: "Key link" }[ev.type] || (ev.type || "Record")[0].toUpperCase() + (ev.type || "record").slice(1);
    const c = ev.counts;
    const pays = [ev.fee != null && ["Registrar fee", dfDoge(ev.fee)], ev.amount && [ev.type === "comment" || ev.type === "quote" ? "Paid to the parent's author" : "Paid to " + (ev.type === "follow" ? "the followed account" : "the author"), h("span", {}, dfDoge(ev.amount), " → ", ev.recipient ? link(dfShort(ev.recipient), "dfaddr", ev.recipient, "mono") : "")]].filter(Boolean);
    const rows = [
      ["Status", ev.valid ? h("span", { class: "df-ok" }, "✓ valid D:// record") : h("span", { class: "df-bad" }, "✕ not a valid D:// record: " + (ev.reason || ""))],
      ["Author", h("span", {}, link(ev.author || "", "dfaddr", ev.author, "mono"), ev.account && ev.account !== ev.author ? [" for ", link(ev.account, "dfaddr", ev.account, "mono")] : "")],
      ev.registrar && ["Stamped by", h("span", {}, dfStamp(ev.registrar), " ", h("span", { class: "mono faint" }, ev.registrar.address))],
      ["Block", h("span", {}, link(fmt(ev.height), "block", String(ev.height)), ` · ${fmt(ev.confirmations)} confirmations (${ev.status})`)],
      ["Transaction", link(ev.txid, "tx", ev.txid, "mono")],
      ev.inscription && ["Inscription", link(ev.inscription, "inscription", ev.inscription, "mono")],
      ev.ts && ["Signed at", dfWhen(ev)],
      ev.target && ["Target", dfTarget(ev)],
      ...pays,
      c && ["Counts", h("span", {}, `♥ ${fmt(c.likes)} · ⑂ ${fmt(c.branches)} · ❝ ${fmt(c.quotes)} · 💬 ${fmt(c.comments)} · 🪙 ${fmt(c.tips.n)} (${c.tips.doge} DOGE) · 🔓 ${fmt(c.reveals.n)}`)],
      ev.deleted && ["Deleted", "by its author (the bytes stay on chain)"],
      ev.latestEdit && ["Latest edit", link(ev.latestEdit, "dfpost", ev.latestEdit, "mono")],
    ];
    const long = ev.bytes > 240 ? h("div", { class: "df-warn" }, h("b", {}, "Stored past its declared content. "), `DogeFeed's envelope declares one piece, so standard Doginals readers (and the Inscriptions tab) show the first 240 bytes; this index joins every chunk: ${fmt(ev.bytes)} bytes.`) : "";
    const body = [crumbs(link("DogeFeed", "dogefeed"), title), head(title, h("span", { class: "df-op " + ev.type }, ev.type)),
      h("div", { class: "df-card df-big" }, h("div", { class: "df-who" }, dfAvatar(ev.account || ev.author), link(dfShort(ev.account || ev.author), "dfaddr", ev.account || ev.author, "mono df-h"), h("span", { class: "df-t" }, dfWhen(ev))),
        dfTarget(ev), ev.text ? dfText(ev.text) : "", (ev.media || []).length ? h("div", { class: "df-media" }, ...(ev.media || []).map((m) => h("img", { src: contentUrl(m), alt: "" }))) : ""),
      kv(rows), long];
    if (ev.activity) {
      const box = h("div", { class: "df-feed" });
      (ev.activity.items || []).forEach((x) => box.append(dfCard(x)));
      body.push(sect(`Activity on it`, null, (ev.activity.items || []).length ? box : h("div", { class: "x-empty" }, "No likes, replies or tips yet.")));
    }
    if (ev.record) body.push(sect("Record (all chunks joined)", null, h("pre", { class: "df-json" }, JSON.stringify(ev.record, null, 1))));
    return h("div", {}, ...body);
  }
  async function vDfAddr() {
    await imgArgs();
    const a = String(arg || "");
    const dir = (location.hash.includes("dir=in")) ? "in" : "out";
    const r = await xapi("explorer.dogefeed_address", { chain: chain.key, address: a, limit: 30 });
    const c = r.counts || {};
    const stat = (k, v) => h("div", { class: "df-stat" }, h("small", {}, k), h("b", {}, v));
    const stats = h("div", { class: "df-stats" }, stat("Posts", fmt(c.posts)), stat("Comments", fmt(c.comments)), stat("Quotes", fmt(c.quotes)),
      stat("Followers", fmt(c.followers)), stat("Following", fmt(c.following)), stat("Likes given", fmt(c.likes_given)), stat("Likes received", fmt(c.likes_received)),
      stat("Tips sent", (c.tips_sent || "0") + " DOGE"), stat("Tips received", (c.tips_received || "0") + " DOGE"));
    const { box, more } = await dfPage("explorer.dogefeed_address", { chain: chain.key, address: a, counts: false, limit: 30 }, null, "Nothing on D:// from this address yet.");
    const inBtn = h("button", { class: "df-chip", onclick: async (e) => { const m = await dfPage("explorer.dogefeed_address", { chain: chain.key, address: a, counts: false, direction: "in", limit: 30 }, null, "Nothing on D:// paid this address yet."); e.target.closest(".x-sect").querySelector(".df-feed").replaceWith(m.box); } }, "What paid it");
    return h("div", {}, crumbs(link("DogeFeed", "dogefeed"), dfShort(a)), head(dfShort(a), link("Address page", "address", a)),
      h("p", { class: "mono x-note" }, a), stats,
      (c.linked_keys || []).length ? sect("Linked keys", null, kv(c.linked_keys.map((k) => [k.label || "key", h("span", { class: "mono" }, k.key, " · ", (k.ops || []).join(", "))]))) : "",
      sect("Activity", null, h("div", {}, h("div", { class: "df-chips" }, h("button", { class: "df-chip on" }, "What it did"), inBtn), box, more)));
  }
  const VIEWS = { collections: vCollections, collection: vCollection, home: vHome, blocks: vBlocks, mempool: vMempool, inscriptions: vInscriptions, tokens: vTokens, names: vNames, maps: vMaps, dogefeed: vDogeFeed, dfpost: vDfPost, dfaddr: vDfAddr, ledger: vLedger,
    block: vBlock, tx: vTx, address: vAddress, inscription: vInscription, token: vToken, name: vName, map: vMap,
    ltokens: vLTokens, linscs: vLInscs, ltoken: vLToken, linsc: vLInsc, tag: vTag, results: vResults };

  // ---------- navigation ----------
  function paintNav() {
    const cur = DETAIL_PARENT[page] || page;
    $("x-nav").replaceChildren(...SECTIONS.filter(([, , ix]) => !ix || has(ix)).map(([k, t]) => h("button", { class: cur === k ? "on" : "", onclick: () => go(k) }, t)));
    const hint = ["block height or hash", "txid", "address", "@tag"];
    if (has("ord")) hint.push("inscription #");
    if (has("names")) hint.push("name");
    if (has("maps")) hint.push("map");
    if (has("tokens")) hint.push("tick");
    $("x-q").placeholder = hint.join(", ") + "…";
  }
  async function render() {
    if (!chain) await loadChains();
    if (!chain) { $("x-view").replaceChildren(h("div", { class: "x-empty" }, "Waiting for arcaded…")); return; }
    const my = ++seq;
    paintNav();
    try {
      if (!sum || sum.chain !== chain.key) sum = await xapi("explorer.summary", { chain: chain.key });
      const sumFrom = sum._mesh || (sum.mesh_parts ? Object.values(sum.mesh_parts)[0] : null);
      // A page whose index is off on this chain isn't available: back to the overview.
      if (NEEDS[page] && !has(NEEDS[page])) { page = "home"; arg = null; }
      if (!VIEWS[page]) { page = "home"; arg = null; }
      meshFrom = page === "home" ? sumFrom : null;
      const v = await VIEWS[page]();
      if (my !== seq) return;
      $("x-view").replaceChildren(backBar(), v);
      paintNav();
    } catch (e) {
      if (my !== seq) return;
      $("x-view").replaceChildren(backBar(), h("div", { class: "x-empty bad" }, e.message), h("div", { class: "row" }, h("button", { onclick: () => go("home") }, "Back to the overview")));
    }
  }
  // Where you've been (Martin 2026-11-10: always a Back to the page you were on): every move
  // through go() is kept, with the chain, the type filter and where the page was scrolled to.
  const HIST = [];
  // The page of each list you were last on (per chain): going up to a list from a detail page
  // (its section button or its crumb) opens that page, not the first one.
  const LAST = {};
  const here = () => ({ chain: chain?.key, page, arg, insType, y: window.scrollY });
  function remember() { if (chain) { HIST.push(here()); if (HIST.length > 200) HIST.shift(); } }
  function go(p, a = null, keep = true) {
    if (a === null && DETAIL_PARENT[page] && chain && LAST[`${chain.key}|${p}`] != null) a = LAST[`${chain.key}|${p}`];
    if (keep) remember();
    page = p; arg = a;
    if (chain && !DETAIL_PARENT[p]) LAST[`${chain.key}|${p}`] = a;
    render(); window.scrollTo({ top: 0 });
  }
  async function back() {
    const s = HIST.pop();
    if (!s) return;
    if (s.chain && chain && s.chain !== chain.key) {
      const c = chains.find((x) => x.key === s.chain);
      if (c) { chain = c; sum = null; paintSelector(); announce(); }
    }
    page = s.page; arg = s.arg; insType = s.insType; oldKey = "";
    await render();
    window.scrollTo({ top: s.y || 0 });
  }
  const backBar = () => HIST.length ? h("div", { class: "x-back" }, h("button", { class: "pg", onclick: back, title: "Back to the page you were on (Alt+←)" }, "← Back")) : "";
  // Alt+Left and the mouse's back button go back too, while the Explorer is open.
  document.addEventListener("keydown", (ev) => { if (ev.altKey && ev.key === "ArrowLeft" && $("explorer").classList.contains("on")) { ev.preventDefault(); back(); } });
  document.addEventListener("mouseup", (ev) => { if (ev.button === 3 && $("explorer").classList.contains("on")) { ev.preventDefault(); back(); } });

  $("x-q").addEventListener("keydown", async (ev) => {
    if (ev.key !== "Enter") return;
    const q = ev.target.value.trim();
    if (!q || !chain) return;
    try {
      const r = await xapi("explorer.search", { chain: chain.key, q });
      if (NEEDS[r.kind] && !has(NEEDS[r.kind])) throw new Error(`Turn on the ${NEEDS[r.kind]} index for ${chain.name} in Chains & Coins to look up ${q}.`);
      if (r.kind === "results") { lastResults = r; go("results", r.id); return; }
      go(r.kind, r.id);
    } catch (e) { $("x-view").replaceChildren(h("div", { class: "x-empty" }, `${chain.name}: ${e.message}`)); }
  });

  document.querySelector('nav button[data-tab="explorer"]').addEventListener("click", () => { sum = null; render(); });
  // Keep the overview fresh while it is open.
  setInterval(() => { if ($("explorer").classList.contains("on") && page === "home" && !document.hidden) { sum = null; render(); } }, 30000);
  setInterval(loadChains, 15000);
  loadChains();
  // #explorer/<page>/<arg> opens that page.
  const hp = (location.hash || "").slice(1).split("/");
  if (hp[0] === "explorer") { if (hp[1]) { page = hp[1]; arg = hp.slice(2).join("/") || null; } render(); }
  window.arcadeExplorer = { go, chain: () => chain };
  window.arcadeChain = () => chain;
})();
