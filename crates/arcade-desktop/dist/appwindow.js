// The app window: run one app sandboxed in a frame and answer what it asks (the original arcade's
// viewer "doors"): storage through this arcade, its window state, rooms over the arcade mesh.
// Only messages from the app's own frame are answered; the page's identity is the page this
// window opened, never what a message says.
(() => {
  const q = new URLSearchParams(location.search);
  // The app's page and its key (apps.launch gives "<page>?pass=<key>"): asked for with the key, the arcade serves
  // the page with its pass for the page API; a bare request for the page gets none.
  const [frame, passKey] = (() => { const f = q.get("frame") || ""; const m = /^(.*)\?pass=([0-9a-f]{32})$/.exec(f); return m ? [m[1], m[2]] : [f, ""]; })();
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
  let appState = { state: "open", timeout: null, closesAt: null, inShell: false, card: null };

  const perms = async () => (id ? await api("apps.perms", { id }) : {});
  const appName = document.title;

  // Cards drawn over the app, outside its sandbox: the app can't see, draw or press them.
  let busy = false;
  // The app hears when a card waits on the person and when it is answered (arcade.app's state: card = its title, or
  // null), so it can say "answer the card" instead of looking stuck (2026-11-22: a page waiting on a card in a window
  // nobody watched looked like a hang).
  function cardState(title) {
    appState = { ...appState, card: title };
    el.contentWindow?.postMessage({ arcade: "app", event: true, ...appState }, "*");
  }
  function card(title, lines, yes, no = "No") {
    cardState(title);
    return new Promise((resolve) => {
      const ov = document.createElement("div"); ov.className = "ov";
      const box = document.createElement("div"); box.className = "box";
      const h = document.createElement("h3"); h.textContent = title; box.append(h);
      for (const l of lines) { const p = document.createElement("p"); if (typeof l === "string") p.textContent = l; else p.append(l); box.append(p); }
      const row = document.createElement("div"); row.className = "btns";
      const n = document.createElement("button"); n.textContent = no;
      const y = document.createElement("button"); y.textContent = yes; y.className = "yes";
      const done = (v) => { ov.remove(); cardState(null); resolve(v); };
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

  // Files an app saves (arcade.files.save, and groups.save for apps written before it): the window asks once
  // ("Let <app> save files?"), then the app saves until the window closes. groups.save keeps its old rule: behind the
  // Group chats switch, the first file goes through and more ask once.
  const SAVE_MAX = 128 * 1024 * 1024;
  let saves = 0, savesOk = false;
  async function saveFile(m, askFirst) {
    // An app's frame cannot save a file, so this window does, for bytes the app hands over: a file named by the app,
    // put in the Downloads folder beside any file already there. Nothing opens it.
    const bytes = m.data instanceof ArrayBuffer ? m.data : null;
    const file = String(m.name || "file").replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_").slice(0, 120);
    if (!bytes) throw new Error("nothing to save");
    if (bytes.byteLength > SAVE_MAX) throw new Error("too big: a saved file is at most 128 MB");
    if ((askFirst || saves > 0) && !savesOk) {
      const mb = (bytes.byteLength / 1048576).toFixed(bytes.byteLength < 10485760 ? 1 : 0);
      const yes = await oneCard(() => card(`Let ${appName} save files?`, [
        `${appName} wants to save “${file}” (${mb} MB) to your Downloads folder. Allow lets it save files until this window closes.`], "Allow"));
      if (!yes) throw new Error("declined");
      savesOk = true;
    }
    saves++;
    const kept = await invoke("save_file", { name: file, data: base64(bytes) });
    toast(`Saved “${kept}” to Downloads`);
    return { saved: kept };
  }
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

  // Shared models (arcade.llm, through the Models app's helper): the app spends this wallet's model credit only after a
  // yes, once a window.
  let llmYes = false;
  // Services over the mesh (arcade.svc, docs/SVC.md): asking in the player's name, and managing this computer's
  // helper, each after a yes once a window and service; each payment and new key on its own card.
  const svcAskYes = {}, svcLocalYes = {};
  // An app the person gave an allowance for a service (apps.allowance, 2026-11-21) uses that service without the
  // once-a-window cards: it runs unattended. New keys still ask; payments beyond the allowance still ask.
  // By the app's id when the window has it: by page the arcade looks the app up through the whole App Store, which took
  // minutes on a busy arcade, and the page's first svc call waited for it (2026-11-21).
  const who = () => (id ? { id } : { page: here });
  async function allowed(service) {
    // At most 5 s: no allowance known in time is no allowance (the usual cards ask).
    const ask = api("apps.allowance", { ...who(), service }).then((a) => a.per_day > 0).catch(() => false);
    return Promise.race([ask, new Promise((res) => setTimeout(() => res(false), 5000))]);
  }
  const doors = {
    async svc(m) {
      // An allowance asked about with no service names the AI models' (the one the arcade knows apps to buy from).
      const service = String(m.service || (m.op === "allowance" || m.op === "ask_allowance" ? "models" : ""));
      if (!/^[a-z0-9-]{1,32}$/.test(service)) throw new Error("service: 1-32 of a-z, 0-9 and -");
      if (m.op === "offers") return { offers: await api("svc.offers", { service }) };
      if (m.op === "ask") {
        const to = String(m.to || "");
        if (!/^[0-9a-f]{64}$/.test(to)) throw new Error("to: an arcade's node id");
        if (!svcAskYes[service] && !(await allowed(service))) {
          const yes = await oneCard(() => card(`Let ${appName} use ${service} at other arcades?`, [
            `${appName} asks to send requests to ${service} services on the mesh in your name (your ARCADE address), so a seller can charge them to the credit you bought there.`,
            "It can't move your coins or buy credit without asking."], "Allow"));
          if (!yes) throw new Error("Cancelled.");
          svcAskYes[service] = true;
        }
        const r = await api("svc.ask", { to, service, body: m.body || {}, timeout: Math.min(1200, Math.max(1, Number(m.timeout) || 600)) });
        return { last: r.last, answers: r.answers };
      }
      if (m.op === "local") {
        const body = m.body && typeof m.body === "object" ? m.body : {};
        const what = String(body.what || "");
        // A helper that isn't attached here is refused at once, before any card (2026-11-22: a page asking for one
        // that only its author runs showed "manage your <name> helper?" on every arcade, and its other asks hit "busy").
        const attached = await api("svc.services", {}).then((l) => (l || []).some((x) => x.service === service)).catch(() => true);
        if (!attached) throw new Error(`no ${service} helper on this arcade`);
        // A payment within the app's allowance for this service needs neither card (counted at once; given back if it
        // fails below).
        let reserved = 0;
        if (what === "buy" && Number(body.amount) > 0) {
          try {
            const u = await api("apps.allowance_use", { ...who(), service, amount: Number(body.amount), note: `credit from ${String(body.seller || "").slice(0, 16)}` });
            if (u.within === true) reserved = Number(body.amount);
          } catch (_) { /* no allowance: the cards below */ }
        }
        if (!svcLocalYes[service] && !reserved && !(what !== "key_new" && await allowed(service))) {
          const yes = await oneCard(() => card(`Let ${appName} manage your ${service} helper?`, [
            `${appName} asks to read and change the settings of the ${service} helper running on this computer, and to use it as you.`,
            "Each payment and each new key still asks on its own card."], "Allow"));
          if (!yes) throw new Error("Cancelled.");
          svcLocalYes[service] = true;
        }
        if (what === "buy" && !reserved) {
          const amount = Number(body.amount);
          if (!(amount > 0)) throw new Error("amount: ARCADE to pay");
          {
            const yes = await oneCard(() => card(`Pay ${amount} ARCADE?`, [
              `${appName} asks to buy ${amount} ARCADE of ${service} credit from ${String(body.seller || "").slice(0, 16)}, paid from your ARCADE wallet.`,
              "The seller credits it once the payment is in a block."], "Pay"));
            if (!yes) throw new Error("Cancelled.");
          }
        }
        if (what === "key_new") {
          const yes = await oneCard(() => card("Make a key for programs on this computer?", [
            `${appName} asks to make a key that lets programs on this computer use ${service} with your credit.`], "Make key"));
          if (!yes) throw new Error("Cancelled.");
        }
        let r;
        try { r = await api("svc.ask", { to: "self", service, body, timeout: 1200 }); }
        catch (e) {
          if (reserved) await api("apps.allowance_use", { ...who(), service, amount: reserved, undo: true }).catch(() => {});
          throw e;
        }
        if (reserved && r && r.last && r.last.error) await api("apps.allowance_use", { ...who(), service, amount: reserved, undo: true }).catch(() => {});
        return { result: r.last };
      }
      // The allowance this app has for a service, and asking the person for one (2026-11-21).
      if (m.op === "allowance") return { allowance: await api("apps.allowance", { ...who(), service }) };
      if (m.op === "ask_allowance") {
        const per = Number(m.per_day);
        if (!(per > 0)) throw new Error("per_day: ARCADE a day, above zero");
        const what = service === "models" ? "AI models" : `the ${service} service`;
        const reason = String(m.reason || "").slice(0, 200);
        const yes = await oneCard(() => card(`Give ${appName} an allowance?`, [
          `${appName} asks to spend up to ${per} ARCADE a day on ${what}${reason ? ` (${reason})` : ""}.`,
          "Payments within it go without asking; anything beyond it asks on a card. You can change or take it away in the app's settings."], "Allow"));
        if (!yes) throw new Error("Cancelled.");
        const cur = await api("apps.allowance", { ...who(), service });
        await api("apps.perms", { id: cur.id, set: { allowances: { [service]: per } } });
        return { allowance: await api("apps.allowance", { ...who(), service }) };
      }
      throw new Error("svc ops are offers, ask, local, allowance and ask_allowance");
    },
    // arcade.llm (the original API, APP_API.md B4) over the Models app's service (docs/SVC.md): offers straight from the
    // mesh; chats through this computer's arcade-models helper, paid from this wallet's credit at the seller.
    async llm(m) {
      if (m.op === "offers") {
        const list = await api("svc.offers", { service: "models" });
        const chat = list.filter((o) => o.offer && Array.isArray(o.offer.models))
          .map((o) => ({ ...o, models: o.offer.models.filter((x) => x.kind !== "speech") })).filter((o) => o.models.length);
        const coins = (u) => Number(u || 0) / 1e8;
        return { offers: chat.map((o) => ({
          seller: o.arcade, here: o.here, name: o.name || "", tag: o.tag || "", max_out: o.offer.max_out,
          free: o.models.every((x) => !x.in && !x.out), models: o.models.map((x) => ({ id: x.id })),
          prices: o.models.map((x) => ({ leg: "arctest", label: "ARCADE per million tokens", model: x.id, in: coins(x.in), out: coins(x.out) })),
          credit: null,
        })) };
      }
      if (m.op === "chat") {
        const req = m.req || {};
        if (!llmYes && !(await allowed("models"))) {
          const yes = await oneCard(() => card(`Let ${appName} use your model credit?`, [
            `${appName} asks to send prompts to shared AI models, paid from your ARCADE credit at the sellers (free models cost nothing).`,
            "It sees the replies; it can't buy more credit or move your coins."], "Allow"));
          if (!yes) throw new Error("Cancelled.");
          llmYes = true;
        }
        const body = { what: "chat_at", messages: req.messages };
        if (req.seller) body.seller = String(req.seller); else body.model = req.model || "";
        if (req.seller && req.model) body.model = req.model;
        for (const k of ["max_tokens", "max_completion_tokens", "temperature", "top_p", "stop", "tools", "tool_choice", "response_format", "seed"]) if (req[k] !== undefined) body[k] = req[k];
        let r;
        try { r = (await api("svc.ask", { to: "self", service: "models", body, timeout: 600 })).last; }
        catch (e) {
          if (/no models service/.test(String(e && e.message || e))) throw new Error("shared models need the Models app's helper (arcade-models) running on this computer");
          throw e;
        }
        return { content: r.content, reasoning: r.reasoning, usage: r.usage, cost: r.cost, left: r.left, seller: r.seller, model: r.model, leg: "arctest" };
      }
      if (m.op === "image") throw new Error("pictures aren't shared on this arcade yet");
      throw new Error("llm ops are offers, chat and image");
    },
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
    // arcade.files (/r/files.js): save a file to Downloads. No switch on the card; asks once per window.
    async files(m) {
      if (m.op === "save") return await saveFile(m, true);
      throw new Error("files ops are save");
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
        case "save": return await saveFile(m);
        default: throw new Error("groups ops are list, read, send, leave, seal, open, people, add, create and save");
      }
    },
    // The public feed (arcade.feed): reads for any app; posts, acts, follows and mutes only with its Feed switch on
    // (they pay fees from this wallet, in this operator's name: the Feed app has it on from the start).
    async feed(m) {
      const reads = ["read", "channels", "file", "chains", "me", "follows"];
      if (!reads.includes(m.op) && !(m.op === "settings" && !m.set)) {
        const p = await perms();
        if (!p.feed) throw new Error("off: turn on Feed on this app's card");
        if (Date.now() - lastSend < 400) throw new Error("busy: wait a moment");
        lastSend = Date.now();
      }
      const chain = (c) => (typeof c === "string" && /^[a-z0-9]{2,12}$/.test(c) ? c : undefined);
      switch (m.op) {
        case "read": {
          const v = m.view || {};
          const order = ["popular", "new", "newest", "friends"].includes(v.order) ? v.order : "popular";
          return { result: await api("feed", { order, channel: String(v.channel || ""), tag: v.tag ? String(v.tag).replace(/^@/, "") : undefined,
            limit: Math.max(1, Math.min(100, Number(v.limit) || 30)), offset: Math.max(0, Number(v.offset) || 0) }) };
        }
        case "channels": return { result: await api("feed.channels") };
        case "settings": return { result: m.set ? await api("feed.settings", { set: { hide_muted: !!m.set.hide_muted, contacts_only: !!m.set.contacts_only } }) : await api("feed.settings") };
        case "file": return { result: await api("feed.file", { txid: String(m.txid || "") }) };
        case "chains": return { result: await api("ledger.write_chains") };
        case "me": { const r = await api("start.status").catch(() => ({})); return { result: { address: r.address || "", tag: r.tag || "" } }; }
        case "follows": return { result: await api("follows", { who: String(m.who || "") || undefined }) };
        case "post": {
          const p = m.post || {};
          const text = String(p.text || "");
          const f = p.file && typeof p.file === "object" ? { name: String(p.file.name || "file").slice(0, 120), type: String(p.file.type || "application/octet-stream").slice(0, 100), data: String(p.file.data || "") } : undefined;
          if (!text.trim() && !f) throw new Error("nothing to post");
          if (text.length > 4000) throw new Error("too long: at most 4000 characters");
          return { result: await api("feed.post", { text, channel: String(p.channel || "").slice(0, 40), file: f, chain: chain(p.chain) }) };
        }
        case "act": {
          const kinds = ["like", "unlike", "dislike", "undislike", "reply", "share", "tip", "edit", "delete"];
          if (!kinds.includes(m.kind)) throw new Error("kind: " + kinds.join(", "));
          if (!/^[0-9a-f]{64}$/.test(String(m.target || ""))) throw new Error("target: a post's txid");
          const amount = Math.max(0, Math.floor(Number(m.amount) || 0));
          if (m.kind === "tip") {
            // A tip moves coins: shown on its own card first, outside the app's reach.
            const yes = await oneCard(() => card(`Tip from ${appName}?`, [`Send a tip of ${(amount / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, "")} to the post's author?`], "Send tip"));
            if (!yes) throw new Error("declined");
          }
          return { result: await api("feed.act", { kind: m.kind, target: m.target, text: String(m.text || "").slice(0, 4000), amount, chain: chain(m.chain) }) };
        }
        case "follow": return { result: await api("follow", { who: String(m.who || ""), on: m.on !== false }) };
        case "mute": return { result: await api(m.on === false ? "mute.remove" : "mute.add", { address: String(m.address || "") }) };
        default: throw new Error("feed ops are read, channels, settings, file, chains, me, follows, post, act, follow and mute");
      }
    },
    async social(m) {
      // A:// reacts, replies and tips on any chain's transaction (docs/APROTO.md): reads open, writes behind the Feed switch.
      if (!["reactions", "replies"].includes(m.op)) {
        const p = await perms();
        if (!p.feed) throw new Error("off: turn on Feed on this app's card");
        if (Date.now() - lastSend < 400) throw new Error("busy: wait a moment");
        lastSend = Date.now();
      }
      const target = (t) => {
        t = t || {};
        const chain = String(t.chain || "").toLowerCase(), txid = String(t.txid || "").toLowerCase();
        if (!/^[a-z0-9]{1,12}$/.test(chain)) throw new Error("target.chain: a chain key");
        if (!/^[0-9a-f]{64}$/.test(txid)) throw new Error("target.txid: 64 hex");
        return { chain, txid };
      };
      switch (m.op) {
        case "reactions": {
          const ts = Array.isArray(m.targets) ? m.targets.slice(0, 100).map(target) : [];
          if (!ts.length) throw new Error("targets: 1 to 100 {chain, txid}");
          return { result: await api("a.reactions", { targets: ts, me: true }) };
        }
        case "replies": {
          const t = target(m.target);
          return { result: await api("a.replies", { ...t, offset: Math.max(0, Number(m.offset) || 0), limit: Math.max(1, Math.min(200, Number(m.limit) || 50)) }) };
        }
        case "react": {
          if (!["like", "unlike", "dislike", "undislike"].includes(m.kind)) throw new Error("kind: like, unlike, dislike or undislike");
          return { result: await api("a.react", { ...target(m.target), kind: m.kind }) };
        }
        case "reply": {
          const text = String(m.text || "").trim();
          if (!text || new TextEncoder().encode(text).length > 500) throw new Error("a reply is 1 to 500 bytes");
          return { result: await api("a.reply", { ...target(m.target), text }) };
        }
        case "tip": {
          const t = target(m.target);
          const amount = Math.floor(Number(m.amount) || 0);
          if (amount <= 0) throw new Error("amount: sats of ARCADE");
          const to = String(m.to || "");
          const coins = (amount / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, "");
          // A tip moves coins: shown on its own card first, outside the app's reach.
          const yes = await oneCard(() => card(`Tip from ${appName}?`, [`Send a tip of ${coins} ARCADE ${to ? "to " + to : "to the author"}?`], "Send tip"));
          if (!yes) throw new Error("declined");
          return { result: await api("a.tip", { ...t, amount, to: to || undefined, memo: String(m.memo || "").slice(0, 100) }) };
        }
        case "follow": return { result: await api(m.on === false ? "a.unfollow" : "a.follow", { tag: String(m.tag || "").replace(/^@/, "") }) };
        default: throw new Error("social ops are reactions, replies, react, reply, tip and follow");
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
  // arcade.folder (2026-11-21): the app asks for one folder on this computer to read and write inside. The person picks
  // it on a card with a folder browser (folder.browse: subfolders only, inside the home folder); the arcade keeps the
  // grant by the app's id (apps.folder_set) and the page then uses /r/folder/*. The page never learns the path.
  doors.folder = async (m) => {
    if (m.op !== "ask") throw new Error("folder ops here: ask");
    if (!id) throw new Error("only an app opened from Apps can be given a folder");
    const why = String(m.why || "").slice(0, 200);
    const path = await oneCard(() => pickFolder(why));
    if (!path) throw new Error("Cancelled.");
    const r = await api("apps.folder_set", { id, path });
    toast(`${appName} can now use the folder “${r.name}”`);
    return { granted: !!r.granted, name: r.name };
  };
  // The folder card: a breadcrumb, the subfolders of where it is, "Use this folder" and Cancel. Resolves the chosen
  // folder's path, or null.
  function pickFolder(why) {
    return new Promise((resolve) => {
      const ov = document.createElement("div"); ov.className = "ov";
      const box = document.createElement("div"); box.className = "box";
      const h = document.createElement("h3"); h.textContent = `${appName} asks for a folder to work in`;
      const p = document.createElement("p"); p.textContent = why || "It gives no reason.";
      const note = document.createElement("p"); note.className = "caution grey";
      note.textContent = "It can read, change and remove files in the folder you pick, and nowhere else. You can take it away in Apps, Details.";
      const crumbs = document.createElement("div"); crumbs.style.cssText = "display:flex;flex-wrap:wrap;gap:2px;margin:8px 0 4px;font-size:13px";
      const list = document.createElement("div");
      list.style.cssText = "max-height:240px;overflow:auto;border:1px solid #ddd;border-radius:8px;padding:4px";
      const err = document.createElement("p"); err.style.color = "#b3261e";
      const row = document.createElement("div"); row.className = "btns";
      const no = document.createElement("button"); no.textContent = "Cancel";
      const yes = document.createElement("button"); yes.textContent = "Use this folder"; yes.className = "yes";
      let at = null;
      const done = (v) => { ov.remove(); resolve(v); };
      no.onclick = () => done(null);
      yes.onclick = () => { if (at && !at.is_root) done(at.path); };
      const link = (text, go) => {
        const b = document.createElement("button");
        b.textContent = text;
        b.style.cssText = "font:inherit;border:0;background:none;padding:2px 4px;cursor:pointer;color:#2e6b3a;text-align:left";
        b.onclick = go;
        return b;
      };
      async function go(path) {
        err.textContent = "";
        let b;
        try { b = await api("folder.browse", path ? { path } : {}); }
        catch (e) { err.textContent = String(e && e.message || e); return; }
        at = b;
        const parts = [];
        if ((b.roots || []).length > 1) for (const r of b.roots) parts.push(link(`[${r.name}]`, () => go(r.path)));
        (b.crumbs || []).forEach((c, i) => {
          if (i) { const s = document.createElement("span"); s.textContent = "›"; parts.push(s); }
          parts.push(link(c.name, () => go(c.path)));
        });
        crumbs.replaceChildren(...parts);
        const rows = (b.dirs || []).map((d) => {
          const x = link(`📁 ${d}`, () => go(b.path.replace(/[\\/]+$/, "") + (b.path.includes("\\") ? "\\" : "/") + d));
          x.style.display = "block"; x.style.width = "100%";
          return x;
        });
        if (!rows.length) { const e = document.createElement("div"); e.style.cssText = "color:#777;padding:4px"; e.textContent = "No folders in here."; rows.push(e); }
        list.replaceChildren(...rows);
        yes.disabled = !!b.is_root;
        yes.title = b.is_root ? "Pick a folder inside your home folder" : "";
      }
      row.append(no, yes);
      box.append(h, p, note, crumbs, list, err, row); ov.append(box); document.body.append(ov);
      go("");
    });
  }
  // arcade.apps (2026-11-22): the page lists the arcade's apps itself (GET /r/apps) and asks to open one here. The arcade
  // checks the app (one the App Store lists, not this one) and gives what its window needs; the person says yes on a card;
  // then it opens in its own window, as Apps opens it. The page never sees the other app's key.
  doors.apps = async (m) => {
    if (m.op !== "open") throw new Error("apps ops here: open (the list is GET /r/apps)");
    const other = String(m.id || "").trim();
    if (!other) throw new Error("which app? give its id (from arcade.apps.list)");
    if (id && other === id) throw new Error("an app can't open itself: it is already open");
    const l = await api("apps.open", { id: other, from: id, page: here });
    const lines = [`${appName} wants to open ${l.title} in its own window.`];
    if (l.caution) lines.push(String(l.caution));
    if (l.withdrawn) lines.push(`Its publisher withdrew ${l.title}${l.withdrawn.reason ? ": " + l.withdrawn.reason : ""}.`);
    const yes = await oneCard(() => card(`${appName} wants to open ${l.title}`, lines, "Allow"));
    if (!yes) throw new Error("declined");
    await invoke("open_app", { id: l.id, title: l.title, frame: l.frame });
    return { opened: l.id, title: l.title };
  };
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
  //    (the two halves of a direct link), 'h' (please offer), 'n' (I don't link with you) and 'p'
  //    (no WebRTC here: my pictures and sound go as the arcades' media pieces, realtime.media).
  // This window makes no WebRTC links (it has none). A pair whose arcades are both V1.0 goes over the
  // arcades' own fast lane instead (realtime.fast / realtime.lanes): state 'direct'. Every other pair
  // is carried by the room: at most three hidden messages a second, signals first, then the reliable
  // sends in order, and of the rest only the latest. An original arcade that offers a WebRTC link gets
  // no answer and carries the pair over the room after its own wait. A website's viewer that knows the
  // pieces (the room door with fast) is told 'p' instead: it stops waiting for a WebRTC link, takes the
  // arcades' fast lane for this pair, and sends and plays the same pieces as this window.
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
      if (r.rtc) rtcLook(name, s, r.rtc);
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
    for (const id in s.fast.peers) { clearTimeout(s.fast.peers[id].timer); if (s.fast.peers[id].rtc) rtcClose(s, id); }
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
    // A viewer with a higher id waits for this side's WebRTC offer: the arcade makes it (rtcOffer);
    // an arcade that can't says 'p' instead (pieces).
    if (s.me.id < p.id) rtcOffer(name, s, peer);
    // No lane by then: the room carries the pair (and it goes 'direct' if a lane comes up later).
    peer.timer = setTimeout(() => {
      if (s.fast !== fast || fast.peers[p.id] !== peer || peer.state === "direct") return;
      fastSay(name, s, peer.p, "room");
      fastPump(name, s);
    }, FAST_LANE_WAIT);
  }
  // 'p' to a player, at most once a second (a viewer asks again after each rejoin).
  function piecesHint(name, s, peer) {
    const now = Date.now();
    if (now - (peer.hinted || 0) < 1000) return;
    peer.hinted = now;
    fastSignal(name, s, peer.p.id, { t: "p" });
  }
  function fastDrop(s, id) {
    const peer = s.fast && s.fast.peers[id];
    if (!peer) return;
    media.gone(s, id);
    clearTimeout(peer.timer);
    if (peer.rtc) rtcClose(s, id);
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
      // They want a direct link. One the page does not link with is told so; the rest get one made
      // by the arcade (rtcSignal), or 'p' where it can't be made.
      if (!s.fast.peers[p.id]) fastMeet(name, s, p);
      if (!s.fast.peers[p.id] && !fastLinks(s, p.id)) fastSignal(name, s, p.id, { t: "n" });
      else if (s.fast.peers[p.id]) rtcSignal(name, s, s.fast.peers[p.id], o);
    } else if ((o.t === "a" || o.t === "p") && s.fast.peers[p.id]) {
      rtcSignal(name, s, s.fast.peers[p.id], o);
    }
  }
  // WEBRTC MADE BY THE ARCADE (2026-11-19, Martin: "Both should use WebRTC and the UDP punching so that
  // they can connect"; arcaded rtcsvc.rs). This window has no RTCPeerConnection, so its arcade is the
  // WebRTC peer for it: the viewer's compact offer goes to the arcade (realtime.rtc signal) and its
  // answer back over the room, or the arcade offers when this side's id is the lower. Once the link is
  // up (realtime.lanes: rtc), media.js's pieces and the fast sends for that viewer go over it as RTP and
  // data channels, and what the viewer sends comes back as the same pieces and fast events. A link not
  // up within RTC_WAIT, or failed, gives way to the arcades' lane: the viewer is told 'p'. Another V1.0
  // window (its offer says v1) keeps the lane between the arcades, as before.
  const RTC_WAIT = 9000, RTC_FRESH = 8000;
  async function rtcAsk(name, s, peer, ask) {
    const fast = s.fast;
    peer.rtc = { at: Date.now(), up: false };
    const mine = peer.rtc;
    let r;
    try { r = await api("realtime.rtc", { token: s.token, peer: peer.p.id, ...ask }); }
    catch (err) { if (peer.rtc === mine) { peer.rtc = null; piecesHint(name, s, peer); } return; }   // an arcade without it: pieces
    if (s.fast !== fast || fast.peers[peer.p.id] !== peer || peer.rtc !== mine) return;
    if (r && r.signal) fastSignal(name, s, peer.p.id, r.signal);
  }
  function rtcOffer(name, s, peer) { return rtcAsk(name, s, peer, { op: "offer" }); }
  function rtcSignal(name, s, peer, o) {
    if (o.t === "p") { if (peer.rtc) { peer.rtc = null; rtcClose(s, peer.p.id); } return; }   // a V1.0 window: the lane
    if (o.t === "a") {
      if (!peer.rtc) return;
      api("realtime.rtc", { token: s.token, peer: peer.p.id, op: "signal", signal: o }).catch(() => {});
      return;
    }
    if (o.t === "o") {
      if (o.v1) { piecesHint(name, s, peer); return; }
      rtcAsk(name, s, peer, { op: "signal", signal: o });
      return;
    }
    // 'h': they wait for our offer. A new page of theirs (s) gets a new link; the one being made stays.
    if (s.me.id >= peer.p.id) { piecesHint(name, s, peer); return; }
    const fresh = peer.rtc && Date.now() - peer.rtc.at < RTC_FRESH && (!peer.hs || !o.s || peer.hs === o.s);
    if (o.s) peer.hs = o.s;
    if (!fresh && !(peer.rtc && peer.rtc.up && (!o.s || peer.hs === o.s))) rtcOffer(name, s, peer);
  }
  function rtcClose(s, id) { api("realtime.rtc", { token: s.token, peer: id, op: "close" }).catch(() => {}); }
  // Each look at the lanes: a link that is up stays; one that failed, or is not up in time, gives way.
  function rtcLook(name, s, links) {
    for (const id in s.fast.peers) {
      const peer = s.fast.peers[id], st = links[id];
      if (!peer.rtc) continue;
      if (st === "up") { peer.rtc.up = true; continue; }
      const late = !peer.rtc.up && Date.now() - peer.rtc.at > RTC_WAIT;
      if (st === "failed" || late || (!st && Date.now() - peer.rtc.at > 3000)) {
        peer.rtc = null;
        rtcClose(s, id);
        piecesHint(name, s, peer);
      }
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

  // In the desktop app the first wallet.approve is already signed (that program told the signer).
  // In a browser the signer stays locked until its own window is approved, so keep asking.
  async function approveSeen(body) {
    const first = await api("wallet.approve", body);
    if (!first || first.status !== "confirm") return first;
    if (IN_BROWSER && first.confirm_url) {
      const w = window.open(first.confirm_url, "arcade-signer", "popup,width=520,height=720");
      if (!w) {
        const go = await card("Confirm this send in the signer window", ["Pop-ups are blocked. Open this address and approve the send there, then press Check.", first.confirm_url], "Check", "Cancel");
        if (!go) { await api("wallet.deny", { id: body.id }); return first; }
      }
    }
    const until = Date.now() + 5 * 60 * 1000;
    while (Date.now() < until) {
      const r = await api("wallet.approve", body);
      if (!r || r.status !== "confirm") return r;
      await new Promise((res) => setTimeout(res, 1000));
    }
    throw new Error("the send was not confirmed in the signer window");
  }

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
      if (pv.kind === "fog") {
        // A storage choice (fogkeep.rs): keep, pin, forget or undo on this arcade's own storage, or a setting (the keep
        // share, the blob cache: the Storage app's Settings). Nothing is signed or written on a chain; it changes only
        // what this arcade keeps and serves.
        const c = (pv.fog && pv.fog.card) || {};
        const size = (b) => b >= 10485760 ? `${Math.round(b / 1048576)} MB` : b >= 1048576 ? `${(b / 1048576).toFixed(1)} MB` : `${Math.ceil((b || 0) / 1000)} kB`;
        const lines = [`${from} asks to change what this arcade keeps:`];
        for (const l of c.lines || []) lines.push(l);
        const facts = document.createElement("ul"); facts.className = "outs";
        const fact = (t) => { const li = document.createElement("li"); li.textContent = t; facts.append(li); };
        if (c.size_bytes) fact(`Size: ${size(c.size_bytes)}`);
        if (c.copies_before != null && c.floor != null) fact(`Copies: ${c.copies_before}/${c.floor} → ${c.copies_after}/${c.floor}`);
        for (const h of c.handover || []) fact(`${h.segment}: handing over: kept here until a new keeper is registered and audited (${h.before}/${h.floor} now)`);
        for (const t of c.facts || []) fact(t);
        if (c.pay_text) fact(`Pay: ${c.pay_text}`);
        if (c.seal_text) fact(`Seal: ${c.seal_text}`);
        if (facts.childElementCount) lines.push(facts);
        for (const n of c.notes || []) lines.push(n);
        const title = c.op === "forget" ? "Forget this here?" : c.op === "undo" ? "Undo this choice?" : c.op === "keep_share" ? "Change the keep share?"
          : c.op === "cache" ? "Change the blob cache?" : c.mode === "pin" ? "Pin this blob here?" : "Keep this here?";
        const yes = await card(title, lines, "Approve", "Refuse");
        await api(yes ? "wallet.approve" : "wallet.deny", { id: r.id });
        return;
      }
      if (pv.kind === "repo") {
        // A repository write (reposvc.rs): signed by this wallet's @tag on the ARCADE chain.
        const arc = (u) => `${(u / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 })} ARCADE`;
        const lines = [`${from} asks to ${pv.what}.`, "It is signed by your @tag on the ARCADE chain and public there for good.", `Network fee: about ${arc(pv.fee)}`];
        const yes = await card("Approve this repository change?", lines, "Approve", "Refuse");
        await api(yes ? "wallet.approve" : "wallet.deny", { id: r.id });
        return;
      }
      const outs = document.createElement("ul"); outs.className = "outs";
      for (const o of pv.outputs) { const li = document.createElement("li"); li.textContent = o.data ? "data (the arcade record)" : `${pep(o.value)} → ${o.address}${o.mine ? " (back to you)" : ""}`; outs.append(li); }
      const lines = [`${from} asks to send ${pv.what} to ${pv.to_tag ? "@" + pv.to_tag + " " : ""}${pv.to}.`];
      // The app's card is one arcade's word (a chain whose ledger isn't here): said above Approve, not hidden.
      if (who.caution) { const c = document.createElement("span"); c.className = "caution"; c.textContent = "⚠ " + who.caution; lines.push(c); }
      if (pv.note) lines.push(`“${pv.note}”`);
      lines.push(outs, `Network fee: ${pv.fee_unit ? coinNum(pv.fee) + " " + pv.fee_unit : pep(pv.fee)}`);
      // One arcade alone (or none) vouched that its coins carry no inscription: said above Approve.
      if (pv.caution && pv.caution.note && pv.caution.level !== "green") {
        const c = document.createElement("div"); c.className = "caution " + pv.caution.level; c.setAttribute("role", "alert"); c.textContent = pv.caution.note;
        lines.push(c);
      }
        const yes = await card("Approve this send?", lines, "Approve and send", "Refuse");
        // kind, to, units, fee and the outputs are the card the user just saw. The desktop
        // approves that exact transaction on the signer; a different one is refused. In a
        // browser the signer window has to approve it (the service cannot), so wait for that.
        if (!yes) { await api("wallet.deny", { id: r.id }); return; }
        await approveSeen({ id: r.id, kind: pv.kind, to: pv.to, units: pv.units, fee: pv.fee, outputs: pv.outputs });
    } catch (_) { /* answered elsewhere */ }
    finally { showing = null; }
  }, 1500);

  // The page with its pass for the page API (asked for with its key; "?pass=" also makes a new address, so a copy
  // cached before passes isn't used), and the place
  // to open it at when the window was asked for one (`at`: a plain #/ route, as Arcade Git's #/@owner/name).
  const at = q.get("at") || "";
  el.src = frame + "?pass=" + (passKey || "1") + (/^#\/[A-Za-z0-9@._\/-]{0,200}$/.test(at) ? at : "");
  window.arcadeApp = { id, frame, el };
})();
