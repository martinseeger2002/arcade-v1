// Apps tab: the apps installed on this arcade, and the App Store of published app cards
// (the original arcade's game cards). Each app opens in its own window, sandboxed.
(() => {
  const $ = (id) => document.getElementById(id);
  const invoke = window.__TAURI__.core.invoke;
  async function api(cmd, args = {}) {
    const r = await invoke("api", { req: { cmd, ...args } });
    if (!r.ok) throw new Error(r.error || "failed");
    return r.result;
  }
  const h = (tag, attrs = {}, ...kids) => {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === undefined || v === null || v === false) continue;
      if (k === "class") e.className = v; else if (k.startsWith("on")) e[k] = v; else e.setAttribute(k, v === true ? "" : v);
    }
    for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false) e.append(k.nodeType ? k : document.createTextNode(String(k)));
    return e;
  };
  const PEP = homeCoins;
  const ago = (t) => { if (!t) return "waiting"; const s = Date.now() / 1000 - t; return s < 3600 ? `${Math.max(1, Math.round(s / 60))}m ago` : s < 172800 ? `${Math.round(s / 3600)}h ago` : `${Math.round(s / 86400)}d ago`; };
  const PERMS = [["wallet", "Wallet access"], ["contacts", "Contacts"], ["groups", "Group chats"], ["media", "Camera & microphone"], ["pictures", "Pictures in the app"], ["notify", "Notifications"]];
  let view = "mine", sort = "popular", query = "", base = "http://127.0.0.1:8420", detail = null;

  function note(t, bad) { const m = $("ap2-msg"); m.textContent = t; m.className = "msg " + (bad ? "bad" : "ok"); }

  async function open(a) {
    try {
      const l = await api("apps.launch", { chain: a.chain, txid: a.txid });
      await invoke("open_app", { id: l.id, title: l.title, frame: l.frame });
    } catch (e) { note(e.message, true); }
  }

  function cover(a) {
    return a.card.cover
      ? h("img", { class: "ap-cover", src: `${base}/app/${a.chain}/${a.card.cover}`, alt: "" })
      : h("div", { class: "ap-cover ap-tile" }, a.card.name.slice(0, 2));
  }

  function byline(a) {
    const s = a.social || {};
    return [a.creator_tag ? "@" + a.creator_tag : a.creator.slice(0, 10) + "…", ago(a.time), a.card.version ? "v" + a.card.version : null,
      a.card.players ? a.card.players + " players" : null, s.tips ? `tipped ${s.tips} time${s.tips === 1 ? "" : "s"}` : null].filter(Boolean).join(" · ");
  }

  function pills(a) {
    return h("div", { class: "ap-pills" },
      a.card.multiplayer ? h("span", { class: "pill" }, "multiplayer") : "",
      a.card.genre ? h("span", { class: "pill" }, a.card.genre) : "",
      h("span", { class: "pill" }, a.chain_name));
  }

  async function act(kind, a, extra = {}) {
    try {
      if (a.chain !== HOME.key) throw new Error(`likes, comments and tips work on ${HOME.name} apps for now`);
      const r = await api("feed.act", { kind, target: a.txid, ...extra });
      note(`${kind} sent (fee ${PEP(r.fee)}); it counts once it's in a block.`);
    } catch (e) { note(e.message, true); }
  }

  function storeCard(a) {
    const s = a.social || {};
    const star = h("button", { class: "ap-star" + (a.starred ? " on" : ""), title: "Star" }, a.starred ? "★" : "☆");
    star.onclick = async () => { await api("apps.star", { chain: a.chain, txid: a.txid, on: !a.starred }); draw(); };
    const inst = h("button", { class: a.installed ? "" : "primary" }, a.installed ? "Installed" : "Install");
    inst.disabled = a.installed;
    inst.onclick = async () => { await api("apps.install", { chain: a.chain, txid: a.txid }); note(`${a.card.name} is on Your apps.`); draw(); };
    const openB = h("button", {}, "Open"); openB.onclick = () => open(a);
    const details = h("button", { class: "small" }, "Details"); details.onclick = () => { detail = a; draw(); };
    const comment = h("div", { class: "fd-reply", hidden: true }, (() => { const t = h("input", { placeholder: "Comment…" }); const b = h("button", { class: "small primary" }, "Send"); b.onclick = () => t.value.trim() && act("reply", a, { text: t.value.trim() }); return [t, b]; })());
    const tip = h("div", { class: "fd-reply", hidden: true }, (() => { const t = h("input", { type: "number", min: "0.01", step: "0.01", placeholder: homeTicker(), style: "width:8em" }); const b = h("button", { class: "small primary" }, "Tip"); b.onclick = () => { const v = Math.round(Number(t.value) * 1e8); if (v > 0) act("tip", a, { amount: v }); }; return [t, b]; })());
    const btn = (l, f, on) => { const b = h("button", { class: "fd-btn" + (on ? " on" : "") }, l); b.onclick = f; return b; };
    return h("div", { class: "ap-card" }, cover(a),
      h("div", { class: "ap-body" },
        h("div", { class: "ap-title" }, h("b", {}, a.card.name), star),
        pills(a), h("div", { class: "s" }, byline(a)),
        a.card.description ? h("div", { class: "ap-desc" }, a.card.description) : "",
        h("div", { class: "row ap-row" }, openB, inst, details),
        h("div", { class: "fd-acts" },
          btn(`▲ ${s.likes || 0}`, () => act(s.my_opinion === "like" ? "unlike" : "like", a), s.my_opinion === "like"),
          btn(`▼ ${s.dislikes || 0}`, () => act(s.my_opinion === "dislike" ? "undislike" : "dislike", a), s.my_opinion === "dislike"),
          btn(`Comment${s.comments ? " · " + s.comments : ""}`, () => { comment.hidden = !comment.hidden; }),
          a.mine ? "" : btn("Tip", () => { tip.hidden = !tip.hidden; })),
        comment, tip,
        ...(s.last_comments || []).map((c) => h("div", { class: "ap-comment s" }, (c.from || "").slice(0, 8) + "…: " + c.text))));
  }

  function mineCard(a) {
    if (a.missing) {
      const un = h("button", { class: "small" }, "Uninstall"); un.onclick = async () => { await api("apps.uninstall", { id: a.id }); draw(); };
      return h("div", { class: "ap-mine" }, h("div", { class: "ap-cover ap-tile" }, "?"), h("b", {}, a.card?.name || a.id), h("div", { class: "s" }, "Not found on the chains here now"), un);
    }
    const tile = h("div", { class: "ap-mine" + (a.removed ? " gone" : "") }, cover(a), h("b", {}, a.card.name), h("div", { class: "s" }, a.removed ? "removed by its publisher" : (a.card.version ? "v" + a.card.version : "")));
    tile.onclick = (e) => { if (e.target.tagName !== "BUTTON" && !a.removed) open(a); };
    const un = h("button", { class: "small" }, "Uninstall");
    un.onclick = async () => { if (!un.dataset.sure) { un.dataset.sure = 1; un.textContent = "Sure?"; return; } await api("apps.uninstall", { id: a.id }); draw(); };
    const det = h("button", { class: "small" }, "Details"); det.onclick = () => { detail = a; draw(); };
    tile.append(h("div", { class: "row ap-row" }, det, un));
    return tile;
  }

  async function drawDetail() {
    const a = await api("apps.card", { chain: detail.chain, txid: detail.txid });
    const back = h("button", { class: "small" }, "← Back"); back.onclick = () => { detail = null; draw(); };
    const claimed = PERMS.filter(([k]) => a.card[k]);
    const sw = claimed.map(([k, label]) => {
      const box = h("input", { type: "checkbox", checked: !!a.permissions[k] });
      box.onchange = async () => { await api("apps.perms", { id: a.id, set: { [k]: box.checked } }); note(`${label} ${box.checked ? "on" : "off"} for ${a.card.name}.`); };
      return h("label", { class: "ap-perm" }, box, " ", label);
    });
    const openB = h("button", { class: "primary" }, "Open"); openB.onclick = () => open(a);
    $("ap2-body").replaceChildren(h("div", { class: "ap-detail" }, back,
      h("div", { class: "ap-card" }, cover(a), h("div", { class: "ap-body" }, h("h2", {}, a.card.name), pills(a), h("div", { class: "s" }, byline(a)),
        h("div", { class: "ap-desc" }, a.card.description || ""), h("div", { class: "row ap-row" }, openB))),
      claimed.length ? h("div", { class: "card" }, h("div", { class: "k" }, "This app asks for"), ...sw, h("div", { class: "s" }, "Off: it asks you the first time it wants it.")) : "",
      h("h2", {}, "Versions"),
      h("ul", { class: "ap-list" }, ...a.versions.map((v, i) => h("li", {}, `${v.version ? "v" + v.version : "version"} · block ${v.height}${i === 0 ? " · shown" : ""}`))),
      h("h2", {}, `Comments (${a.comments.length})`),
      h("ul", { class: "ap-list" }, ...(a.comments.length ? a.comments.map((c) => h("li", {}, h("b", {}, (c.from || "").slice(0, 10) + "… "), c.text)) : [h("li", { class: "s" }, "None yet.")])),
      h("div", { class: "s" }, `Card ${a.txid} · page ${a.opens} on ${a.chain_name} · by ${a.creator}`)));
  }

  let drawSeq = 0;
  async function draw() {
    const mySeq = ++drawSeq;
    const body = $("ap2-body");
    // A slower, older draw must not replace a newer one.
    const put = (...kids) => { if (mySeq === drawSeq) body.replaceChildren(...kids); };
    document.querySelectorAll("#ap2-views button").forEach((b) => b.classList.toggle("on", b.dataset.v === view));
    $("ap2-storebar").hidden = view !== "store";
    try {
      base = (await api("rpc.info")).url.replace(/\/rpc$/, "");
      if (detail) return drawDetail();
      if (view === "mine") {
        const list = await api("apps.installed");
        put(list.length ? h("div", { class: "ap-grid" }, ...list.map(mineCard)) : h("div", { class: "mx-empty" }, "No apps installed yet. Find some in the App Store."));
      } else if (view === "store") {
        const r = await api("apps.store", { sort, q: query });
        put(r.apps.length ? h("div", { class: "ap-store" }, ...r.apps.map(storeCard)) : h("div", { class: "mx-empty" }, "No apps found."));
      } else if (view === "publish") {
        drawPublish(put);
      } else {
        const r = await api("apps.store", { flagged: true });
        put(h("p", { class: "note" }, "These cards look like an earlier app by someone else, so the App Store hides them until you decide. Trusted publishers are never held back."),
          ...(r.apps.length ? r.apps.map((a) => {
            const ok = h("button", { class: "small primary" }, "Show it"); ok.onclick = async () => { await api("apps.review", { chain: a.chain, txid: a.txid, verdict: "clear" }); draw(); };
            const no = h("button", { class: "small" }, "Keep hidden"); no.onclick = async () => { await api("apps.review", { chain: a.chain, txid: a.txid, verdict: "confirm" }); draw(); };
            const tr = h("button", { class: "small" }, "Trust this publisher"); tr.onclick = async () => { await api("apps.trust", { address: a.creator, on: true }); draw(); };
            return h("div", { class: "ap-card" }, cover(a), h("div", { class: "ap-body" }, h("b", {}, a.card.name), h("div", { class: "s" }, byline(a)), h("div", { class: "s bad" }, a.flag), h("div", { class: "row ap-row" }, ok, no, tr)));
          }) : [h("div", { class: "mx-empty" }, "Nothing to review.")]));
      }
    } catch (e) { note(e.message, true); }
  }

  function drawPublish(put) {
    const f = (ph, attrs = {}) => h("input", { placeholder: ph, ...attrs });
    const name = f("Name (required, up to 60)"), version = f("Version, e.g. 1.0"), players = f("Players, e.g. 1-4"), genre = f("Genre, e.g. Puzzle"), family = f("Family (rooms shared across versions; letters, digits . _ : -)");
    const desc = h("textarea", { rows: 3, placeholder: "Description (up to 280)" });
    const FLAGS = [["multiplayer", "Multiplayer"], ["wallet", "Uses the wallet"], ["contacts", "Reads contacts"], ["groups", "Group chats"], ["media", "Camera & microphone"], ["pictures", "Draws pictures"], ["notify", "Rings while closed"]];
    const boxes = FLAGS.map(([k, l]) => h("label", { class: "ap-perm" }, h("input", { type: "checkbox", value: k }), " ", l));
    const page = h("input", { type: "file", accept: ".html,.htm,text/html" }), cov = h("input", { type: "file", accept: "image/*" });
    const est = h("div", { class: "s" });
    const read = (input) => new Promise((ok) => { const file = input.files[0]; if (!file) return ok(null); const r = new FileReader(); r.onload = () => ok({ name: file.name, type: file.type, data: String(r.result).split(",")[1], size: file.size }); r.readAsDataURL(file); });
    const cardOf = () => {
      const c = { name: name.value.trim() };
      for (const [k, el] of [["version", version], ["players", players], ["genre", genre], ["family", family], ["description", desc]]) if (el.value.trim()) c[k] = el.value.trim();
      for (const b of boxes) { const i = b.querySelector("input"); if (i.checked) c[i.value] = true; }
      return c;
    };
    const estimate = async () => {
      const pg = await read(page), cv = await read(cov);
      if (!pg) { est.textContent = ""; return; }
      const a = await api("inscribe.estimate", { data: pg.data, encoding: "base64", contenttype: "text/html", json: JSON.stringify({ app: cardOf() }) });
      const b = cv ? await api("inscribe.estimate", { data: cv.data, encoding: "base64", contenttype: cv.type || "image/png" }) : { fee: 0, locked: 0, transactions: 0 };
      est.textContent = `${a.transactions + b.transactions} transaction${a.transactions + b.transactions === 1 ? "" : "s"} · network fee about ${PEP(a.fee + b.fee)} · ${PEP(a.locked + b.locked)} kept in data outputs (yours, spendable later)`;
    };
    page.onchange = estimate; cov.onchange = estimate;
    const go = h("button", { class: "primary" }, "Publish");
    go.onclick = async () => {
      const c = cardOf();
      if (!c.name) return note("Give the app a name.", true);
      const pg = await read(page);
      if (!pg) return note("Pick the app's HTML file.", true);
      go.disabled = true;
      try {
        const cv = await read(cov);
        if (cv) {
          const r = await api("inscribe", { data: cv.data, encoding: "base64", contenttype: cv.type || "image/png" });
          c.cover = r.txid;
        }
        const r = await api("inscribe", { data: pg.data, encoding: "base64", contenttype: "text/html", json: JSON.stringify({ app: c }) });
        note(`Published ${c.name}: card ${r.txid.slice(0, 12)}… It shows in the App Store once its transactions are in blocks.`);
      } catch (e) { note(e.message, true); }
      go.disabled = false;
    };
    put(h("div", { class: "card ap-pub" },
      h("p", { class: "s" }, "An app is an HTML page inscribed with its card. Publishing again under the same name replaces your earlier version; the old one still plays at its own link."),
      name, h("div", { class: "row" }, version, players, genre), family, desc, h("div", { class: "ap-flags" }, ...boxes),
      h("label", {}, "App page (HTML) ", page), h("label", {}, "Cover image ", cov), est, h("div", { class: "row" }, go)));
  }

  document.querySelectorAll("#ap2-views button").forEach((b) => b.onclick = () => { view = b.dataset.v; detail = null; draw(); });
  document.querySelectorAll("#ap2-sort button").forEach((b) => b.onclick = () => { sort = b.dataset.s; document.querySelectorAll("#ap2-sort button").forEach((x) => x.classList.toggle("on", x === b)); draw(); });
  $("ap2-q").addEventListener("input", (e) => { query = e.target.value; clearTimeout(window.__apq); window.__apq = setTimeout(draw, 300); });
  document.querySelector('nav button[data-tab="apps"]').addEventListener("click", () => { detail = null; draw(); });
})();
