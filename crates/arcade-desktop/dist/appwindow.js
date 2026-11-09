// The app window: run one app sandboxed in a frame and answer what it asks (the original arcade's
// viewer "doors"): storage through this arcade, its window state, rooms over the arcade mesh.
// Only messages from the app's own frame are answered; the page's identity is the page this
// window opened, never what a message says.
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
  const pep = homeCoins;
  let lastSend = 0;
  // Wallet access: what uses the player's key asks first while the switch is off (Yes turns it on).
  async function walletGate(what) {
    const p = await perms();
    if (p.wallet) return true;
    const yes = await oneCard(() => card(`Let ${appName} use your wallet?`, [
      `${appName} wants to ${what}. That needs Wallet access; yes turns it on for this app (you can turn it off on its card).`,
      "Check this is the app you meant to open: copies of popular apps can look the same."], "Allow"));
    if (yes) await api("apps.perms", { id, set: { wallet: true } });
    return yes;
  }

  const doors = {
    async node(m) {
      if (m.op === "identity") return await api("apps.node", { op: "identity" });
      if (m.op === "send") return await api("apps.node", { op: "send", page: here, to: String(m.to || ""), body: m.body });
      if (m.op === "sent") return await api("apps.node", { op: "sent", page: here });
      if (m.op === "replies") return await api("apps.node", { op: "replies", page: here, after: m.after || 0, limit: m.limit || 100 });
      throw new Error("node ops are identity, send, sent and replies");
    },
    async state(m) {
      throw new Error("this arcade has no referee for that game yet: its state can be read, not written, here");
    },
    async inscribe(m) {
      if (!(await walletGate("inscribe something as yours"))) throw new Error("Cancelled.");
      const encoding = m.encoding === "base64" ? "base64" : "";
      const data = typeof m.data === "string" ? m.data : JSON.stringify(m.data);
      const ctype = String(m.contenttype || "text/plain").slice(0, 255);
      const json = m.json && typeof m.json === "object" ? JSON.stringify(m.json) : (typeof m.json === "string" ? m.json : undefined);
      const size = encoding ? Math.floor(data.length * 3 / 4) : new TextEncoder().encode(data).length;
      if (size > 7000) throw new Error("too large: a page may inscribe up to 7,000 bytes (one transaction)");
      const est = await api("inscribe.estimate", { data, encoding, contenttype: ctype, json });
      const lines = [`${appName} asks to inscribe ${size.toLocaleString()} bytes of ${ctype} as yours, on the chain for good.`];
      if (m.note) lines.push(`“${String(m.note).slice(0, 200)}”`);
      lines.push(`Network fee about ${pep(est.fee)}, and ${pep(est.locked)} kept in its data outputs (yours, spendable later).`);
      const yes = await oneCard(() => card("Inscribe this?", lines, "Inscribe"));
      if (!yes) throw new Error("Cancelled.");
      const r = await api("inscribe", { data, encoding, contenttype: ctype, json });
      return { txid: r.txid };
    },
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
      const name = String(m.room == null ? "" : m.room);
      if (m.op === "join") {
        if (rooms[name]) { const old = rooms[name]; delete rooms[name]; api("realtime.leave", { token: old.token }).catch(() => {}); }
        let r;
        try { r = await api("realtime.join", { page: here, game: String(m.game || ""), room: name }); }
        catch (err) { return { online: false, why: String(err && err.message || err), me: null, members: [] }; }
        const seat = { token: r.token, game: String(m.game || "") };
        rooms[name] = seat;
        listen(name, seat);
        return { online: true, me: r.me, members: r.members, fast: false, media: false, screen: false };
      }
      if (m.op === "send") {
        const seat = rooms[name];
        if (!seat) throw new Error("join the room first");
        await api("realtime.send", { token: seat.token, data: typeof m.data === "string" ? m.data : JSON.stringify(m.data) });
        return {};
      }
      if (m.op === "leave") {
        const seat = rooms[name]; delete rooms[name];
        if (seat) await api("realtime.leave", { token: seat.token }).catch(() => {});
        return {};
      }
      if (m.op === "ringed") return { ring: null };
      if (m.op === "ring") return { rung: false, why: "ringing is coming to Arcade V1.0" };
      if (m.op === "media") throw new Error("media needs a room joined with {fast: true}");
      throw new Error("realtime ops are join, send, media and leave");
    },
  };

  // Rooms: one seat per room name; each listens for its events and hands them to the page.
  const rooms = {};
  const push = (event) => el.contentWindow?.postMessage({ arcade: "realtime", event }, "*");
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));
  async function listen(name, seat) {
    let fails = 0;
    while (rooms[name] === seat) {
      let r;
      try { r = await api("realtime.poll", { token: seat.token, wait: 10000 }); fails = 0; }
      catch (err) {
        // The arcade service is restarting: say so once, keep trying.
        if (fails++ === 0) push({ type: "reconnecting", room: name });
        await pause(Math.min(5000, 500 * fails));
        if (fails > 1) r = { events: [], gone: true }; else continue;
      }
      if (rooms[name] !== seat) return;
      for (const ev of r.events || []) if (ev.type !== "reconnecting") push(ev);
      if (r.gone) {
        // The seat was let go (the service restarted): take it again.
        try {
          const j = await api("realtime.join", { page: here, game: seat.game, room: name });
          seat.token = j.token; fails = 0;
          push({ type: "rejoined", room: name, member: j.me, media: null });
          for (const p of j.members || []) if (!j.me || p.id !== j.me.id) push({ type: "join", room: name, member: p });
        } catch (err) { await pause(2000); }
      }
    }
  }
  window.addEventListener("pagehide", () => {
    for (const k of Object.keys(rooms)) { api("realtime.leave", { token: rooms[k].token }).catch(() => {}); delete rooms[k]; }
  });

  window.addEventListener("message", async (e) => {
    if (e.source !== el.contentWindow) return;
    const m = e.data;
    if (!m || typeof m !== "object" || typeof m.seq !== "number" || !m.arcade) return;
    if (m.arcade === "realtime" && m.op === "fast") return; // never answered
    if (m.arcade === "inscribe") el.contentWindow.postMessage({ arcade: "inscribe", seq: m.seq, heard: true }, "*");
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
