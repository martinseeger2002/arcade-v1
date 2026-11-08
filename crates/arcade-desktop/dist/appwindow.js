// The app window: run one app sandboxed in a frame and answer what it asks (the original arcade's
// viewer "doors"): storage through this arcade, its window state, rooms (offline until the mesh
// carries them). Only messages from the app's own frame are answered; the page's identity is the
// page this window opened, never what a message says.
(() => {
  const q = new URLSearchParams(location.search);
  const frame = q.get("frame") || "";
  const id = q.get("id") || "";
  document.title = q.get("title") || "App";
  const el = document.getElementById("app");
  if (!/^http:\/\/127\.0\.0\.1:\d+\/app\/[a-z]+\/[0-9a-f]{64}$/.test(frame)) {
    const e = document.getElementById("err");
    e.textContent = "This app can't be opened.";
    e.style.display = "flex";
    return;
  }
  const here = frame.split("/").pop();
  const invoke = window.__TAURI__.core.invoke;
  async function api(cmd, args = {}) {
    const r = await invoke("api", { req: { cmd, ...args } });
    if (!r.ok) throw new Error(r.error || "failed");
    return r.result;
  }
  let appState = { state: "open", timeout: null, closesAt: null, inShell: false };

  const perms = async () => (id ? await api("apps.perms", { id }) : {});
  const appName = document.title;

  // Cards drawn over the app, outside its sandbox: the app can't see, draw or press them.
  let busy = false;
  function card(title, lines, yes, no = "No") {
    return new Promise((resolve) => {
      const ov = document.createElement("div"); ov.className = "ov";
      const box = document.createElement("div"); box.className = "box";
      const h = document.createElement("h3"); h.textContent = title; box.append(h);
      for (const l of lines) { const p = document.createElement("p"); if (typeof l === "string") p.textContent = l; else p.append(l); box.append(p); }
      const row = document.createElement("div"); row.className = "btns";
      const n = document.createElement("button"); n.textContent = no;
      const y = document.createElement("button"); y.textContent = yes; y.className = "yes";
      const done = (v) => { ov.remove(); resolve(v); };
      n.onclick = () => done(false); y.onclick = () => done(true);
      row.append(n, y); box.append(row); ov.append(box); document.body.append(ov);
      y.focus();
    });
  }
  async function oneCard(fn) {
    if (busy) throw new Error("busy: this page already has a question open; ask again once it is answered");
    busy = true;
    try { return await fn(); } finally { busy = false; }
  }
  const pep = (sats) => (sats / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 }) + " PEPtest";
  let lastSend = 0;
  const doors = {
    async access(m) {
      const p = await perms();
      if (m.op === "status") return { wallet: !!p.wallet, signedIn: true, node: new URL(frame).host, game: !!id, name: appName };
      if (m.op === "wallet") {
        if (p.wallet) return { wallet: true, status: "already" };
        const yes = await oneCard(() => card(`Let ${appName} use your wallet?`, [
          `${appName} asks for Wallet access: trades, shops, escrow, claims and inscribing in its name. Every payment is still shown to you on its own card first.`,
          "Check this is the app you meant to open: copies of popular apps can look the same."], "Allow"));
        if (!yes) return { wallet: false, status: "declined" };
        await api("apps.perms", { id, set: { wallet: true } });
        return { wallet: true, status: "allowed" };
      }
      if (m.op === "nodes") { const h = new URL(frame); return { nodes: [{ base: h.origin, host: h.host, ms: 0, here: true, arcade: true, affiliate: false, name: "this arcade", by: "" }] }; }
      if (m.op === "move") { if (String(m.to || "") === new URL(frame).origin) return { status: "here" }; throw new Error("not an arcade this node knows: pick one from nodes()"); }
      throw new Error("access ops are status, wallet, nodes and move");
    },
    async contacts(m) {
      if (m.op === "list") {
        const p = await perms();
        if (!p.contacts) return { people: [], why: "off" };
        return { people: (await api("contacts.list")).filter((c) => c.tag || c.address).map((c) => ({ address: c.address, tag: c.tag })) };
      }
      const a = String(m.address || "").trim();
      if (!/^@?[A-Za-z0-9_]{1,24}$/.test(a) && !/^[A-Za-z0-9]{25,90}$/.test(a)) throw new Error("not an address: give an address or an @tag");
      const name = typeof m.name === "string" ? m.name.trim().slice(0, 60) : "";
      const list = await api("contacts.list");
      const isTag = /^@?[A-Za-z0-9_]{1,24}$/.test(a) && !/^[A-Za-z0-9]{25,90}$/.test(a);
      const tag = isTag ? a.replace(/^@/, "").toLowerCase() : null;
      const found = list.find((c) => (tag && c.tag === tag) || c.address === a);
      if (m.op === "add") {
        if (found) return { status: "already" };
        let w;
        try { w = await api("msg.who", { name: isTag ? "@" + tag : a }); } catch (e) { throw new Error(e.message.includes("not claimed") ? `nobody holds @${tag} on this chain` : e.message); }
        const ok = await oneCard(() => card(`${appName} would like to add a contact`, [`Add ${w.tag ? "@" + w.tag : ""} (${(w.address || "").slice(0, 10)}…) to your address book${name ? ` as “${name}”` : ""}?`], "Add"));
        if (!ok) throw new Error("declined");
        await api("contacts.save", { key: w.key, name: name || (w.tag ? "@" + w.tag : ""), address: w.address });
        return { status: "added" };
      }
      if (m.op === "remove") {
        if (!found) return { status: "absent" };
        const ok = await oneCard(() => card(`${appName} would like to remove a contact`, [`Remove ${found.tag ? "@" + found.tag : found.address} from your address book?`], "Remove"));
        if (!ok) throw new Error("declined");
        await api("contacts.delete", { key: found.key });
        return { status: "removed" };
      }
      throw new Error("contacts ops are list, add and remove");
    },
    async owner(m) {
      if (m.op === "identity") return await api("apps.owner", { page: here });
      if (m.op === "send") { const { arcade, seq, op, ...request } = m; return await api("apps.owner_send", { page: here, request }); }
      throw new Error("owner ops are identity and send");
    },
    async groups(m) {
      const p = await perms();
      if (!p.groups) throw new Error("off: turn on Group chats on this app's card");
      switch (m.op) {
        case "list": return await api("group.app_list");
        case "read": return await api("group.app_read", { id: m.group, after: m.after || 0, limit: m.limit || 200 });
        case "send": {
          const text = String(m.text || "");
          if (!text.trim()) throw new Error("nothing to send");
          if (text.length > 2000) throw new Error("too long: at most 2000 characters");
          if (Date.now() - lastSend < 400) throw new Error("busy: wait a moment");
          lastSend = Date.now();
          const r = await api("group.say", { id: m.group, text });
          return { txid: r.id, fee: r.fee };
        }
        case "leave": { await api("group.leave", { id: m.group }); return { left: m.group }; }
        case "seal": return await api("group.seal", { id: m.group, text: String(m.text || "") });
        case "open": return await api("group.open", { id: m.group, sealed: String(m.sealed || "") });
        case "people": return { people: (await api("contacts.list")).map((c) => ({ address: c.address, tag: c.tag })) };
        case "add": return await api("group.add", { id: m.group, members: (m.members || []).slice(0, 50).map((x) => typeof x === "string" ? x : (x.tag ? "@" + x.tag : x.address)) });
        case "create": {
          const name = String(m.name || "").slice(0, 60);
          const members = (m.members || []).slice(0, 50).map((x) => typeof x === "string" ? x : (x.tag ? "@" + x.tag : x.address));
          if (!name) throw new Error("a group needs a name");
          const r = await api("group.create", { name, members });
          return { group: (await api("group.app_list")).groups.find((g) => g.id === r.id) || { id: r.id, name } };
        }
        case "save": throw new Error("saving files from an app is coming to Arcade V1.0");
        default: throw new Error("groups ops are list, read, send, leave, seal, open, people, add, create and save");
      }
    },
    async storage(m) {
      const r = await api("apps.storage", { page: here, op: m.op, key: m.key, value: m.value });
      return r;
    },
    async app(m) {
      if (m.op !== "state") throw new Error("app: op is state");
      return { ...appState };
    },
    async realtime(m) {
      if (m.op === "join") return { online: false, why: "rooms are coming to Arcade V1.0: playing alone for now", me: null, members: [] };
      if (m.op === "leave") return {};
      if (m.op === "ringed") return { ring: null };
      if (m.op === "ring") return { rung: false, why: "ringing is coming to Arcade V1.0" };
      throw new Error("not in a room: rooms are coming to Arcade V1.0");
    },
  };

  window.addEventListener("message", async (e) => {
    if (e.source !== el.contentWindow) return;
    const m = e.data;
    if (!m || typeof m !== "object" || typeof m.seq !== "number" || !m.arcade) return;
    if (m.arcade === "realtime" && m.op === "fast") return; // never answered
    const door = doors[m.arcade];
    let body;
    if (!door) body = { ok: false, error: `${m.arcade} is coming to Arcade V1.0` };
    else {
      try { body = { ok: true, ...(await door(m)) }; }
      catch (err) { body = { ok: false, error: String(err && err.message || err) }; }
    }
    body.arcade = m.arcade; body.seq = m.seq;
    el.contentWindow.postMessage(body, "*");
  });

  // Window state for arcade.app: minimized / restored / closing.
  const tell = (state) => { appState = { ...appState, state }; el.contentWindow?.postMessage({ arcade: "app", event: true, ...appState }, "*"); };
  document.addEventListener("visibilitychange", () => tell(document.hidden ? "minimized" : "restored"));
  window.addEventListener("beforeunload", () => tell("closing"));

  // Requests this page filed with POST /r/send: shown over it, one at a time, outside its reach.
  let showing = null;
  setInterval(async () => {
    if (showing) return;
    let list = [];
    try { list = await api("wallet.requests", { page: here }); } catch (_) { return; }
    const r = list[0];
    if (!r) return;
    showing = r.id;
    try {
      let pv;
      try { pv = await api("wallet.preview", { id: r.id }); }
      catch (e) { await card("This request can't be sent", [e.message], "Refuse", "Close"); await api("wallet.deny", { id: r.id }); return; }
      const who = pv.page_card || {};
      const from = `${who.app || appName}${who.creator_tag ? " by @" + who.creator_tag : ""}`;
      const outs = document.createElement("ul"); outs.className = "outs";
      for (const o of pv.outputs) { const li = document.createElement("li"); li.textContent = o.data ? "data (the arcade record)" : `${pep(o.value)} → ${o.address}${o.mine ? " (back to you)" : ""}`; outs.append(li); }
      const lines = [`${from} asks to send ${pv.what} to ${pv.to_tag ? "@" + pv.to_tag + " " : ""}${pv.to}.`];
      if (pv.note) lines.push(`“${pv.note}”`);
      lines.push(outs, `Network fee: ${pep(pv.fee)}`);
      const yes = await card("Approve this send?", lines, "Approve and send", "Refuse");
      await api(yes ? "wallet.approve" : "wallet.deny", { id: r.id });
    } catch (_) { /* answered elsewhere */ }
    finally { showing = null; }
  }, 1500);

  el.src = frame;
  window.arcadeApp = { id, frame, el };
})();
