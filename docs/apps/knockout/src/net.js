// net.js - ONLINE: play together through the arcade's rooms and fast lane (arcade.realtime, {fast: true}), browser to
// browser with no game server, like CUL-DE-SAC.
//   - The HOST (the player with the lowest id) runs the match. Everyone else sends their buttons and draws the host's
//     snapshots. Anyone becomes host when the last one leaves.
//   - Rooms "knockout-1".."knockout-9", up to 4 players each (a full room sends you on to the next).
// Messages (JSON arrays):
//   lobby  ['p', who, ready]                                  my pick (reliable on change, plain every 2 s)
//          ['L', {stage, stocks, cpus, phase, mid, start}]    the host's lobby (reliable on change, plain every second)
//          ['G', {mid, stage, stocks, roster}]                the host starts a match: roster [[id | 'cpu', who, level?]]
//   match  ['i', mid, seq, held, c0..c7]                      a player's buttons, 30 a second: held bits + press counters
//          ['S', mid, frame, countdown, end, fighters, projectiles, events]   the host's snapshot, 30 a second
//          ['q', mid]  a player left the match (host: a CPU takes over)    ['X', mid]  the host ended the match
'use strict';
const NET_ROOMS = 9, NET_MAX = 4;
const NET_STATES = ['idle', 'run', 'crouch', 'squat', 'air', 'attack', 'special', 'helpless', 'land', 'land0', 'shield', 'roll', 'dizzy', 'hurt', 'tumble', 'ledge', 'respawn'];
const NET_ANIMS = ['idle', 'run', 'jump', 'fall', 'crouch', 'jab', 'smash', 'upair', 'kick', 'special', 'upspecial', 'shield', 'hurt', 'tumble', 'splat', 'win'];
const NET_PROJ = ['coin', 'token', 'wave'];
const NET_EDGES = ['jumpP', 'attackP', 'specialP', 'shieldP', 'upP', 'downP', 'leftP', 'rightP'];
const NET_WORDS = ['BONK!', 'SPLAT!', 'THUD!', 'OOF!', 'WOW. GLASS.', 'NOT THE SCREEN!'];
const NET = {
  room: null, me: null, online: false, joining: false, why: '', roomNo: 0,
  picks: {}, lobby: { stage: 'arcade', stocks: 3, cpus: 0, phase: 'lobby', mid: 0, start: 0 },
  inputs: {}, consumed: {}, counters: [0, 0, 0, 0, 0, 0, 0, 0], seq: 0, snap: null, evq: [], evId: 0, evSeen: new Set(), match: null, note: '', noteT: 0,
  rt() { return window.arcade && window.arcade.realtime; },
  people() {
    if (!this.room) return [];
    const now = performance.now();   // a player silent for 8 s is gone (a closed window can take the arcade longer to notice)
    const out = this.room.members().filter((p) => (this.me && p.id === this.me.id) || now - (this.seen[p.id] ?? now) < 8000);
    if (this.me && !out.some((p) => p.id === this.me.id)) out.push(this.me);
    return out.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)).slice(0, 8);
  },
  nameOf(p) { return !p ? '?' : p.tag ? '@' + p.tag : p.guest ? 'GUEST ' + String(p.id).slice(-4) : String(p.id).slice(0, 8); },
  hostId() { const P = this.people(); return P.length ? P[0].id : this.me && this.me.id; },
  isHost() { return !!this.me && this.hostId() === this.me.id; },
  say(s, t = 180) { this.note = s; this.noteT = t; },
  send(arr, reliable) { if (!this.room || !this.online) return; try { if (this.room.fast) this.room.fast.send(arr, reliable ? { reliable: true } : undefined); else if (reliable) this.room.send(arr).catch(() => {}); } catch (e) {} },
  link(id) { const P = this.room && this.room.fast ? this.room.fast.peers() : {}; return P[id] || ''; },
  async join() {
    const rt = this.rt();
    if (!rt) { this.why = 'noarcade'; return false; }
    this.joining = true; this.why = '';
    for (let n = 1; n <= NET_ROOMS; n++) {
      let r;
      try { r = await rt.join('knockout-' + n, { fast: true }); } catch (e) { r = { online: false, why: String(e && e.message || e) }; }
      if (!r || !r.online) { this.joining = false; this.why = (r && r.why) || 'offline'; return false; }
      const count = r.members().filter((p) => !r.me || p.id !== r.me.id).length + 1;
      if (count > NET_MAX && n < NET_ROOMS) { try { r.leave(); } catch (e) {} continue; }
      this.attach(r, n); this.joining = false; return true;
    }
    this.joining = false; this.why = 'every room is full'; return false;
  },
  attach(r, n) {
    Object.assign(this, { room: r, me: r.me, online: true, roomNo: n, picks: {}, inputs: {}, consumed: {}, snap: null, match: null, evSeen: new Set(), seen: {}, gone: {} });
    for (const p of r.members()) this.seen[p.id] = performance.now();
    this.lobby = { stage: 'arcade', stocks: 3, cpus: 0, phase: 'lobby', mid: 0, start: 0 };
    const hear = (d, p) => this.hear(d, p);
    r.on('message', hear);
    if (r.fast) r.fast.on('message', hear);
    r.on('join', (p) => { this.seen[p.id] = performance.now(); delete this.gone[p.id]; this.lobbyDirty = true; this.pickDirty = true; });
    r.on('leave', (p) => this.left(p));
    r.on('rejoined', (me) => { this.me = me || this.me; this.online = true; this.pickDirty = true; });
    r.on('reconnecting', () => { this.online = false; this.say('RECONNECTING...'); });
    r.on('closed', () => { this.online = false; this.say('THE ROOM CLOSED'); });
    this.pickDirty = true;
  },
  leave() { if (this.room) { try { this.room.leave(); } catch (e) {} } this.room = null; this.online = false; this.match = null; },
  left(p) {
    delete this.picks[p.id]; delete this.inputs[p.id];
    const M = this.match;
    if (!M) return;
    if (M.host && scene === Fight) { const f = F.find((g) => g.netId === p.id); if (f) { f.ctrl = 'cpu'; f.level = 5; f.tag = 'CPU'; this.say(this.nameOf(p) + ' LEFT: A CPU TAKES OVER'); } }
    else if (!M.host && p.id === M.hostId && !M.done) { this.match = null; this.say('THE HOST LEFT. BACK TO THE LOBBY.'); if (scene === Fight || scene === Results) go(Online); }
  },
  hear(d, p) {
    if (!Array.isArray(d) || !p || (this.me && p.id === this.me.id)) return;
    this.seen[p.id] = performance.now(); delete this.gone[p.id];
    const fromHost = p.id === this.hostId();
    switch (d[0]) {
      case 'p': this.picks[p.id] = { who: ROSTER.includes(d[1]) ? d[1] : 'buster', ready: !!d[2] }; break;
      case 'L': if (fromHost && d[1] && typeof d[1] === 'object') this.lobby = { ...this.lobby, ...d[1] }; break;
      case 'G': if (fromHost && d[1]) this.startClient(d[1], p.id); break;
      case 'i': { const M = this.match; if (!M || !M.host || d[1] !== M.mid) break; const I = this.inputs[p.id];
        if (!I || d[2] > I.seq) this.inputs[p.id] = { seq: d[2], held: d[3] | 0, c: d.slice(4, 12), at: frameNo }; break; }
      case 'S': { const M = this.match; if (!M || M.host || d[1] !== M.mid) break; this.snapAt = performance.now(); if (!this.snap || d[2] >= this.snap[2]) this.snap = d; break; }
      case 'q': { const M = this.match; if (M && M.host && d[1] === M.mid) { const f = F.find((g) => g.netId === p.id); if (f) { f.ctrl = 'cpu'; f.level = 5; f.tag = 'CPU'; } } break; }
      case 'X': { const M = this.match; if (M && !M.host && d[1] === M.mid) { this.match = null; this.say('THE HOST ENDED THE MATCH'); go(Online); } break; }
    }
  },
  // ---- lobby ------------------------------------------------------------------------------------------------------------
  myPick() { return this.picks[this.me && this.me.id] || (this.picks[this.me && this.me.id] = { who: 'buster', ready: false }); },
  // players who went silent count as left (once each), so the host changes and CPUs take over
  watch() {
    if (!this.room) return;
    const now = performance.now();
    for (const p of this.room.members()) if (this.me && p.id !== this.me.id && now - (this.seen[p.id] ?? now) > 8000 && !this.gone[p.id]) { this.gone[p.id] = 1; this.left(p); }
    const M = this.match;
    if (M && !M.host && scene === Fight && now - (this.snapAt || M.at || now) > 6000) { this.match = null; this.say('LOST THE HOST. BACK TO THE LOBBY.'); go(Online); }
  },
  lobbyTick() {
    if (!this.online || !this.me) return;
    const mine = this.myPick();
    if (this.pickDirty || sceneT % 120 === 0) { this.send(['p', mine.who, mine.ready ? 1 : 0], this.pickDirty); this.pickDirty = false; }
    if (!this.isHost()) return;
    const L = this.lobby, P = this.people().slice(0, NET_MAX);
    L.cpus = Math.min(L.cpus, NET_MAX - P.length);
    const allReady = P.every((p) => (this.picks[p.id] || {}).ready) && P.length + L.cpus >= 2;
    if (L.phase === 'lobby') {
      if (allReady && !L.start) { L.start = 180; this.lobbyDirty = true; }
      if (!allReady && L.start) { L.start = 0; this.lobbyDirty = true; }
      if (L.start && --L.start <= 0) { L.start = 0; this.startHost(P); return; }
    }
    if (this.lobbyDirty || sceneT % 60 === 0) { this.send(['L', { stage: L.stage, stocks: L.stocks, cpus: L.cpus, phase: L.phase, mid: L.mid, start: L.start }], this.lobbyDirty); this.lobbyDirty = false; }
  },
  startHost(P) {
    const L = this.lobby; L.mid = (L.mid || 0) + 1 + (Math.random() * 1000 | 0) * 0; L.phase = 'match';
    const roster = P.map((p) => [p.id, (this.picks[p.id] || {}).who || 'buster']);
    for (let i = 0; i < L.cpus; i++) roster.push(['cpu', pick(ROSTER), 6]);
    const G = { mid: Date.now() % 1e9, stage: L.stage, stocks: L.stocks, roster };
    L.mid = G.mid;
    this.send(['G', G], true);
    this.match = { host: true, mid: G.mid, hostId: this.me.id, roster };
    this.inputs = {}; this.consumed = {}; this.evq = []; this.lobbyDirty = true;
    for (const p of P) if (this.picks[p.id]) this.picks[p.id].ready = false;
    go(Fight, G.stage, null, { host: true, mid: G.mid, stocks: G.stocks, roster });
  },
  startClient(G, hostId) {
    if (!G.roster || !STAGES[G.stage]) return;
    this.lobby.phase = 'match'; this.lobby.mid = G.mid;
    const mine = this.myPick(); mine.ready = false; this.pickDirty = true;
    if (!G.roster.some((r) => r[0] === (this.me && this.me.id))) { this.say('A MATCH STARTED. YOU ARE IN THE NEXT ONE.'); return; }
    this.match = { host: false, mid: G.mid, hostId, roster: G.roster, endT: 0, at: performance.now() }; this.snapAt = 0;
    this.snap = null; this.counters = [0, 0, 0, 0, 0, 0, 0, 0]; this.seq = 0; this.evSeen = new Set();
    go(Fight, G.stage, null, { host: false, mid: G.mid, stocks: G.stocks, roster: G.roster });
  },
  // ---- the host's side of a match ---------------------------------------------------------------------------------------
  padFor(id) {
    const I = this.inputs[id], pad = { x: 0, y: 0 };
    if (!I || frameNo - I.at > 90) return pad;           // nothing heard for 1.5 s: let go of everything
    const b = I.held;
    pad.x = (b & 2 ? 1 : 0) - (b & 1 ? 1 : 0); pad.y = (b & 8 ? 1 : 0) - (b & 4 ? 1 : 0);
    pad.jump = !!(b & 16); pad.attack = !!(b & 32); pad.special = !!(b & 64); pad.shield = !!(b & 128);
    const C = this.consumed[id] || (this.consumed[id] = I.c.slice());
    NET_EDGES.forEach((k, i) => { if (I.c[i] !== C[i]) { pad[k] = true; C[i] = I.c[i]; } });
    return pad;
  },
  event(e) { if (this.match && this.match.host) { this.evq.push([++this.evId, frameNo, ...e]); } },
  sendSnap() {
    const M = MATCH;
    this.evq = this.evq.filter((e) => frameNo - e[1] < 16);
    const fs = F.map((f) => { const [anim, fi] = fighterAnim(f);
      const flags = (f.dead ? 1 : 0) | (f.out ? 2 : 0) | (f.inv > 0 && (frameNo >> 2) % 2 ? 4 : 0) | (f.hitlag > 0 ? 8 : 0) | (f.state === 'special' && f.who === 'pep3d' && f.spKey === 'neutral' && f.sp && f.sp.len > 0 ? 16 : 0);
      return [Math.round(f.x * 10), Math.round(f.y * 10), f.face, NET_STATES.indexOf(f.state), NET_ANIMS.indexOf(anim), fi | 0, Math.round(f.dmg), f.stocks, flags, Math.round(f.shieldHP), f.sp && f.sp.len ? Math.round(f.sp.len) : 0, f.kos, f.falls, f.outAt | 0, f.t | 0]; });
    const ps = PROJ.filter((p) => NET_PROJ.includes(p.type)).slice(0, 12).map((p) => [NET_PROJ.indexOf(p.type), Math.round(p.x), Math.round(p.y)]);
    this.send(['S', this.match.mid, frameNo, M.countdown, M.end, fs, ps, this.evq.map((e) => [e[0], ...e.slice(2)])]);
  },
  // ---- everyone else's side ---------------------------------------------------------------------------------------------
  clientTick() {
    const M = MATCH, me = this.match;
    if (!me) return;
    const p = heroPad();
    let held = (p.x < 0 ? 1 : 0) | (p.x > 0 ? 2 : 0) | (p.y < 0 ? 4 : 0) | (p.y > 0 ? 8 : 0) | (p.jump ? 16 : 0) | (p.attack ? 32 : 0) | (p.special ? 64 : 0) | (p.shield ? 128 : 0);
    NET_EDGES.forEach((k, i) => { if (p[k]) this.counters[i] = (this.counters[i] + 1) % 256; });
    if (sceneT % 2 === 0) this.send(['i', me.mid, ++this.seq, held, ...this.counters]);
    const s = this.snap;
    if (s) this.applySnap(s);
    if (M.end) { me.endT++; if (me.endT > 150) { MATCH.order = F.filter((f) => f.out).sort((a, b) => b.outAt - a.outAt); go(Results); } }
  },
  applySnap(s) {
    const M = MATCH;
    if (s !== this.applied) {
      this.applied = s;
      frameNo = s[2]; M.countdown = s[3]; if (s[4] && !M.end) M.end = 1;
      stageTick(STAGE, frameNo);
      s[5].forEach((d, i) => { const f = F[i]; if (!f) return;
        const st = NET_STATES[d[3]] || 'idle';
        if (st !== f.state && (st === 'attack' || st === 'special')) audio.sfx.swing();
        f.nx = d[0] / 10; f.ny = d[1] / 10; f.face = d[2] || 1; f.state = st; f.netAnim = [NET_ANIMS[d[4]] || 'idle', d[5]];
        f.dmg = d[6]; f.stocks = d[7]; f.dead = !!(d[8] & 1); f.out = !!(d[8] & 2); f.inv = d[8] & 4 ? 1 : 0; f.netBlink = !!(d[8] & 4); f.hitlag = d[8] & 8 ? 1 : 0;
        f.shieldHP = d[9]; f.spKey = d[8] & 16 ? 'neutral' : null; f.sp = d[8] & 16 ? { len: d[10] } : null; f.kos = d[11]; f.falls = d[12]; f.outAt = d[13]; f.t = d[14];
        if (f.x === undefined || Math.abs(f.nx - f.x) > 48 || Math.abs(f.ny - f.y) > 48) { f.x = f.nx; f.y = f.ny; } });
      PROJ = s[6].map((q) => ({ type: NET_PROJ[q[0]], x: q[1], y: q[2] }));
      for (const e of s[7]) { if (this.evSeen.has(e[0])) continue; this.evSeen.add(e[0]); this.replay(e.slice(1)); }
      if (this.evSeen.size > 400) this.evSeen = new Set([...this.evSeen].slice(-200));
    }
    for (const f of F) if (f.nx !== undefined) { f.x += (f.nx - f.x) * 0.55; f.y += (f.ny - f.y) * 0.55; }
  },
  replay(e) {
    const f = F[e[1]];
    if (e[0] === 'h') hitFX(e[2], e[3], e[4], e[5]);
    else if (e[0] === 'b') shieldFX(e[2], e[3], !!e[4]);
    else if (e[0] === 'k' && f) koFX(f, e[2], e[3], !!e[4], NET_WORDS[e[5]] || 'BONK!');
  },
};

