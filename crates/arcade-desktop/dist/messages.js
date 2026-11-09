// Messages tab: private messages on the home chain (compatible with the original arcade).
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
  const when = (t) => {
    if (!t) return "";
    const d = new Date(t * 1000), now = new Date();
    return d.toDateString() === now.toDateString() ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : d.toLocaleDateString([], { month: "short", day: "numeric" });
  };
  const nameOf = (w) => (w.name ? w.name : w.tag ? "@" + w.tag : w.address ? w.address.slice(0, 10) + "…" : w.fingerprint || "someone");
  const sub = (w) => [w.tag ? "@" + w.tag : null, w.fingerprint].filter(Boolean).join(" · ");
  const PEP = homeCoins;

  let view = "chats", open = null, composing = false, lastUnread = null, notified = new Set();

  function note(text, bad) { const m = $("mx-msg"); m.textContent = text; m.className = "msg " + (bad ? "bad" : "ok"); }

  async function refreshMe() {
    try {
      const me = await api("msg.me");
      $("mx-me").replaceChildren(
        h("div", { class: "k" }, "Your messaging key"),
        h("div", { class: "mono" }, me.fingerprint),
        h("div", { class: "s" }, me.announced ? "published: people can message you" : "not published yet: it goes out with your @tag"));
    } catch (e) {
      $("mx-me").replaceChildren(h("div", { class: "s" }, e.message.includes("wallet") ? "Create or restore a wallet first (Wallet tab)." : e.message));
    }
  }

  async function refreshList() {
    const list = $("mx-list");
    if (view === "groups") {
      const gs = await api("group.list");
      if (!gs.length) { list.replaceChildren(h("li", { class: "mx-empty" }, "No groups yet. Make one with New group, or ask someone to add you.")); return; }
      list.replaceChildren(...gs.map((g) => {
        const li = h("li", { class: "mx-item" + (open === "g:" + g.id ? " on" : "") },
          h("div", { class: "mx-row" }, h("b", {}, g.name), h("time", {}, g.last ? when(g.last.at) : "")),
          h("div", { class: "s" }, g.last ? (g.last.dir === "out" ? "You: " : "") + g.last.text : `${g.members.length} member${g.members.length === 1 ? "" : "s"}`),
          g.unread ? h("span", { class: "badge" }, String(g.unread)) : "");
        li.onclick = () => { open = "g:" + g.id; composing = false; draw(); };
        return li;
      }));
      return;
    }
    const items = view === "contacts" ? await api("contacts.list") : await api("msg.conversations", { requests: view === "requests" });
    if (!items.length) {
      list.replaceChildren(h("li", { class: "mx-empty" }, view === "contacts" ? "No contacts yet. Add someone from a conversation, or with New message." : view === "requests" ? "No message requests." : "No conversations yet."));
      return;
    }
    list.replaceChildren(...items.map((c) => {
      const w = view === "contacts" ? c : c.who;
      const peer = view === "contacts" ? c.key : c.peer;
      const li = h("li", { class: "mx-item" + (open === peer ? " on" : "") },
        h("div", { class: "mx-row" }, h("b", {}, nameOf(w)), view === "contacts" ? "" : h("time", {}, when(c.last.at))),
        h("div", { class: "s" }, view === "contacts" ? sub(w) : (c.last.dir === "out" ? "You: " : "") + (c.last.text || (c.last.file ? "📎 " + c.last.file.name : ""))),
        c.unread ? h("span", { class: "badge" }, String(c.unread)) : "");
      li.onclick = () => { open = peer; composing = false; draw(); };
      return li;
    }));
  }

  async function fileView(m) {
    const f = m.file;
    if (!f) return null;
    const box = h("div", { class: "mx-file" }, "📎 ", f.name, h("span", { class: "s" }, ` · ${(f.size / 1024).toFixed(1)} KB`));
    if ((f.type || "").startsWith("image/") && f.size < 4 << 20) {
      try {
        const d = await api("msg.file", { id: m.id });
        box.prepend(h("img", { src: `data:${d.type};base64,${d.data}`, alt: f.name }), h("br"));
      } catch (_) { /* shown as a name */ }
    } else {
      const b = h("button", { class: "small" }, "Save");
      b.onclick = async () => {
        const d = await api("msg.file", { id: m.id });
        const a = h("a", { href: `data:${d.type};base64,${d.data}`, download: d.name });
        document.body.append(a); a.click(); a.remove();
      };
      box.append(" ", b);
    }
    return box;
  }

  async function drawGroup(id) {
    const head = $("mx-head"), body = $("mx-thread");
    const t = await api("group.thread", { id });
    const g = t.group, v1 = g.v1 || {};
    const me = (await api("msg.me")).key;
    const admin = g.creator === me || (v1.admins || []).includes(me);
    const memberLine = (g.members || []).map((m) => (m.tag ? "@" + m.tag : (m.address || "").slice(0, 8) + "…") + ((v1.admins || []).includes(m.key) || g.creator === m.key ? " (admin)" : "")).join(", ");
    const acts = h("div", { class: "mx-acts" });
    if (admin) {
      const edit = h("button", { class: "small" }, "Edit");
      edit.onclick = () => groupEditor(g);
      acts.append(edit);
    }
    const leave = h("button", { class: "small" }, "Leave");
    leave.onclick = async () => { if (!leave.dataset.sure) { leave.dataset.sure = 1; leave.textContent = "Leave: sure?"; return; } await api("group.leave", { id }); note(`You left ${g.name}.`); open = null; draw(); };
    acts.append(leave);
    head.replaceChildren(h("div", { class: "mx-ghead" },
      v1.picture ? h("img", { class: "mx-gpic", src: v1.picture, alt: "" }) : "",
      h("div", {}, h("b", {}, g.name), h("div", { class: "s" }, v1.description || ""), h("div", { class: "s" }, memberLine))), acts);
    const rows = [];
    const pinned = v1.pinned && t.lines.find((l) => l.id === v1.pinned);
    if (pinned) rows.push(h("div", { class: "mx-pin" }, "📌 ", pinned.text));
    for (const m of t.lines) {
      const mine = m.dir !== "in";
      const f = await fileView(m);
      const who = (g.members || []).find((x) => x.address === m.peer_address);
      const pin = admin ? h("button", { class: "linkish" }, "pin") : "";
      if (pin) pin.onclick = async () => { await api("group.update", { id, changes: { pinned: m.id } }); note("Pinned."); draw(); };
      rows.push(h("div", { class: "mx-msg " + (mine ? "mine" : "theirs") },
        mine ? "" : h("div", { class: "mx-who" }, who ? (who.tag ? "@" + who.tag : who.address) : m.peer_address),
        m.text ? h("div", { class: "mx-text" }, m.text) : "", f,
        h("div", { class: "mx-meta" }, when(m.at), m.height ? "" : " · waiting for a block", " ", pin)));
    }
    body.replaceChildren(...(rows.length ? rows : [h("div", { class: "mx-empty" }, "No messages in this group yet.")]));
    body.scrollTop = body.scrollHeight;
    $("mx-to-row").hidden = true;
    $("mx-compose").hidden = false;
    if (t.lines.some((m) => m.unread)) { await api("group.read", { id }); refreshBadge(); }
  }

  function groupEditor(g) {
    const v1 = g.v1 || {};
    const body = $("mx-thread");
    const desc = h("input", { value: v1.description || "", placeholder: "Description" });
    const pic = h("input", { type: "file", accept: "image/*" });
    let picData = null;
    pic.onchange = () => {
      const f = pic.files[0]; if (!f) return;
      const img = new Image(), r = new FileReader();
      r.onload = () => { img.onload = () => { const cv = document.createElement("canvas"); cv.width = cv.height = 64; cv.getContext("2d").drawImage(img, 0, 0, 64, 64); picData = cv.toDataURL("image/jpeg", 0.7); }; img.src = r.result; };
      r.readAsDataURL(f);
    };
    const members = (g.members || []).filter((m) => m.key !== g.creator);
    const removeBoxes = members.map((m) => h("label", {}, h("input", { type: "checkbox", value: m.key }), " ", m.tag ? "@" + m.tag : m.address));
    const adminBoxes = members.map((m) => h("label", {}, h("input", { type: "checkbox", value: m.key, checked: (v1.admins || []).includes(m.key) }), " ", m.tag ? "@" + m.tag : m.address));
    const save = h("button", { class: "primary" }, "Save changes");
    save.onclick = async () => {
      try {
        const admins = [g.creator, ...adminBoxes.map((l) => l.querySelector("input")).filter((i) => i.checked).map((i) => i.value)];
        await api("group.update", { id: g.id, changes: { description: desc.value, admins, ...(picData ? { picture: picData } : {}) } });
        const gone = removeBoxes.map((l) => l.querySelector("input")).filter((i) => i.checked).map((i) => i.value);
        if (gone.length) { const r = await api("group.remove", { id: g.id, members: gone }); open = "g:" + r.id; }
        note("Saved. Members see it once it's on chain."); draw();
      } catch (e) { note(e.message, true); }
    };
    body.replaceChildren(h("div", { class: "mx-form" },
      h("h3", {}, "Edit group"), desc, h("label", {}, "Picture ", pic),
      h("div", { class: "k" }, "Admins"), ...adminBoxes,
      members.length ? h("div", { class: "k" }, "Remove (gives everyone else a new group key)") : "", ...removeBoxes,
      h("div", { class: "row" }, save)));
  }

  function newGroup() {
    const body = $("mx-thread");
    $("mx-head").replaceChildren(h("b", {}, "New group"));
    const name = h("input", { placeholder: "Group name" });
    const mem = h("input", { placeholder: "Members: @tags or addresses, separated by commas" });
    const desc = h("input", { placeholder: "Description (optional)" });
    const go = h("button", { class: "primary" }, "Create and invite");
    go.onclick = async () => {
      go.disabled = true;
      try {
        const members = mem.value.split(",").map((x) => x.trim()).filter(Boolean);
        const r = await api("group.create", { name: name.value, members, description: desc.value });
        note(`Group made; ${members.length} invite${members.length === 1 ? "" : "s"} sent (fee ${PEP(r.fee)}).`);
        open = "g:" + r.id; view = "groups"; draw();
      } catch (e) { note(e.message, true); go.disabled = false; }
    };
    body.replaceChildren(h("div", { class: "mx-form" }, name, mem, desc,
      h("p", { class: "s" }, "Each member gets the group's key in a private message; old Dogecoin Arcade accounts can join too."), h("div", { class: "row" }, go)));
    $("mx-compose").hidden = true;
  }

  async function drawThread() {
    const head = $("mx-head"), body = $("mx-thread");
    if (open && open.startsWith("g:")) return drawGroup(open.slice(2));
    if (composing === "group") return newGroup();
    if (composing || !open) {
      head.replaceChildren(h("b", {}, composing ? "New message" : "Messages"), composing ? "" : h("span", { class: "s" }, " · pick a conversation or start a new one"));
      body.replaceChildren();
      $("mx-to-row").hidden = !composing;
      $("mx-compose").hidden = !composing;
      return;
    }
    const t = await api("msg.thread", { peer: open });
    const w = t.who;
    const add = h("button", { class: "small" }, w.contact ? "In contacts" : "Add to contacts");
    add.disabled = w.contact;
    add.onclick = async () => { await api("contacts.save", { key: open, name: w.name || (w.tag ? "@" + w.tag : ""), address: w.address }); note("Added to contacts."); draw(); };
    const mute = h("button", { class: "small" }, "Mute");
    mute.disabled = !w.address;
    mute.onclick = async () => { await api("mute.add", { address: w.address }); note(`Muted ${nameOf(w)}. Unmute from Contacts.`); open = null; draw(); };
    head.replaceChildren(h("div", {}, h("b", {}, nameOf(w)), h("div", { class: "s" }, sub(w) + (w.address ? " · " + w.address : ""))), h("div", { class: "mx-acts" }, add, mute));
    const rows = [];
    for (const m of t.messages) {
      const mine = m.dir !== "in";
      const f = await fileView(m);
      rows.push(h("div", { class: "mx-msg " + (mine ? "mine" : "theirs") },
        m.text ? h("div", { class: "mx-text" }, m.text) : "", f,
        h("div", { class: "mx-meta" }, when(m.at), m.height ? "" : " · waiting for a block", m.fee ? ` · fee ${PEP(m.fee)}` : "")));
    }
    body.replaceChildren(...(rows.length ? rows : [h("div", { class: "mx-empty" }, "No messages yet.")]));
    body.scrollTop = body.scrollHeight;
    $("mx-to-row").hidden = true;
    $("mx-compose").hidden = false;
    if (t.messages.some((m) => m.unread)) { await api("msg.read", { peer: open }); refreshBadge(); }
  }

  async function draw() {
    document.querySelectorAll("#mx-views button").forEach((b) => b.classList.toggle("on", b.dataset.v === view));
    try { await Promise.all([refreshMe(), refreshList(), drawThread()]); } catch (e) { note(e.message, true); }
  }

  // Unread badge on the tab and a notification (sender only, never the text).
  async function refreshBadge() {
    try {
      const u = await api("msg.unread");
      const tab = document.querySelector('nav button[data-tab="messages"]');
      tab.querySelector(".badge")?.remove();
      if (u.unread) tab.append(h("span", { class: "badge" }, String(u.unread)));
      $("mx-req").textContent = u.requests ? ` (${u.requests})` : "";
      if (lastUnread !== null && u.unread + u.requests > lastUnread) {
        const cs = [...await api("msg.conversations", { requests: false }), ...await api("msg.conversations", { requests: true })];
        for (const c of cs.filter((c) => c.unread && !notified.has(c.peer + c.last.at))) {
          notified.add(c.peer + c.last.at);
          notify(`New message from ${nameOf(c.who)}`);
        }
        if (document.querySelector(".tab.on")?.id === "messages") draw();
      }
      lastUnread = u.unread + u.requests;
    } catch (_) { /* no wallet yet */ }
  }
  function notify(text) {
    try {
      if (!("Notification" in window)) return;
      if (Notification.permission === "granted") new Notification("Arcade", { body: text });
      else if (Notification.permission !== "denied") Notification.requestPermission().then((p) => p === "granted" && new Notification("Arcade", { body: text }));
    } catch (_) { /* not available here */ }
  }

  // Sending.
  let attached = null;
  $("mx-file").addEventListener("change", (e) => {
    const f = e.target.files[0];
    if (!f) { attached = null; $("mx-fileinfo").textContent = ""; return; }
    const r = new FileReader();
    r.onload = () => {
      attached = { name: f.name, type: f.type || "application/octet-stream", data: String(r.result).split(",")[1] };
      const txs = Math.max(1, Math.ceil((f.size + 300) / 7600));
      $("mx-fileinfo").textContent = `${f.name} · ${(f.size / 1024).toFixed(1)} KB · about ${txs} transaction${txs === 1 ? "" : "s"} each way`;
    };
    r.readAsDataURL(f);
  });
  $("mx-compose").addEventListener("submit", async (e) => {
    e.preventDefault();
    const text = $("mx-text").value.trim();
    if (!text && !attached) return;
    const to = composing ? $("mx-to").value.trim() : open;
    if (!to) { note("Who to? A @tag, an address or a messaging key.", true); return; }
    const btn = $("mx-send"); btn.disabled = true; note("Sending…");
    if (to.startsWith("g:")) {
      try {
        const r = await api("group.say", { id: to.slice(2), text, file: attached || undefined });
        $("mx-text").value = ""; $("mx-file").value = ""; attached = null; $("mx-fileinfo").textContent = "";
        note(`Sent to the group (fee ${PEP(r.fee)}).`); draw();
      } catch (err) { note(err.message, true); }
      btn.disabled = false;
      return;
    }
    try {
      const r = await api("msg.send", { to, text, file: attached || undefined });
      $("mx-text").value = ""; $("mx-file").value = ""; attached = null; $("mx-fileinfo").textContent = "";
      note(`Sent in ${r.txids.length} transaction${r.txids.length === 1 ? "" : "s"} (fee ${PEP(r.fee)}). It shows on their side once it's in a block.`);
      open = r.to.key; composing = false; draw();
    } catch (err) { note(err.message, true); }
    btn.disabled = false;
  });
  $("mx-to").addEventListener("change", async () => {
    const v = $("mx-to").value.trim();
    if (!v) return;
    try { const w = await api("msg.who", { name: v }); $("mx-towho").textContent = `${nameOf(w)} · ${sub(w)}`; $("mx-towho").className = "s"; }
    catch (e) { $("mx-towho").textContent = e.message; $("mx-towho").className = "s bad"; }
  });
  $("mx-newgroup").onclick = () => { composing = "group"; open = null; view = "groups"; draw(); };
  $("mx-new").onclick = () => { composing = true; open = null; $("mx-to").value = ""; $("mx-towho").textContent = ""; draw(); $("mx-to").focus(); };
  document.querySelectorAll("#mx-views button").forEach((b) => b.addEventListener("click", () => { view = b.dataset.v; draw(); }));
  document.querySelector('nav button[data-tab="messages"]').addEventListener("click", () => draw());

  window.arcadeMessages = { draw };
  refreshBadge();
  setInterval(refreshBadge, 15000);
  setInterval(() => { if (document.querySelector(".tab.on")?.id === "messages") refreshList().catch(() => {}); }, 15000);
})();
