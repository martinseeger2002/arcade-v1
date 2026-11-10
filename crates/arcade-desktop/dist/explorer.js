// Explorer tab + the chain selector in the header. Everything is read from this arcade's own
// chain.mdbx through arcaded's explorer.* API: no outside service.
(() => {
  // A chain's coin: the original arcade's icon, or a letter on the chain's colour.
  const coinEl = (c) => coinLogo(c.key, c.name, "span", "coin");
  const SECTIONS = [
    ["home", "Overview"], ["blocks", "Blocks"], ["mempool", "Mempool"], ["inscriptions", "Inscriptions", "ord"],
    ["tokens", "Tokens", "tokens"], ["names", "Names", "names"], ["maps", "Maps", "maps"], ["ledger", "Arcade ledger", "ledger"],
    // Runes and dunes join here (with their index keys) when those indexes exist.
  ];
  // Pages and the index each needs; a page comes online only when its index is on the chain.
  const NEEDS = { inscriptions: "ord", inscription: "ord", tokens: "tokens", token: "tokens", names: "names", name: "names", maps: "maps", map: "maps", ledger: "ledger", ltokens: "ledger", linscs: "ledger", ltoken: "ledger", linsc: "ledger" };
  const DETAIL_PARENT = { block: "blocks", tx: "blocks", address: "home", inscription: "inscriptions", token: "tokens", name: "names", map: "maps", ltokens: "ledger", linscs: "ledger", ltoken: "ledger", linsc: "ledger", tag: "home", results: "home" };

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
  function pick(c) { chain = c; close(); paintSelector(); sum = null; go("home"); announce(); }
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
    if (c.kind === "image") t.append(h("img", { src: c.data, alt: `inscription ${i.n}`, loading: "lazy" }));
    else if (c.kind === "text") t.append(h("pre", {}, c.text.slice(0, big ? 20000 : 400)));
    else t.append(h("div", { class: "x-ph" }, c.kind === "html" ? "HTML (not run here)" : c.kind === "missing" ? "content not stored" : (c.type || "binary")));
    if (!big) t.append(h("span", { class: "tag" }, (i.ct || "").split(";")[0] || "no type"));
    return t;
  }
  function blockRows(list) {
    return h("div", { class: "x-list" }, list.map((b) => h("div", { class: "x-blk", onclick: () => go("block", String(b.height)) },
      h("div", { class: "x-hbox" }, h("b", {}, fmt(b.height)), h("small", {}, ago(b.time))),
      h("div", { class: "x-mid" }, h("div", { class: "x-hash" }, b.hash),
        h("div", { class: "chips" }, h("span", { class: "chip" }, `${b.txs} tx`), b.inscriptions ? h("span", { class: "chip ins" }, `${b.inscriptions} inscription${b.inscriptions > 1 ? "s" : ""}`) : "")))));
  }
  function insGrid(list) {
    if (!list.length) return h("div", { class: "x-empty" }, "No inscriptions yet.");
    return h("div", { class: "x-grid" }, list.map((i) => h("div", { class: "x-ins", onclick: () => go("inscription", i.id) }, preview(i),
      h("div", { class: "cap" }, h("b", {}, `#${fmt(i.n)}`), h("span", { class: "faint" }, fmt(i.h))))));
  }
  function table(head, rows) {
    return h("div", { class: "x-tbl" }, h("table", {}, h("thead", {}, h("tr", {}, head.map(([t, num]) => h("th", { class: num ? "num" : "" }, t)))),
      h("tbody", {}, rows.map((r) => h("tr", { class: r.go ? "click" : "", onclick: r.go || null }, r.cells.map(([v, num]) => h("td", { class: num ? "num" : "" }, v)))))));
  }
  // Pages: the page number lives in the address arg as "p<N>" (1 = newest).
  const pageOf = () => (arg && /^p\d+$/.test(arg) ? Math.max(1, Number(arg.slice(1))) : 1);
  function pager(total, size, page, where, gotoLabel, gotoPage) {
    const pages = Math.max(1, Math.ceil(total / size));
    page = Math.min(page, pages);
    const btn = (label, p, on) => h("button", { class: "pg" + (on ? " on" : ""), disabled: p < 1 || p > pages || (on && true), onclick: () => go(where, `p${p}`) }, label);
    const nums = [];
    const lo = Math.max(1, page - 2), hi = Math.min(pages, page + 2);
    if (lo > 1) { nums.push(btn("1", 1)); if (lo > 2) nums.push(h("span", { class: "pg-gap" }, "…")); }
    for (let i = lo; i <= hi; i++) nums.push(btn(fmt(i), i, i === page));
    if (hi < pages) { if (hi < pages - 1) nums.push(h("span", { class: "pg-gap" }, "…")); nums.push(btn(fmt(pages), pages)); }
    const box = h("input", { type: "text", inputmode: "numeric", placeholder: gotoLabel || "page", "aria-label": gotoLabel || "Go to page" });
    const jump = () => {
      const v = Number(String(box.value).replace(/[,#\s]/g, ""));
      if (!Number.isFinite(v) || box.value === "") return;
      const p = gotoPage ? gotoPage(v) : v;
      go(where, `p${Math.min(pages, Math.max(1, p))}`);
    };
    box.onkeydown = (e) => { if (e.key === "Enter") jump(); };
    return h("div", { class: "x-pager" },
      h("div", { class: "pg-nums" }, btn("« First", 1, false), btn("‹ Prev", page - 1, false), ...nums, btn("Next ›", page + 1, false), btn("Last »", pages, false)),
      h("div", { class: "pg-info" }, `Page ${fmt(page)} of ${fmt(pages)} · ${fmt(total)} in all`, h("span", { class: "pg-go" }, box, h("button", { class: "pg", onclick: jump }, "Go"))));
  }
  const sect = (title, more, body) => h("section", { class: "x-sect" }, h("header", {}, h("h2", {}, title), more ? link("View all →", more) : ""), body);
  const kv = (rows) => h("div", { class: "x-kv" }, rows.filter(Boolean).flatMap(([k, v]) => [h("div", { class: "k" }, k), h("div", {}, v)]));
  const crumbs = (...parts) => h("div", { class: "x-crumbs" }, parts.flatMap((p, i) => (i ? [" / ", p] : [p])));
  const head = (title, ...pills) => h("div", { class: "x-dhead" }, h("h1", {}, title), ...pills);
  const pill = (t, ok) => h("span", { class: "pill" + (ok ? " ok" : "") }, t);

  // ---------- views ----------
  async function vHome() {
    sum = await xapi("explorer.summary", { chain: chain.key });
    const tok = sum.protocol ? sum.protocol.toUpperCase() : "Token";
    const st = [
      ["Height", fmt(sum.tip), sum.tip_time ? `last block ${ago(sum.tip_time)}` : ""],
      ["Mempool", `${fmt(sum.mempool.count)} tx`, `${fmt(sum.mempool.bytes)} bytes`],
      has("ord") && ["Inscriptions", fmt(sum.inscriptions), "ord index"],
      has("tokens") && [`${tok} tokens`, fmt(sum.ticks), "deployed ticks"],
      has("names") && ["Names", fmt(sum.names?.[0] ?? 0), sum.names ? `json ${fmt(sum.names[1])} · doge2023 ${fmt(sum.names[2])}` : ""],
      has("maps") && ["Maps", fmt(sum.maps), "first claim wins"],
    ].filter(Boolean);
    const lower = [
      has("tokens") && sect(`${tok} tokens`, "tokens", tokTable(sum.tokens || [])),
      has("names") && sect("Names", "names", (sum.name_rows || []).length ? table([["Name"], ["Inscription", 1]], sum.name_rows.map((r) => ({ go: () => go("name", r.name), cells: [[h("span", { class: "mono" }, r.name)], [`#${fmt(r.number)}`, 1]] }))) : h("div", { class: "x-empty" }, "No names claimed yet.")),
      has("maps") && sect("Maps", "maps", (sum.map_rows || []).length ? table([["Claim"], ["Inscription", 1]], sum.map_rows.map((r) => ({ go: () => go("map", r.claim), cells: [[h("span", { class: "mono" }, r.claim)], [`#${fmt(r.number)}`, 1]] }))) : h("div", { class: "x-empty" }, "No block maps claimed yet.")),
    ].filter(Boolean);
    return h("div", {},
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
    const body = m.txs.length ? table([["Txid"], ["Out", 1], ["Size", 1], ["Kind"]], m.txs.map((t) => ({ go: () => go("tx", t.txid),
      cells: [[h("span", { class: "mono" }, short(t.txid, 16, 8))], [coins(t.out), 1], [`${fmt(t.size)} B`, 1], [h("span", { class: "chip" + (t.kind === "inscription" ? " ins" : "") }, t.kind + (t.rbf ? " · RBF" : ""))]] })))
      : h("div", { class: "x-empty" }, "No unconfirmed transactions right now.");
    return h("div", {}, sect("Mempool", null, body),
      m.pending_tags.length ? h("p", { class: "x-note" }, `Tag claims waiting for a block: ${m.pending_tags.map((t) => "@" + (t.tag || t)).join(", ")}`) : "",
      h("p", { class: "x-note" }, "Read from this arcade's mempool. Spending with zero confirmations is fine on chains without RBF."));
  }
  async function vInscriptions() {
    const size = 36, page = pageOf();
    const first = await xapi("explorer.inscriptions", { chain: chain.key, n: 1 });
    const total = first.total;
    const r = await xapi("explorer.inscriptions", { chain: chain.key, before: Math.max(0, total - (page - 1) * size), n: size });
    const toPage = (num) => Math.floor((total - 1 - num) / size) + 1;
    return sect(`Inscriptions · ${fmt(total)}`, null, h("div", {}, pager(total, size, page, "inscriptions", "inscription #", toPage), insGrid(r.items), pager(total, size, page, "inscriptions", "inscription #", toPage)));
  }
  async function vTokens() {
    const size = 50, page = pageOf();
    const r = await xapi("explorer.tokens", { chain: chain.key, offset: (page - 1) * size, n: size });
    return sect(`${r.protocol.toUpperCase()} tokens · ${fmt(r.total)}`, null, h("div", {}, pager(r.total, size, page, "tokens"), tokTable(r.items), pager(r.total, size, page, "tokens")));
  }
  async function vNames() {
    const r = await xapi("explorer.names", { chain: chain.key, after: arg || undefined, n: 50 });
    const last = r.items[r.items.length - 1];
    return sect(`Names · ${fmt(r.counts[0])}`, null, h("div", {}, r.items.length ? table([["Name"], ["Winning inscription", 1]], r.items.map((x) => ({ go: () => go("name", x.name), cells: [[h("span", { class: "mono" }, x.name)], [`#${fmt(x.number)}`, 1]] })))
      : h("div", { class: "x-empty" }, "No names claimed yet."), r.items.length === 50 && last ? h("div", { class: "row" }, h("button", { onclick: () => go("names", last.name) }, "More")) : ""));
  }
  async function vMaps() {
    const r = await xapi("explorer.maps", { chain: chain.key, n: 50 });
    return sect("Maps", null, h("div", {}, r.items.length ? table([["Claim"], ["Winning inscription", 1]], r.items.map((x) => ({ go: () => go("map", x.claim), cells: [[h("span", { class: "mono" }, x.claim)], [`#${fmt(x.number)}`, 1]] })))
      : h("div", { class: "x-empty" }, "No block maps claimed yet."),
      h("p", { class: "x-note" }, "A map claim is an inscription whose whole text is <block>.<name>map (pepemap, dogemap, bitmap, …). The block can't be above the one it was inscribed in, and the first claim wins.")));
  }
  // Arcade ledger: the arcade's own tokens (Omni-style properties), inscriptions and collections.
  function ledgerGrid(list) {
    if (!list.length) return h("div", { class: "x-empty" }, "No arcade inscriptions yet.");
    return h("div", { class: "x-grid" }, list.map((i) => h("div", { class: "x-ins", onclick: () => go("linsc", i.txid) },
      preview({ content: i.content, ct: i.content_type, n: i.number }), h("div", { class: "cap" }, h("b", {}, `#${fmt(i.number)}`), h("span", { class: "faint" }, fmt(i.block_height))))));
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
      head(`Arcade inscription #${fmt(i.number)}`, h("span", { class: "chip arc" }, (i.content_type || "").split(";")[0] || "no type")),
      h("div", { class: "x-split" }, preview({ content: i.content, ct: i.content_type, n: i.number }, true),
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
        ["Bits · nonce · version", h("span", { class: "mono" }, `${b.bits} · ${b.nonce} · ${b.version}`)], ["Merkle root", h("span", { class: "mono" }, b.merkle_root)]]),
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
      a.history ? h("h2", { class: "x-h2" }, "History") : "",
      !a.history ? "" : a.history.length ? table([["Block"], ["Txid"], ["Received", 1], ["Sent", 1]], a.history.map((r) => ({ go: r.txid ? () => go("tx", r.txid) : null,
        cells: [[fmt(r.height)], [h("span", { class: "mono" }, short(r.txid || "", 16, 8))], [r.received ? coins(r.received) : "", 1], [r.sent ? coins(r.sent) : "", 1]] })))
        : h("div", { class: "x-empty" }, "No transactions."));
  }
  async function vInscription() {
    const i = await xapi("explorer.inscription", { chain: chain.key, id: arg });
    const ct = (i.ct || "").split(";")[0];
    return h("div", {}, crumbs(link("Inscriptions", "inscriptions"), `#${fmt(i.n)}`),
      head(`Inscription #${fmt(i.n)}`, h("span", { class: "chip ins" }, ct || "no type"), i.burned != null ? pill("burned", false) : ""),
      h("div", { class: "x-split" }, preview(i, true),
        kv([["Id", h("span", { class: "mono" }, i.id)], ["Holder", i.owner ? addrLink(i.owner, i.owner_tag) : h("span", { class: "faint" }, i.burned != null ? `burned at ${fmt(i.burned)}` : "—")],
          i.at && ["Location", link(short(i.at, 12, 10), "tx", i.at.split(":")[0], "mono")], ["Inscribed", link(`block ${fmt(i.h)}`, "block", String(i.h))],
          ["Content type", i.ct || "—"], ["Size", `${fmt(i.size)} bytes · ${i.pieces} piece${i.pieces === 1 ? "" : "s"}`], i.proto && ["Protocol", i.proto],
          i.delegate && ["Delegate", link(short(i.delegate, 12, 6), "inscription", i.delegate, "mono")], i.parent && ["Parent", link(short(i.parent, 12, 6), "inscription", i.parent, "mono")],
          i.re && ["Reinscription", "yes"], ["Reveal", link(short(i.txs?.[i.txs.length - 1] || "", 12, 8), "tx", i.txs?.[i.txs.length - 1], "mono")]])));
  }
  async function vToken() {
    const t = await xapi("explorer.token", { chain: chain.key, tick: arg });
    const pct = Number(BigInt(t.minted || "0") * 1000n / (BigInt(t.supply || "1") || 1n)) / 10;
    return h("div", {}, crumbs(link("Tokens", "tokens"), t.shown || t.tick),
      head(t.shown || t.tick, pct >= 100 ? pill("minted out") : pill(`${pct}% minted`, true)),
      kv([["Supply", units(t.supply, t.dec)], ["Minted", units(t.minted, t.dec)], ["Mint limit", units(t.lim, t.dec)], ["Decimals", String(t.dec)],
        ["Deploy", link(`inscription #${fmt(t.n)}`, "inscription", t.id)], ["Deployer", addrLink(t.deployer)], ["Holders", t.holders]]),
      h("h2", { class: "x-h2" }, "Top holders"),
      t.top.length ? table([["Holder"], ["Balance", 1], ["Transferable", 1]], t.top.map((x) => ({ go: () => go("address", x.address), cells: [[addrLink(x.address, x.tag)], [units(x.overall, t.dec), 1], [units(x.transferable, t.dec), 1]] })))
        : h("div", { class: "x-empty" }, "Nobody holds any yet."));
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
  const VIEWS = { home: vHome, blocks: vBlocks, mempool: vMempool, inscriptions: vInscriptions, tokens: vTokens, names: vNames, maps: vMaps, ledger: vLedger,
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
      // A page whose index is off on this chain isn't available: back to the overview.
      if (NEEDS[page] && !has(NEEDS[page])) { page = "home"; arg = null; }
      if (!VIEWS[page]) { page = "home"; arg = null; }
      meshFrom = null;
      const v = await VIEWS[page]();
      if (my !== seq) return;
      const from = meshFrom && page !== "home"
        ? h("div", { class: "x-mesh" }, `Answered by ${meshFrom.tag ? "@" + meshFrom.tag + "'s arcade" : "arcade " + meshFrom.node.slice(0, 8)} on the mesh: this arcade doesn't keep that ${chain.name} index, so it's their word, not checked here.`)
        : null;
      $("x-view").replaceChildren(...[from, v].filter(Boolean));
      paintNav();
    } catch (e) {
      if (my !== seq) return;
      $("x-view").replaceChildren(h("div", { class: "x-empty bad" }, e.message), h("div", { class: "row" }, h("button", { onclick: () => go("home") }, "Back to the overview")));
    }
  }
  function go(p, a = null) { page = p; arg = a; render(); window.scrollTo({ top: 0 }); }

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