// ---- the ONLINE scene: join, pick, ready, the host picks the stage ---------------------------------------------------------
const Online = {
  enter() {
    audio.play(STAGES.arcade.song); this.leaveT = 0;
    if (!NET.room && !NET.joining) NET.join();
  },
  update() {
    backdrop(sceneT);
    text('ONLINE', W / 2, 8, { size: 3, align: 'center', color: '#ffd23a', outline: '#160e1e' });
    const p = P1();
    if (!NET.rt()) {
      ['ONLINE PLAY RUNS INSIDE THE ARCADE APP.', '', 'OPEN ARCADE KNOCKOUT FROM THE ARCADE APP STORE', 'AND EVERYONE WHO OPENS ONLINE MEETS IN THE SAME ROOM.', '', 'ESC: BACK'].forEach((l, i) => text(l, W / 2, 80 + i * 14, { align: 'center', color: i === 5 ? '#9a86c0' : '#ffffff' }));
      if (p.backP || clicks.length) go(Menu); return;
    }
    if (!NET.room) {
      text(NET.joining ? 'JOINING A ROOM...' : 'COULD NOT GO ONLINE: ' + String(NET.why).toUpperCase().slice(0, 60), W / 2, 100, { align: 'center', color: NET.joining ? '#ffffff' : '#ff8a3a' });
      if (!NET.joining) { text('ENTER: TRY AGAIN · ESC: BACK', W / 2, 130, { align: 'center', color: '#9a86c0' }); if (p.startP || p.attackP) NET.join(); }
      if (p.backP) go(Menu); return;
    }
    NET.watch(); NET.lobbyTick();
    if (scene !== Online) return;
    const P = NET.people(), L = NET.lobby, host = NET.isHost(), mine = NET.myPick();
    text(`ROOM ${NET.roomNo} · ${P.length} / ${NET_MAX} PLAYERS · ${STAGES[L.stage].name} · ${L.stocks} STOCKS${L.cpus ? ' · ' + L.cpus + ' CPU' : ''}`, W / 2, 34, { align: 'center', color: '#9a86c0' });
    const n = Math.max(2, Math.min(NET_MAX, P.length + L.cpus)), cw = 104, gap = (W - n * cw) / (n + 1);
    for (let i = 0; i < n; i++) {
      const x = Math.round(gap + i * (cw + gap)), y = 50, pl = P[i], cpu = !pl && i < P.length + L.cpus;
      rect(ctx, x, y, cw, 150, '#160e1ecc'); rect(ctx, x, y, cw, 3, PCOLORS[i]);
      if (!pl && !cpu) { text('WAITING...', x + cw / 2, y + 70, { align: 'center', color: '#6a5a8a' }); continue; }
      const pk = cpu ? { who: '?', ready: true } : NET.picks[pl.id] || { who: 'buster', ready: false }, isMe = pl && NET.me && pl.id === NET.me.id;
      text(cpu ? 'CPU' : (isMe ? 'YOU' : NET.nameOf(pl)).slice(0, 16), x + cw / 2, y + 8, { align: 'center', color: PCOLORS[i] });
      if (pl && pl.id === NET.hostId()) text('HOST', x + cw / 2, y + 20, { align: 'center', color: '#ffd23a' });
      if (!cpu) { drawSprite(pk.who, pk.ready ? 'win' : 'idle', pk.ready ? (sceneT >> 4) % 2 : (sceneT / 9 | 0) % 4, x + cw / 2, y + 112); text(CHARS[pk.who].name, x + cw / 2, y + 116, { align: 'center' }); }
      else text('RANDOM', x + cw / 2, y + 80, { align: 'center' });
      text(pk.ready ? 'READY!' : 'PICKING', x + cw / 2, y + 128, { align: 'center', color: pk.ready ? '#5aff8a' : '#9a86c0' });
      if (pl && !isMe) { const l = NET.link(pl.id); text(l === 'direct' ? 'FAST LINK' : l === 'room' ? 'SLOW LINK' : l ? 'LINKING...' : '', x + cw / 2, y + 140, { align: 'center', color: l === 'direct' ? '#5aff8a' : '#ff8a3a' }); }
    }
    // my controls
    if (!mine.ready) { if (p.leftP || p.rightP) { const k = ROSTER.indexOf(mine.who); mine.who = ROSTER[(k + (p.rightP ? 1 : ROSTER.length - 1)) % ROSTER.length]; NET.pickDirty = true; audio.sfx.select(); } }
    if (p.startP || p.attackP) { mine.ready = !mine.ready; NET.pickDirty = true; audio.sfx[mine.ready ? 'ok' : 'select'](); }
    if (host && L.phase === 'lobby' && !mine.ready) {
      if (keysP.has('KeyV') || keysP.has('Period')) { L.stage = STAGE_ORDER[(STAGE_ORDER.indexOf(L.stage) + 1) % STAGE_ORDER.length]; NET.lobbyDirty = true; audio.sfx.select(); }
      if (keysP.has('KeyB') || keysP.has('Slash')) { L.cpus = (L.cpus + 1) % (NET_MAX - P.length + 1); NET.lobbyDirty = true; audio.sfx.select(); }
      if (keysP.has('KeyN')) { L.stocks = L.stocks % 5 + 1; NET.lobbyDirty = true; audio.sfx.select(); }
    }
    let msg = mine.ready ? 'READY! ENTER: CHANGE YOUR MIND' : 'LEFT/RIGHT: PICK A FIGHTER · ENTER OR C: READY';
    text(msg, W / 2, 208, { align: 'center' });
    text(host ? 'YOU ARE HOST: V STAGE · B CPUS · N STOCKS (WHILE PICKING)' : 'THE HOST PICKS THE STAGE', W / 2, 222, { align: 'center', color: '#5aebf5' });
    if (L.phase === 'match' && !NET.match) text('A MATCH IS ON. YOU ARE IN THE NEXT ONE.', W / 2, 236, { align: 'center', color: '#ffd23a' });
    else if (L.start) text('STARTING IN ' + Math.ceil(L.start / 60) + '...', W / 2, 236, { align: 'center', color: '#5aff8a', size: 1 });
    else if (P.length + L.cpus < 2) text('WAITING FOR SOMEONE ELSE TO OPEN ONLINE' + (host ? ' (OR ADD A CPU: B)' : ''), W / 2, 236, { align: 'center', color: '#9a86c0' });
    else text('THE MATCH STARTS WHEN EVERYONE IS READY', W / 2, 236, { align: 'center', color: '#9a86c0' });
    if (NET.noteT > 0) { NET.noteT--; text(NET.note, W / 2, 250, { align: 'center', color: '#ff8a3a' }); }
    else text('ESC: LEAVE THE ROOM', W / 2, 252, { align: 'center', color: '#6a5a8a' });
    if (p.backP && !mine.ready) { NET.leave(); go(Menu); }
    else if (p.backP) { mine.ready = false; NET.pickDirty = true; }
  },
};
