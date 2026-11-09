// Feed tab: public posts on the home chain (the original arcade's feed), with likes, replies,
// shares, tips, edits and deletes.
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
  const ago = (hours) => hours < 1 ? `${Math.max(1, Math.round(hours * 60))}m` : hours < 48 ? `${Math.round(hours)}h` : `${Math.round(hours / 24)}d`;
  let order = "popular", channel = "", tag = "", offset = 0, page = 30, attached = null;

  function note(t, bad) { const m = $("fd-msg"); m.textContent = t; m.className = "msg " + (bad ? "bad" : "ok"); }

  async function act(kind, target, extra = {}) {
    try { const r = await api("feed.act", { kind, target, ...extra }); note(`${kind} sent (fee ${PEP(r.fee)}); it counts once it's in a block.`); setTimeout(draw, 1500); }
    catch (e) { note(e.message, true); }
  }

  function card(p, depth = 0) {
    const a = p.author || {};
    const who = h("a", { class: "fd-who", href: "#" }, a.tag ? "@" + a.tag : (a.address || "").slice(0, 10) + "…");
    who.onclick = (e) => { e.preventDefault(); if (a.tag) { tag = a.tag; offset = 0; draw(); } };
    const text = p.muted ? h("div", { class: "s" }, "Muted. ", (() => { const b = h("button", { class: "linkish" }, "Unmute"); b.onclick = async () => { await api("mute.remove", { address: a.address }); draw(); }; return b; })())
      : h("div", { class: "fd-text" }, p.text || "");
    const file = p.file ? h("div", { class: "fd-file" }, "📎 ", p.file.name) : null;
    if (p.file && (p.file.type || "").startsWith("image/") && p.height) {
      api("feed.file", { txid: p.txid }).then((d) => file.prepend(h("img", { src: `data:${d.type};base64,${d.data}`, alt: p.file.name }), h("br"))).catch(() => {});
    }
    const btn = (label, fn, on) => { const b = h("button", { class: "fd-btn" + (on ? " on" : "") }, label); b.onclick = fn; return b; };
    const replyBox = h("div", { class: "fd-reply", hidden: true });
    const tipBox = h("div", { class: "fd-reply", hidden: true });
    const acts = h("div", { class: "fd-acts" },
      btn(`▲ ${p.likes}`, () => act(p.my_opinion === "like" ? "unlike" : "like", p.txid), p.my_opinion === "like"),
      btn(`▼ ${p.dislikes}`, () => act(p.my_opinion === "dislike" ? "undislike" : "dislike", p.txid), p.my_opinion === "dislike"),
      btn(`Reply${p.replies.length ? " · " + p.replies.length : ""}`, () => { replyBox.hidden = !replyBox.hidden; }),
      depth === 0 ? btn(`Share${p.shares ? " · " + p.shares : ""}`, () => act("share", p.txid)) : "",
      p.mine ? "" : btn(`Tip${p.tips ? " · " + PEP(p.tips) : ""}`, () => { tipBox.hidden = !tipBox.hidden; }),
      p.mine ? btn("Edit", () => {
        const t = h("textarea", { rows: 2 }); t.value = p.text || "";
        const s = h("button", { class: "small primary" }, "Save edit");
        s.onclick = () => act("edit", p.txid, { text: t.value });
        replyBox.replaceChildren(t, s); replyBox.hidden = false;
      }) : "",
      p.mine ? btn("Delete", (e) => { const b = e.target; if (!b.dataset.sure) { b.dataset.sure = 1; b.textContent = "Delete: sure?"; return; } act("delete", p.txid); }) : "",
      !p.mine && a.address ? btn("Mute", async () => { await api("mute.add", { address: a.address }); draw(); }) : "");
    const rt = h("textarea", { rows: 2, placeholder: "Write a reply…" });
    const rs = h("button", { class: "small primary" }, "Reply");
    rs.onclick = () => rt.value.trim() && act("reply", p.txid, { text: rt.value.trim() });
    replyBox.append(rt, rs);
    const amt = h("input", { type: "number", min: "0.01", step: "0.01", placeholder: homeTicker(), style: "width:9em" });
    const ts = h("button", { class: "small primary" }, "Send tip");
    ts.onclick = () => { const v = Math.round(Number(amt.value) * 1e8); if (v > 0) act("tip", p.txid, { amount: v }); };
    tipBox.append(amt, " to ", a.tag ? "@" + a.tag : a.address, " ", ts);
    return h("div", { class: "fd-card" + (depth ? " fd-sub" : "") },
      h("div", { class: "fd-head" }, who, a.name && a.name !== a.tag ? h("span", { class: "s" }, " " + a.name) : "",
        h("span", { class: "s" }, ` · ${p.height ? ago(p.age_hours || 0) : "waiting for a block"}${p.edited ? " · edited" : ""}${p.channel && p.channel !== "main" ? " · #" + p.channel : ""}`)),
      text, file, acts, replyBox, tipBox,
      ...(p.replies || []).map((r) => card(r, depth + 1)));
  }

  async function draw() {
    document.querySelectorAll("#fd-orders button").forEach((b) => b.classList.toggle("on", b.dataset.o === order));
    $("fd-tagbar").hidden = !tag;
    $("fd-tagname").textContent = tag ? "@" + tag : "";
    try {
      const [f, chs, st] = await Promise.all([
        api("feed", { order, channel, tag: tag || undefined, limit: page, offset }),
        api("feed.channels"), api("feed.settings"),
      ]);
      const sel = $("fd-channel");
      const cur = sel.value;
      sel.replaceChildren(h("option", { value: "" }, "All channels"), ...chs.map((c) => h("option", { value: c.channel }, `#${c.channel} (${c.posts})`)));
      sel.value = cur;
      $("fd-hidemuted").checked = !!st.hide_muted; $("fd-contacts").checked = !!st.contacts_only;
      $("fd-list").replaceChildren(...(f.posts.length ? f.posts.map((p) => card(p)) : [h("div", { class: "mx-empty" }, order === "friends" ? "Nothing from your contacts yet." : "No posts.")]));
      $("fd-page").textContent = f.total ? `${offset + 1}–${Math.min(offset + page, f.total)} of ${f.total}` : "";
      $("fd-prev").disabled = offset === 0; $("fd-next").disabled = offset + page >= f.total;
    } catch (e) { note(e.message, true); }
  }

  document.querySelectorAll("#fd-orders button").forEach((b) => b.onclick = () => { order = b.dataset.o; offset = 0; draw(); });
  $("fd-channel").onchange = (e) => { channel = e.target.value; offset = 0; draw(); };
  $("fd-prev").onclick = () => { offset = Math.max(0, offset - page); draw(); };
  $("fd-next").onclick = () => { offset += page; draw(); };
  $("fd-alltags").onclick = () => { tag = ""; offset = 0; draw(); };
  const saveSettings = () => api("feed.settings", { set: { hide_muted: $("fd-hidemuted").checked, contacts_only: $("fd-contacts").checked } }).then(draw);
  $("fd-hidemuted").onchange = saveSettings; $("fd-contacts").onchange = saveSettings;
  $("fd-file").addEventListener("change", (e) => {
    const f = e.target.files[0];
    if (!f) { attached = null; return; }
    const r = new FileReader();
    r.onload = () => { attached = { name: f.name, type: f.type || "application/octet-stream", data: String(r.result).split(",")[1] }; };
    r.readAsDataURL(f);
  });
  $("fd-post").onclick = async () => {
    const text = $("fd-text").value.trim();
    if (!text && !attached) return;
    try {
      const r = await api("feed.post", { text, channel: $("fd-postch").value.trim(), file: attached || undefined });
      $("fd-text").value = ""; $("fd-file").value = ""; attached = null;
      note(`Posted (${r.txids.length} transaction${r.txids.length === 1 ? "" : "s"}, fee ${PEP(r.fee)}).`); order = "new"; offset = 0; draw();
    } catch (e) { note(e.message, true); }
  };
  document.querySelector('nav button[data-tab="feed"]').addEventListener("click", () => draw());
  setInterval(() => { if (document.querySelector(".tab.on")?.id === "feed") draw(); }, 30000);
})();
