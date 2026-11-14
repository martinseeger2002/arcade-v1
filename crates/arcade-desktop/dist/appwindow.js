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
  // In the app the frame is the arcade's 127.0.0.1 address; in a browser, this page's own origin.
  const frameOk = IN_BROWSER ? frame.startsWith(location.origin + "/app/") && /^\/app\/[a-z]+\/[0-9a-f]{64}$/.test(frame.slice(location.origin.length))
    : /^http:\/\/127\.0\.0\.1:\d+\/app\/[a-z]+\/[0-9a-f]{64}$/.test(frame);
  if (!frameOk) {
    const e = document.getElementById("err");
    e.textContent = "This app can't be opened.";
    e.style.display = "flex";
    return;
  }
  const here = frame.split("/").pop();
  // The chain the app is inscribed on (its frame's address). Its reads answer on that chain; this
  // arcade's wallet writes on its home chain only, so the doors that write say so for another.
  const appChain = frame.split("/")[4];
  let homeChain = null;
  const homeReady = api("chains.list").then((l) => { const h = (l.chains || l || []).find((c) => c.home); homeChain = h ? h.key : null; }).catch(() => {});
  const WRITES = { inscribe: () => true, swap: (m) => m.op !== "items", seed: () => true, claim: () => true, owner: (m) => m.op === "send" };
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

  // Files an app saves (groups.save): the first goes through, more ask once per window.
  const SAVE_MAX = 64 * 1024 * 1024;
  let saves = 0, savesOk = false;
  function base64(buf) {
    const b = new Uint8Array(buf);
    let out = "";
    for (let i = 0; i < b.length; i += 0x8000) out += String.fromCharCode.apply(null, b.subarray(i, i + 0x8000));
    return btoa(out);
  }
  function toast(text) {
    const t = document.createElement("div"); t.className = "toast"; t.textContent = text;
    document.body.append(t);
    setTimeout(() => t.remove(), 5000);
  }
  // The ring this window was opened from, if any (arcade.realtime.ringed).
  const ringId = /^[0-9a-f]{16}$/.test(q.get("ring") || "") ? q.get("ring") : "";

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
        // Answered under both names: `contacts` is the original viewer's, `people` this shim's.
        if (!p.contacts) return { contacts: [], people: [], why: "off" };
        const list = (await api("contacts.list")).filter((c) => c.address).map((c) => ({ address: c.address, tag: c.tag || "" }));
        return { contacts: list, people: list };
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
        case "save": {
          // An app's frame cannot save a file, so this window does, for bytes the app hands over: a file named by
          // the app, put in the Downloads folder beside any file already there. Nothing opens it.
          const bytes = m.data instanceof ArrayBuffer ? m.data : null;
          const file = String(m.name || "file").replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_").slice(0, 120);
          if (!bytes) throw new Error("nothing to save");
          if (bytes.byteLength > SAVE_MAX) throw new Error("too big: a saved file is at most 64 MB");
          if (saves > 0 && !savesOk) {
            const yes = await oneCard(() => card(`Let ${appName} save more files?`, [
              `${appName} has saved a file to your Downloads folder and wants to save another (“${file}”). Allow lets it save files until this window closes.`], "Allow"));
            if (!yes) throw new Error("declined");
            savesOk = true;
          }
          saves++;
          const kept = await invoke("save_file", { name: file, data: base64(bytes) });
          toast(`Saved “${kept}” to Downloads`);
          return { saved: file };
        }
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
        if (rooms[name]) { const old = rooms[name]; delete rooms[name]; fastStop(old); api("realtime.leave", { token: old.token }).catch(() => {}); }
        let r;
        try { r = await api("realtime.join", { page: here, game: String(m.game || ""), room: name, fast: !!m.fast }); }
        catch (err) { return { online: false, why: String(err && err.message || err), me: null, members: [] }; }
        const seat = { name, token: r.token, game: String(m.game || ""), me: r.me, members: {}, wantFast: !!m.fast,
                       link: fastLinkList(m.link), fastMax: fastMaxOf(m.max), fast: null, media: null };
        for (const p of r.members || []) seat.members[p.id] = p;
        rooms[name] = seat;
        if (seat.wantFast) fastStart(name, seat);
        listen(name, seat);
        return { online: true, me: r.me, members: r.members, fast: !!seat.fast, media: !!seat.fast,
                 screen: !!(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia) };
      }
      // Ringing: this window adds WHICH app is ringing (its own page, never what the page says); the arcade adds
      // who (the player's cert). Whether it reaches anybody is for the other person's arcade to decide.
      if (m.op === "ringed") return ringId ? await api("ring.get", { id: ringId, page: here }).catch(() => ({ ring: null })) : { ring: null };
      if (m.op === "ring") {
        const r = await api("ring.send", { page: here, to: String(m.to || ""), line: String(m.line || ""), link: String(m.link || "") });
        return { rung: !!r.rung, why: r.why || "" };
      }
      const seat = rooms[name];
      if (m.op === "leave") {
        delete rooms[name];
        if (seat) { fastStop(seat); await api("realtime.leave", { token: seat.token }).catch(() => {}); }
        return {};
      }
      if (!seat) throw new Error("join the room first");
      if (m.op === "fastroutes") return { routes: fastRoutes(seat) };
      if (m.op === "fastlink") { fastLink(name, seat, m.link, m.max); return { fast: !!seat.fast }; }
      if (m.op === "send") {
        await api("realtime.send", { token: seat.token, data: typeof m.data === "string" ? m.data : JSON.stringify(m.data) });
        return {};
      }
      if (m.op === "media") return await media.door(seat, m);
      throw new Error("realtime ops are join, send, media and leave");
    },
  };

  // Rooms: one seat per room name; each listens for its events and hands them to the page.
  const rooms = {};
  // Camera, microphone and screen on fast rooms (media.js).
  const media = window.ArcadeMedia({ api, push: (ev) => push(ev), perms });
  // Trades between two players of one room (trade.js).
  const trade = window.ArcadeTrade({ api, rooms, toPage: (b) => el.contentWindow?.postMessage(b, "*"), card, oneCard, walletGate });
  doors.swap = (m) => trade.door(m);
  // Prize pools on the chain: a seed for a run (from the pool's referee) and a claim of one lot. The arcade
  // opens the lot with the page's phrase and, for a refereed pool, has its referee check the run.
  doors.seed = async (m) => {
    if (!(await walletGate("play for a prize"))) throw new Error("cancelled");
    return await api("prize.seed", { page: here, pool: m.pool == null ? "" : String(m.pool), game: m.game === true, session: m.session === true,
                                     judge: m.judge ? String(m.judge) : "", referee: m.referee ? String(m.referee) : "" });
  };
  doors.claim = async (m) => {
    if (!(await walletGate("claim a prize"))) throw new Error("cancelled");
    const ask = { page: here, pool: m.pool ? String(m.pool) : "", secret: String(m.secret || ""),
                  replay: m.replay && typeof m.replay === "object" ? { seed: String(m.replay.seed || ""), inputs: m.replay.inputs } : null };
    const pre = await api("prize.claim", ask);
    if (!pre.pool_pays) {
      // The claimer pays: the price to the pool's creator and the network fee, said first.
      if (m.silent === true) throw new Error("this claim costs coins, so the player says yes first");
      const seller = pre.seller_tag ? "@" + pre.seller_tag : "its owner";
      const price = (Number(pre.price) / 1e8).toString();
      const yes = await oneCard(() => card(`Claim ${pre.piece}?`, [
        `${seller} set this aside for whoever got here. It costs ${price} ${price === "1" ? "coin" : "coins"} to ${seller} and a network fee of ${pre.fee_text}. `
        + "What you claim and the coins move in one transaction, or neither does." + (pre.refereed ? " The pool's referee checks your run before it pays." : "")],
        "Claim it"));
      if (!yes) throw new Error("Cancelled.");
    }
    const done = await api("prize.claim", { ...ask, lot: pre.lot, send: true });
    return { txid: done.txid, piece: done.piece };
  };
  const push = (event) => el.contentWindow?.postMessage({ arcade: "realtime", event }, "*");
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));

  // The fast lane (a room joined with {fast: true}), on the original arcade's room wire: hidden room
  // messages that begin with RTC and never reach the page.
  //  - {f: 1, d, r?, b?, t?}: fast data carried by the room. r: sent reliable; b: d is base64 of
  //    bytes; t: only for these player ids.
  //  - {to, k, i, n, d}: piece i of n of signal k for one player. The signal is JSON {t}: 'o' and 'a'
  //    (the two halves of a direct link), 'h' (please offer) and 'n' (I don't link with you).
  // This window makes no WebRTC links (it has none). A pair whose arcades are both V1.0 goes over the
  // arcades' own fast lane instead (realtime.fast / realtime.lanes): state 'direct'. Every other pair
  // is carried by the room: at most three hidden messages a second, signals first, then the reliable
  // sends in order, and of the rest only the latest. An original arcade that offers a WebRTC link gets
  // no answer and carries the pair over the room after its own wait.
  const RTC = "\u001farcade-rtc:", TRADE = "\u001farcade-trade:";
  const FAST_MAX = 16384, FAST_PEERS = 19, FAST_PEERS_MAX = 60, FAST_TO_MAX = 64, RTC_PIECE = 300, FAST_LANE_WAIT = 5000, LANES_EVERY = 1000;
  const fastLinkList = (link) => (Array.isArray(link) ? link.slice(0, FAST_PEERS_MAX).map(String) : null);
  const fastMaxOf = (max) => Math.max(1, Math.min(FAST_PEERS_MAX, (max | 0) || FAST_PEERS));
  const fastLinks = (s, id) => !s.link || s.link.includes(String(id));
  function fastB64(buf) { const u = new Uint8Array(buf); let t = ""; for (let i = 0; i < u.length; i++) t += String.fromCharCode(u[i]); return btoa(t); }
  function fastUnB64(text) { const t = atob(text), u = new Uint8Array(t.length); for (let i = 0; i < t.length; i++) u[i] = t.charCodeAt(i); return u.buffer; }
  function fastStart(name, s) {
    const fast = (s.fast = { peers: {}, out: [], parts: {}, latest: null, reliable: [], k: 0, pump: null,
                             lane: { out: [], busy: false }, routes: {} });
    for (const id in s.members) fastMeet(name, s, s.members[id]);
    // Which players the arcades' fast lane reaches: those pairs go 'direct', the rest stay on the room.
    const look = async () => {
      if (s.fast !== fast || rooms[name] !== s) return;
      let r;
      try { r = await api("realtime.lanes", { token: s.token }); } catch (err) { r = null; }
      if (s.fast !== fast || !r) return;
      const up = new Set(r.up || []);
      fast.routes = r.routes || {};
      for (const id in fast.peers) {
        const peer = fast.peers[id];
        if (up.has(id) && peer.state !== "direct") { clearTimeout(peer.timer); fastSay(name, s, peer.p, "direct"); }
        else if (!up.has(id) && peer.state === "direct") { fastSay(name, s, peer.p, "room"); fastPump(name, s); }
      }
    };
    fast.lanes = setInterval(look, LANES_EVERY);
    look();
  }
  function fastStop(s) {
    if (!s) return;
    media.close(s);
    if (!s.fast) return;
    for (const id in s.fast.peers) clearTimeout(s.fast.peers[id].timer);
    clearInterval(s.fast.pump);
    clearInterval(s.fast.lanes);
    s.fast = null;
  }
  function fastSay(name, s, p, state) {
    const peer = s.fast && s.fast.peers[p.id];
    if (peer) { if (peer.state === state) return; peer.state = state; }
    push({ type: "peer", room: name, member: p, state });
  }
  function fastMeet(name, s, p) {
    if (!s.fast || !s.me || p.id === s.me.id || s.fast.peers[p.id]) return;
    if (!fastLinks(s, p.id)) return;                 // the page does not want this link
    if (Object.keys(s.fast.peers).length >= s.fastMax) return;
    const fast = s.fast, peer = (fast.peers[p.id] = { p, state: "" });
    fastSay(name, s, p, "connecting");
    // No lane by then: the room carries the pair (and it goes 'direct' if a lane comes up later).
    peer.timer = setTimeout(() => {
      if (s.fast !== fast || fast.peers[p.id] !== peer || peer.state === "direct") return;
      fastSay(name, s, peer.p, "room");
      fastPump(name, s);
    }, FAST_LANE_WAIT);
  }
  function fastDrop(s, id) {
    const peer = s.fast && s.fast.peers[id];
    if (!peer) return;
    media.gone(s, id);
    clearTimeout(peer.timer);
    delete s.fast.peers[id];
  }
  function fastLink(name, s, link, max) {
    s.link = fastLinkList(link); s.fastMax = fastMaxOf(max);
    if (!s.fast) return;
    for (const id in s.fast.peers) if (!fastLinks(s, id)) {
      const p = s.fast.peers[id].p; fastDrop(s, id); fastSay(name, s, p, "gone");
      fastSignal(name, s, p.id, { t: "n" });
    }
    for (const id in s.members) {
      // A player newly linked with is asked to link back: they may have let this one go.
      const had = !!s.fast.peers[id];
      fastMeet(name, s, s.members[id]);
      if (!had && s.fast.peers[id]) fastSignal(name, s, id, { t: "h" });
    }
  }
  const fastViaRoom = (s) => Object.values(s.fast.peers).some((peer) => peer.state === "room");
  function fastRoutes(s) {
    const out = {};
    if (s.fast) for (const id in s.fast.peers) {
      const st = s.fast.peers[id].state;
      out[id] = st === "room" ? "none" : st === "direct" ? (s.fast.routes[id] || "direct") : "connecting";
    }
    return out;
  }
  function fastSignal(name, s, to, body) {
    const text = JSON.stringify(body), k = (++s.fast.k).toString(36), n = Math.ceil(text.length / RTC_PIECE);
    for (let i = 0; i < n; i++) s.fast.out.push({ to, k, i, n, d: text.slice(i * RTC_PIECE, (i + 1) * RTC_PIECE) });
    fastPump(name, s);
  }
  function fastPump(name, s) {
    if (!s.fast || s.fast.pump) return;
    const fast = s.fast;
    fast.pump = setInterval(() => {
      if (s.fast !== fast || rooms[name] !== s) { clearInterval(fast.pump); return; }
      let msg = fast.out.shift();
      if (!msg && fastViaRoom(s)) {
        let it = null;
        if (fast.reliable.length) { it = fast.reliable.shift(); msg = { f: 1, r: 1, d: it.d }; }
        else if (fast.latest != null) { it = fast.latest; msg = { f: 1, d: it.d }; fast.latest = null; }
        if (it && it.b) msg.b = 1;
        if (it && it.t) msg.t = it.t;
      }
      if (!msg) {
        if (!fastViaRoom(s)) { clearInterval(fast.pump); fast.pump = null; }
        return;
      }
      api("realtime.send", { token: s.token, data: RTC + JSON.stringify(msg) }).catch(() => {});
    }, 340);
  }
  const fastHeard = (name, p, data) => push({ type: "fast", room: name, member: p, data });
  // A hidden fast-lane message heard in the room.
  function rtcHeard(name, s, p, text) {
    let msg; try { msg = JSON.parse(text); } catch (err) { return; }
    if (!msg || typeof msg !== "object" || !s.fast || !p || !s.me || p.id === s.me.id) return;
    if (msg.f) {
      if (msg.t && (!Array.isArray(msg.t) || !msg.t.includes(s.me.id))) return;   // sent to others only
      if (typeof msg.d === "string") { try { fastHeard(name, p, msg.b ? fastUnB64(msg.d) : msg.d); } catch (err) {} }
      return;
    }
    if (msg.to !== s.me.id || typeof msg.d !== "string") return;
    const key = p.id + "/" + msg.k, got = s.fast.parts[key] || (s.fast.parts[key] = []);
    got[msg.i | 0] = msg.d;
    const n = msg.n | 0;
    if (n < 1 || n > 8) { delete s.fast.parts[key]; return; }
    for (let i = 0; i < n; i++) if (got[i] == null) return;
    delete s.fast.parts[key];
    let o; try { o = JSON.parse(got.join("")); } catch (err) { return; }
    if (!o) return;
    if (o.t === "n") {                              // they do not link with us: let them go
      if (s.fast.peers[p.id]) { fastDrop(s, p.id); fastSay(name, s, p, "gone"); }
    } else if (o.t === "o" || o.t === "h") {
      // They want a direct link. One the page does not link with is told so; the rest get no
      // answer, and the pair stays on the room.
      if (!s.fast.peers[p.id]) fastMeet(name, s, p);
      if (!s.fast.peers[p.id] && !fastLinks(s, p.id)) fastSignal(name, s, p.id, { t: "n" });
    }
  }
  // The page's fast.send: through the room to the players it is for.
  function fastSend(s, m) {
    if (!s.fast) return;
    const bin = m.data instanceof ArrayBuffer;
    const data = bin ? m.data : String(m.data == null ? "" : m.data);
    if ((bin ? data.byteLength : data.length) > FAST_MAX) return;
    const to = Array.isArray(m.to) ? m.to.slice(0, FAST_TO_MAX).map(String) : null;
    let viaRoom = false;
    const lane = [];
    for (const id in s.fast.peers) {
      if (to && !to.includes(id)) continue;
      const st = s.fast.peers[id].state;
      if (st === "direct") lane.push(id); else if (st === "room") viaRoom = true;
    }
    if (lane.length) laneSend(s, { to: lane.slice(0, FAST_TO_MAX), reliable: !!m.reliable, ...(bin ? { b64: fastB64(data) } : { data }) });
    if (!viaRoom) return;
    const d = bin ? fastB64(data) : data, it = { d };
    if (bin) it.b = 1;
    if (to) it.t = to;
    if (d.length <= 400 && JSON.stringify(it).length <= 470) {
      if (m.reliable) { if (s.fast.reliable.length < 50) s.fast.reliable.push(it); }
      else s.fast.latest = it;
    }
  }

  // Sends over the arcades' lane, one at a time so reliable ones keep their order; an unreliable send
  // still waiting is replaced by the newest for the same players. Reliable sends are never dropped:
  // when the arcade says "too fast" the send waits a moment and goes again (a data channel buffers too).
  const LANE_RELIABLE_MAX = 10000;
  function laneSend(s, msg) {
    const lane = s.fast.lane, key = msg.reliable ? null : msg.to.join(",");
    if (key !== null) {
      const i = lane.out.findIndex((x) => !x.reliable && x.to.join(",") === key);
      if (i >= 0) { lane.out[i] = msg; return; }
    } else if (lane.out.length >= LANE_RELIABLE_MAX) return;     // as a full data channel: no more for now
    lane.out.push(msg);
    laneNext(s, s.fast);
  }
  function laneNext(s, fast) {
    const lane = fast.lane;
    if (lane.busy || s.fast !== fast) return;
    const msg = lane.out.shift();
    if (!msg) return;
    lane.busy = true;
    api("realtime.fast", { token: s.token, ...msg }).then(() => 0, (err) => {
      if (msg.reliable && /too fast/.test(String(err && err.message || err))) { lane.out.unshift(msg); return 50; }
      return 0;
    }).then((wait) => {
      const go = () => { lane.busy = false; laneNext(s, fast); };
      if (wait) setTimeout(go, wait); else go();
    });
  }

  // One room event: the seat's own list of who is here, the fast lane's part, then the page's.
  function hear(name, s, ev) {
    if (!ev || ev.type === "reconnecting") return;
    ev.room = name;
    const p = ev.member;
    if (p && ev.type === "join") {
      s.members[p.id] = p;
      if (s.fast && s.fast.peers[p.id]) s.fast.peers[p.id].p = p;
      fastMeet(name, s, p);
    }
    if (p && ev.type === "leave") {
      // The seat they left behind on an arcade they dropped from, after coming back through
      // another: they are still here.
      const now = s.members[p.id];
      if (now && now.node && p.node && now.node !== p.node) return;
      delete s.members[p.id];
      if (s.fast && s.fast.peers[p.id]) { fastDrop(s, p.id); fastSay(name, s, p, "gone"); }
    }
    if (ev.type === "mchunk") { media.heard(s, ev); return; }   // pieces of a call: the window's own
    if (ev.type === "fast") {
      // From the arcades' lane: only from a player this page links with, and bytes as an ArrayBuffer.
      if (!p || !s.fast || !s.fast.peers[p.id]) return;
      let data = ev.data;
      if (typeof ev.b64 === "string") { try { data = fastUnB64(ev.b64); } catch (err) { return; } }
      fastHeard(name, p, data);
      return;
    }
    if (ev.type === "message" && typeof ev.data === "string") {
      if (ev.data.startsWith(RTC)) { rtcHeard(name, s, p, ev.data.slice(RTC.length)); return; }
      if (ev.data.startsWith(TRADE)) { trade.heard(name, p, ev.data.slice(TRADE.length)); return; }   // the windows' own talk: never the page's
    }
    push(ev);
  }
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
      for (const ev of r.events || []) hear(name, seat, ev);
      if (r.gone) {
        // The seat was let go (the service restarted): take it again, and tell the page who left
        // and who is here now.
        try {
          const j = await api("realtime.join", { page: here, game: seat.game, room: name, fast: seat.wantFast });
          if (rooms[name] !== seat) { api("realtime.leave", { token: j.token }).catch(() => {}); return; }
          const before = seat.members, was = seat.me;
          fastStop(seat);
          seat.token = j.token; seat.me = j.me; seat.members = {}; fails = 0;
          for (const p of j.members || []) seat.members[p.id] = p;
          if (seat.wantFast) fastStart(name, seat);
          push({ type: "rejoined", room: name, member: j.me, media: null });
          if (j.me && (!was || j.me.id !== was.id)) push({ type: "me", room: name, member: j.me });
          for (const id in before) if (!seat.members[id]) push({ type: "leave", room: name, member: before[id] });
          for (const id in seat.members) if (!before[id]) push({ type: "join", room: name, member: seat.members[id] });
        } catch (err) { await pause(2000); }
      }
    }
  }
  window.addEventListener("pagehide", () => {
    for (const k of Object.keys(rooms)) { fastStop(rooms[k]); api("realtime.leave", { token: rooms[k].token }).catch(() => {}); delete rooms[k]; }
  });

  window.addEventListener("message", async (e) => {
    if (e.source !== el.contentWindow) return;
    const m = e.data;
    if (!m || typeof m !== "object" || typeof m.seq !== "number" || !m.arcade) return;
    if (m.arcade === "realtime" && m.op === "fast") {   // many a second: never answered
      const s = rooms[String(m.room == null ? "" : m.room)];
      if (s) fastSend(s, m);
      return;
    }
    if (m.arcade === "inscribe" || m.arcade === "claim") el.contentWindow.postMessage({ arcade: m.arcade, seq: m.seq, heard: true }, "*");
    const door = doors[m.arcade];
    let body;
    if (WRITES[m.arcade] && WRITES[m.arcade](m)) await homeReady;
    if (!door) body = { ok: false, error: `${m.arcade} is coming to Arcade V1.0` };
    else if (WRITES[m.arcade] && WRITES[m.arcade](m) && homeChain && appChain !== homeChain)
      body = { ok: false, error: `this app is on ${appChain}: this arcade's wallet writes on ${homeChain} only` };
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

  // The page with its pass for the page API (a new address, so a copy cached before passes isn't used).
  el.src = frame + "?pass=1";
  window.arcadeApp = { id, frame, el };
})();
