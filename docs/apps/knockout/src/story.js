// story.js - STORY MODE: "The UTXO Chronicles". Chapter cards, talk scenes, rhythm cheers, boss fights, continues, credits.
'use strict';
const STORY_SONG = { bpm: 96, bass: [45, 0, 0, 0, 52, 0, 0, 0, 50, 0, 0, 0, 48, 0, 0, 0], lead: [69, 0, 0, 72, 0, 0, 76, 0, 74, 0, 72, 0, 71, 0, 0, 0, 69, 0, 0, 72, 0, 0, 77, 0, 76, 0, 74, 0, 72, 0, 0, 0] };
const BOSS_SONG = { bpm: 150, bass: [40, 40, 0, 40, 43, 0, 40, 0, 38, 38, 0, 38, 41, 0, 38, 0], lead: [64, 0, 67, 0, 70, 0, 69, 67, 0, 64, 0, 62, 64, 0, 0, 0, 64, 0, 67, 0, 71, 0, 72, 71, 0, 67, 0, 69, 67, 0, 0, 0], drum: true };
const HOPE_SONG = { bpm: 120, bass: [48, 0, 48, 0, 55, 0, 55, 0, 53, 0, 53, 0, 50, 0, 55, 0], lead: [72, 0, 76, 0, 79, 0, 84, 0, 83, 0, 79, 0, 76, 0, 79, 0, 77, 0, 81, 0, 84, 0, 81, 0, 79, 0, 76, 0, 72, 0, 0, 0], drum: true };

// ---- people who are not fighters (simple pixel figures, not detailed likenesses) --------------------------------------
function drawNPC(kind, x, y, face = 1, t = 0, pose = 'idle', g = ctx) {
  g.save(); g.translate(Math.round(x), Math.round(y)); if (face < 0) g.scale(-1, 1);
  const bob = pose === 'jump' ? -Math.abs(Math.sin(t / 6)) * 6 | 0 : (t >> 4) % 2;
  if (kind === 'charlie') {
    ob(g, -7, -16, 6, 16, '#2a3a5a'); ob(g, 1, -16, 6, 16, '#2a3a5a'); rect(g, -8, -2, 7, 2, '#160e1e'); rect(g, 1, -2, 8, 2, '#160e1e');
    ob(g, -9, -36 + bob, 18, 21, '#8a8a9a'); rect(g, -3, -36 + bob, 6, 3, '#6a6a7a');
    ob(g, 2, -32 + bob, 5, 5, '#e8eef8'); rect(g, 3, -31 + bob, 1, 3, '#3a6ad8'); rect(g, 3, -29 + bob, 3, 1, '#3a6ad8');   // a silver L badge
    if (pose === 'type') ob(g, 6, -30 + bob, 11, 4, '#8a8a9a');
    else if (pose === 'wave') { const w = (t >> 3) % 2; ob(g, 7, -50 + bob + w, 4, 15, '#8a8a9a'); ob(g, 6, -54 + bob + w, 6, 5, '#e8c09a'); }
    else if (pose === 'flip') { ob(g, 6, -40 + bob, 4, 10, '#8a8a9a'); const cy = -56 - Math.abs(Math.sin(t / 10)) * 20; rect(g, 8, cy, [5, 3, 1, 3][(t >> 2) % 4], 5, '#c8d4e8'); }
    else ob(g, 6, -34 + bob, 4, 14, '#8a8a9a');
    ob(g, -7, -50 + bob, 14, 14, '#e8c09a'); rect(g, -8, -52 + bob, 16, 5, '#1a1418'); rect(g, -8, -48 + bob, 4, 5, '#1a1418');
    rect(g, 3, -45 + bob, 2, 2, '#160e1e'); rect(g, 2, -40 + bob, 4, 1, '#7a3a2a');
  } else if (kind === 'satoshi') {
    ob(g, -11, -42, 22, 42, '#3a3a4a'); rect(g, -11, -6, 22, 6, '#2a2a3a');
    ob(g, -9, -56 + bob, 18, 16, '#2a2a3a'); rect(g, -4, -52 + bob, 11, 10, '#0a0a10'); rect(g, 1, -49 + bob, 2, 1, '#f7931a'); rect(g, 5, -49 + bob, 2, 1, '#f7931a');
    text('B', 0, -30, { align: 'center', color: (t >> 4) % 2 ? '#f7931a' : '#ffb04a', shadow: false, g }); rect(g, -1, -32, 1, 11, '#f7931a'); rect(g, 1, -32, 1, 11, '#f7931a');
  } else if (kind === 'pizza') {
    ob(g, -7, -16, 6, 16, '#3a4a6a'); ob(g, 1, -16, 6, 16, '#3a4a6a');
    ob(g, -9, -36 + bob, 18, 21, '#c83a3a'); ob(g, -7, -50 + bob, 14, 14, '#f0c8a0'); rect(g, -8, -52 + bob, 16, 4, '#6a4a2a'); rect(g, 3, -45 + bob, 2, 2, '#160e1e'); rect(g, 1, -40 + bob, 5, 1, '#7a3a2a');
    if (pose === 'pizza') { ob(g, 4, -32 + bob, 22, 6, '#e8d0a0'); ob(g, 4, -38 + bob, 22, 6, '#e8d0a0'); rect(g, 10, -36 + bob, 8, 2, '#c83a3a'); }
    else ob(g, 6, -34 + bob, 4, 14, '#c83a3a');
  } else if (kind === 'devs') {
    for (const [dx, shirt, hair] of [[-12, '#3a6ad8', '#6a4a2a'], [12, '#3aaa6a', '#d8b06a']]) {
      ob(g, dx - 6, -16, 5, 16, '#2a2a3a'); ob(g, dx + 1, -16, 5, 16, '#2a2a3a'); ob(g, dx - 8, -35 + bob, 16, 20, shirt);
      ob(g, dx - 6, -48 + bob, 12, 13, '#f0c8a0'); rect(g, dx - 7, -50 + bob, 14, 4, hair); rect(g, dx + 2, -44 + bob, 2, 2, '#160e1e'); rect(g, dx, -39 + bob, 4, 1, '#7a3a2a');
    }
    if (pose === 'laugh') text('HA HA', 0, -64 - bob, { align: 'center', color: '#ffd23a', g });
  } else if (kind === 'crowd') {
    const cols = ['#ff5fd2', '#3ad0f0', '#ffd23a', '#5aff8a', '#ff8a3a', '#c8a2ff', '#f4f4ec'];
    for (let i = 0; i < 7; i++) { const dx = (i - 3) * 13, j = Math.abs(Math.sin(t / 5 + i * 1.3)) * 5 | 0;
      ob(g, dx - 5, -22 - j, 10, 14, cols[i]); ob(g, dx - 4, -32 - j, 8, 9, '#e8c09a'); rect(g, dx - 7 + (i % 2) * 12, -34 - j, 2, 10, cols[i]); }
  }
  g.restore();
}

// ---- who can speak ----------------------------------------------------------------------------------------------------
const SPEAK = {
  narr: { narr: true }, term: { term: true },
  arcade: { spr: 'arcade' }, buster: { spr: 'buster' }, lester: { spr: 'lester' }, shiba: { spr: 'shiba' }, pep3d: { spr: 'pep3d' }, digi: { spr: 'digi' },
  charlie: { name: 'CHARLIE LEE', color: '#c8d4e8', npc: 'charlie' }, satoshi: { name: 'SATOSHI', color: '#f7931a', npc: 'satoshi' },
  pizza: { name: 'THE PIZZA GUY', color: '#ffd23a', npc: 'pizza' }, devs: { name: 'TWO DEVS WITH A JOKE', color: '#f0a040', npc: 'devs' }, crowd: { name: 'THE CROWD', color: '#ffffff', npc: 'crowd' },
};
for (const k of Object.keys(BOSSES)) SPEAK[k] = { name: BOSSES[k].name, color: BOSSES[k].color, boss: k };
for (const k of ROSTER) Object.assign(SPEAK[k], { name: CHARS[k].name, color: CHARS[k].color });
function wrapText(s, n) { const out = []; let line = ''; for (const w of s.split(' ')) { if ((line + ' ' + w).trim().length > n) { out.push(line); line = w; } else line = (line + ' ' + w).trim(); } if (line) out.push(line); return out; }
function anyPress() { const p = menuPad(), q = readDevice('k2'); return p.attackP || p.jumpP || p.startP || q.attackP || q.jumpP || q.startP || clicks.length > 0; }
function drawActor(a, t, speaking) {
  const bob = speaking ? -((t >> 3) % 2) : 0;
  if (SPEAK[a.id] && SPEAK[a.id].spr) { const who = SPEAK[a.id].spr, anim = a.pose === 'win' ? 'win' : a.pose === 'hurt' ? 'hurt' : 'idle', n = anim === 'idle' ? 4 : 2;
    drawSprite(who, anim, (t / (anim === 'idle' ? 9 : 16) | 0) % n, a.x, a.y + bob, a.face < 0); }
  else if (SPEAK[a.id] && SPEAK[a.id].boss) drawBossStill(a.id, a.x, a.y, a.scale || 0.8, t, a.face);
  else drawNPC(a.id, a.x, a.y, a.face, t, a.pose);
}
function drawPortrait(id, x, y, s, t) {
  rect(ctx, x, y, s, s, '#2a1a40'); rect(ctx, x, y + s - 2, s, 2, '#160e1e');
  ctx.save(); ctx.beginPath(); ctx.rect(x + 1, y + 1, s - 2, s - 2); ctx.clip();
  const S = SPEAK[id];
  if (S.spr) { const hh = CHARS[S.spr].hh; drawSprite(S.spr, 'idle', (t / 9 | 0) % 4, x + s / 2, y + s - 4 + Math.max(0, hh - 46) * 0.9, false); }
  else if (S.boss) { const B = BOSSES[S.boss], k = Math.min(1, (s - 8) / Math.max(B.w, B.h)); drawBossStill(S.boss, x + s / 2, y + s - 3, k, t); }
  else drawNPC(S.npc, x + s / 2, y + s + (S.npc === 'crowd' ? 0 : 6), 1, t, 'idle');
  ctx.restore();
}

// ---- backgrounds for talk scenes ---------------------------------------------------------------------------------------
const BG_CACHE = {};
function drawSceneBG(key, t, sc) {
  if (STAGES[key]) { const S = STAGES[key]; if (!S.bg) S.bg = offscreen(W, H, (g) => S.paint.call(S, g)); ctx.drawImage(S.bg, 0, 0); S.live(t); return S.main.y; }
  if (key === 'terminal') {
    rect(ctx, 0, 0, W, H, '#020a04'); for (let y = 0; y < H; y += 3) rect(ctx, 0, y, W, 1, '#04140a');
    (sc.term || []).forEach((l, i) => text(l, 24, 24 + i * 12, { color: '#5aff8a', shadow: false }));
    if ((t >> 4) % 2) rect(ctx, 24 + textW((sc.term || []).slice(-1)[0] || '') + 3, 24 + ((sc.term || []).length - 1) * 12, 5, 7, '#5aff8a');
    return 190;
  }
  if (key === 'news') {
    gradSky(ctx, ['#120a20', '#24123c']); ctx.save(); ctx.translate(0, 18);   // below the 2-line caption box
    rect(ctx, 80, 24, 320, 150, '#e8e0cc'); rect(ctx, 80, 24, 320, 2, '#160e1e'); text('THE DAILY BLOCK', 240, 32, { size: 2, align: 'center', color: '#160e1e', shadow: false });
    const N = sc.news || { date: '3 JANUARY 2009', lines: ['CHANCELLOR ON BRINK OF', 'SECOND BAILOUT FOR BANKS'] };
    text(N.date, 240, 52, { align: 'center', color: '#5a5040', shadow: false }); rect(ctx, 92, 62, 296, 1, '#160e1e');
    N.lines.forEach((l, i) => text(l, 240, 72 + i * 17, { size: 2, align: 'center', color: '#160e1e', shadow: false }));
    for (let i = 0; i < 6 - N.lines.length + 2; i++) { rect(ctx, 96, 124 + (N.lines.length - 2) * 8 + i * 8, 136, 3, '#a89a80'); rect(ctx, 248, 124 + (N.lines.length - 2) * 8 + i * 8, 136, 3, '#a89a80'); }
    ctx.restore(); return 196;
  }
  if (key === 'room') {
    rect(ctx, 0, 0, W, H, '#1a1430'); rect(ctx, 0, 190, W, 80, '#2a2040'); for (let x = 0; x < W; x += 24) rect(ctx, x, 190, 1, 80, '#221a36');
    rect(ctx, 300, 40, 120, 80, '#0a0e22'); rect(ctx, 300, 40, 120, 3, '#3a3050'); rect(ctx, 358, 40, 3, 80, '#3a3050');
    for (let i = 0; i < 18; i++) rect(ctx, 304 + (i * 37) % 112, 46 + (i * 23) % 70, 1, 1, '#ffffffaa');
    rect(ctx, 40, 150, 200, 6, '#6a4a2a'); rect(ctx, 50, 156, 6, 34, '#4a3020'); rect(ctx, 224, 156, 6, 34, '#4a3020');
    ob(ctx, 120, 112, 56, 36, '#2a2a34'); rect(ctx, 124, 116, 48, 28, sc.glow ? '#e8eef8' : '#3a6ad8'); ob(ctx, 112, 148, 72, 3, '#5a5a6a');
    if (!sc.glow) for (let i = 0; i < 4; i++) rect(ctx, 128, 120 + i * 6, 12 + (i * 13 + (t >> 3)) % 28, 2, '#8ac8ff');
    text('OCT 2011', 360, 128, { align: 'center', color: '#6a5a8a', shadow: false });
    return 190;
  }
  if (key === 'switch') {
    rect(ctx, 0, 0, W, H, '#10182a'); rect(ctx, 0, 190, W, 80, '#1a2236');
    ob(ctx, 300, 70, 80, 120, '#3a4462'); rect(ctx, 306, 76, 68, 30, '#0a0e22');
    const on = sc.switchOn, sig = sc.signal ?? 0;
    text('SEGWIT', 340, 84, { align: 'center', color: on ? '#5aff8a' : '#6a7a9a', shadow: false }); text(on ? 'ON' : 'OFF', 340, 94, { align: 'center', color: on ? '#5aff8a' : '#ff4a5a', shadow: false });
    ob(ctx, 330, 120, 20, 50, '#2a3048'); ob(ctx, 336, on ? 124 : 150, 8, 14, on ? '#5aff8a' : '#ff4a5a');
    rect(ctx, 60, 60, 200, 10, '#2a3048'); rect(ctx, 60, 60, 200 * sig, 10, '#5a8ad8'); rect(ctx, 60 + 150, 56, 2, 18, '#ffd23a'); text('75%', 211, 46, { align: 'center', color: '#ffd23a', shadow: false });
    text('MINERS SIGNALING', 60, 76, { color: '#9a86c0', shadow: false });
    return 190;
  }
  backdrop(t); return 200;
}

// ---- TALK: a list of lines, a background and some people standing in it -------------------------------------------------
const Talk = {
  enter(cfg) {
    this.cfg = cfg; this.i = -1; FX = []; this.sc = { term: [], glow: false, switchOn: false, signal: cfg.signal ?? 0 };
    this.actors = (cfg.actors || []).map((a) => this.mkActor(a)); this.bg = cfg.bg || 'void';
    audio.play(cfg.music === 'boss' ? BOSS_SONG : cfg.music === 'hope' ? HOPE_SONG : cfg.music && STAGES[cfg.music] ? STAGES[cfg.music].song : STORY_SONG);
    this.next();
  },
  mkActor(a) { return { id: a[0], x: a[1], face: a[2] ?? (a[1] < W / 2 ? 1 : -1), pose: a[3] || 'idle', scale: a[4] }; },
  next() {
    this.i++; const L = this.cfg.lines;
    if (this.i >= L.length) { storyNext(); return; }
    const [who, s, o = {}] = L[this.i];
    if (o.bg) this.bg = o.bg;
    if (o.enter) { const a = this.mkActor(o.enter); this.actors = this.actors.filter((b) => b.id !== a.id).concat(a); sparkBurst(a.x, (this.floor || 190) - 24, 14, '#ffd23a', 1); audio.sfx.coin(); }
    if (o.exit) this.actors = this.actors.filter((b) => ![].concat(o.exit).includes(b.id));
    if (o.pose) for (const [id, p] of o.pose) { const a = this.actors.find((b) => b.id === id); if (a) a.pose = p; }
    if (o.term) this.sc.term.push(o.term);
    if (o.set) Object.assign(this.sc, o.set);
    if (o.sfx) audio.sfx[o.sfx]();
    if (o.shake) shake = o.shake;
    if (o.flash) flash = 6;
    if (o.music) audio.play(o.music === 'stop' ? { bpm: 60, bass: [0], lead: [0] } : o.music === 'boss' ? BOSS_SONG : o.music === 'hope' ? HOPE_SONG : STORY_SONG);
    const S = SPEAK[who];
    this.who = who; this.lines = wrapText(s, S.narr || S.term ? 74 : 64); this.shown = 0; this.lineT = 0;
  },
  update() {
    const t = sceneT; this.lineT++;
    ctx.save(); if (shake > 0) { ctx.translate(rnd(-shake, shake) | 0, rnd(-shake, shake) | 0); shake *= 0.85; if (shake < 0.5) shake = 0; }
    this.floor = drawSceneBG(this.bg, t, this.sc);
    for (const a of this.actors) { a.y = this.floor; drawActor(a, t, a.id === this.who); }
    drawFX(); ctx.restore();
    if (flash > 0) { ctx.fillStyle = '#ffffff'; ctx.globalAlpha = flash / 6; ctx.fillRect(0, 0, W, H); ctx.globalAlpha = 1; flash--; }
    const S = SPEAK[this.who], total = this.lines.join('').length;
    this.shown = Math.min(total, this.shown + 1.6);
    let left = Math.floor(this.shown);
    const visible = this.lines.map((l) => { const s = l.slice(0, Math.max(0, left)); left -= l.length; return s; });
    if (S.narr || S.term) {
      const h = 14 + this.lines.length * 11;
      rect(ctx, 8, 8, W - 16, h, S.term ? '#020a04ee' : '#f2e6c8'); rect(ctx, 8, 8 + h - 2, W - 16, 2, '#160e1e'); rect(ctx, 8, 8, 3, h, S.term ? '#5aff8a' : '#ff5fd2');
      visible.forEach((l, i) => text(l, 18, 15 + i * 11, { color: S.term ? '#5aff8a' : '#160e1e', shadow: false }));
    } else {
      rect(ctx, 6, 198, W - 12, 66, '#160e1eee'); rect(ctx, 6, 198, W - 12, 2, S.color);
      drawPortrait(this.who, 12, 204, 54, t);
      text(S.name, 74, 205, { color: S.color });
      visible.forEach((l, i) => text(l, 74, 220 + i * 11, { color: '#ffffff' }));
    }
    const done = this.shown >= total;
    if (done && (t >> 4) % 2) text('>', W - 18, S.narr || S.term ? 10 + this.lines.length * 11 : 252, { color: S.narr ? '#160e1e' : '#ffd23a', shadow: false });
    text('ESC: SKIP SCENE', W - 10, S.narr || S.term ? H - 10 : 188, { align: 'right', color: '#ffffff55', shadow: false });
    if (sceneT < 20) return;
    if (P1().backP) { audio.sfx.select(); storyNext(); return; }
    if (anyPress() && this.lineT > 6) { if (!done) this.shown = total; else { audio.sfx.select(); this.next(); } }
  },
};

// ---- CHEER: a rhythm moment. Rings close on numbered markers: press any button as they meet. -----------------------------
const CHEERS = {
  pizza: {
    title: 'PIZZA DAY', bpm: 120, lead: 4,
    song: { bpm: 120, bass: [48, 0, 55, 0, 52, 0, 55, 0, 53, 0, 57, 0, 55, 0, 59, 0], lead: [72, 0, 74, 0, 76, 0, 79, 0, 77, 0, 76, 0, 74, 0, 72, 0, 72, 0, 76, 0, 79, 0, 84, 0, 83, 0, 79, 0, 76, 0, 74, 0], drum: true },
    notes: [4, 5, 6, 7, 10, 11, 12, 13, 16, 17, 17.5, 18, 19, 22, 23, 24, 25, 25.5, 26, 30, 31, 32, 33, 34, 35, 38, 39, 40, 41],
    caps: [[0, 'MAY 2010. A DEV POSTS: "10,000 BTC FOR TWO PIZZAS. ANYONE?"'], [9, 'DAYS GO BY. NOBODY BITES. (THE PIZZA, OR THE OFFER.)'], [15, 'THEN SOMEONE SAYS: DEAL!'],
      [21, 'PIZZAS ORDERED. CHEESE INCOMING.'], [29, 'DING DONG! THE FIRST REAL THING EVER BOUGHT WITH BITCOIN!'], [37, 'MAY 22 IS NOW PIZZA DAY. A HOLIDAY IS BORN.']],
    draw(k, t) {
      gradSky(ctx, ['#2a1446', '#4a1a4a', '#7a2a3a']);
      rect(ctx, 0, 200, W, 70, '#3a2030');
      drawNPC('pizza', 90, 200, 1, t, k > 0.7 ? 'pizza' : 'idle');
      const n = k > 0.55 ? 2 : k > 0.35 ? 1 : 0;
      for (let i = 0; i < n; i++) { const x = 330 + i * 50, y = 150; ob(ctx, x - 20, y, 40, 40, '#e8b84a'); rect(ctx, x - 16, y + 4, 32, 32, '#d84a2a'); for (let j = 0; j < 5; j++) rect(ctx, x - 12 + (j * 11) % 24, y + 8 + (j * 7) % 24, 5, 5, '#7a1a1a'); }
      const V = [['2010', '$41'], ['2013', 'ABOUT $10 MILLION'], ['2017', 'ABOUT $200 MILLION'], ['2021', 'ABOUT $690 MILLION'], ['2024', 'ABOUT $1 BILLION']], v = V[Math.min(4, Math.floor(k * 5.2))];
      rect(ctx, 150, 60, 180, 40, '#160e1ecc'); text('10,000 BTC IN ' + v[0] + ':', 240, 68, { align: 'center', color: '#9a86c0' }); text(v[1], 240, 82, { align: 'center', color: '#5aff8a' });
    },
  },
  dogemas: {
    title: 'SAVE DOGEMAS', bpm: 128, lead: 4,
    song: { bpm: 128, bass: [43, 0, 50, 0, 43, 0, 50, 0, 48, 0, 52, 0, 50, 0, 47, 0], lead: [76, 0, 76, 0, 76, 0, 0, 0, 76, 0, 76, 0, 76, 0, 0, 0, 76, 0, 79, 0, 72, 0, 74, 0, 76, 0, 0, 0, 0, 0, 0, 0], drum: true },
    notes: [4, 5, 6, 8, 9, 10, 12, 13, 14, 15, 18, 19, 20, 22, 23, 24, 26, 27, 28, 29, 30, 34, 35, 36, 37, 38, 39],
    caps: [[0, 'CHRISTMAS 2013. A WALLET SERVICE IS ROBBED. MILLIONS OF DOGE, GONE.'], [11, 'THE COMMUNITY: "NOPE. NOT ON DOGEMAS."'], [17, 'TIPS POUR IN FROM EVERYWHERE. MUCH GENEROUS.'],
      [25, 'THE STOLEN DOGE GETS REPLACED, COIN BY COIN.'], [33, 'SAVE DOGEMAS: COMPLETE. DO ONLY GOOD EVERYDAY.']],
    draw(k, t) {
      gradSky(ctx, ['#0a1430', '#1a2a5a', '#2a3a6a']);
      for (let i = 0; i < 40; i++) rect(ctx, (i * 53 + t * 0.3 * (1 + i % 3)) % W, (i * 31 + t * (0.5 + (i % 4) * 0.2)) % H, 2, 2, '#ffffffaa');
      rect(ctx, 0, 206, W, 64, '#e8eef8');
      ob(ctx, 300, 70, 50, 90, '#d83a4a'); ob(ctx, 300, 160, 80, 34, '#d83a4a'); rect(ctx, 296, 64, 58, 14, '#f4f4ec');
      const n = Math.round(k * 14); for (let i = 0; i < n; i++) { const x = 306 + (i * 17) % 40, y = 186 - (i / 3 | 0) * 9; ob(ctx, x, y, 8, 8, '#ffd23a'); rect(ctx, x + 3, y + 2, 2, 4, '#c8901a'); }
      drawSprite('shiba', k > 0.9 ? 'win' : 'idle', (t / 12 | 0) % (k > 0.9 ? 2 : 4), 140, 206);
      text('REFUNDED: ' + Math.round(k * 100) + '%', 240, 40, { align: 'center', size: 2, color: '#ffd23a', outline: '#160e1e' });
    },
  },
  segwit: {
    title: 'TEST PILOT', bpm: 132, lead: 4,
    song: { bpm: 132, bass: [45, 45, 0, 45, 52, 0, 45, 0, 43, 43, 0, 43, 50, 0, 48, 0], lead: [69, 0, 72, 0, 76, 0, 81, 0, 79, 0, 76, 0, 72, 0, 76, 0, 74, 0, 77, 0, 81, 0, 84, 0, 83, 0, 79, 0, 76, 0, 0, 0], drum: true },
    notes: [4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 18, 18.5, 19, 20, 21, 22, 23, 26, 27, 28, 29, 30, 30.5, 31, 34, 35, 36, 37, 38, 39, 40],
    caps: [[0, '2017. BITCOIN HAS BEEN ARGUING FOR TWO YEARS.'], [9, 'LITECOIN: "WHAT IF WE JUST... TRY IT FIRST?"'], [17, 'MINERS START SIGNALING. 75% NEEDED.'],
      [25, 'ALMOST... ALMOST...'], [33, 'LOCKED IN! MAY 10, 2017: SEGWIT GOES LIVE ON LITECOIN.']],
    draw(k, t, sc) {
      const sig = Math.min(1, k * 1.08), on = k > 0.98;
      drawSceneBG('switch', t, { signal: sig * 0.85, switchOn: on });
      drawNPC('charlie', 270, 190, 1, t, on ? 'wave' : 'idle'); drawSprite('lester', on ? 'win' : 'idle', (t / 12 | 0) % (on ? 2 : 4), 130, 190);
      if (on) text('TEST PILOT!', 240, 150, { size: 2, align: 'center', color: '#5aff8a', outline: '#160e1e' });
    },
  },
};
const Cheer = {
  enter(key) {
    const C = CHEERS[key]; this.key = key; this.C = C; this.t = 0; this.fpb = 3600 / C.bpm; this.meter = 60; this.res = null; FX = [];
    const r = seeded(key.length * 977 + 13); let x = 0, y = 0, dx = 0, dy = 0, prev = -9, n = 0;
    this.notes = C.notes.map((b) => {
      if (b - prev > 1.5) { x = 90 + r() * 300; y = 80 + r() * 90; const a = r() * Math.PI * 2; dx = Math.cos(a) * 40; dy = Math.sin(a) * 26; n = 0; }
      else { x += dx; y += dy; if (x < 50 || x > 430) { dx = -dx; x += dx * 2; } if (y < 60 || y > 186) { dy = -dy; y += dy * 2; } }
      prev = b; n++;
      return { f: Math.round((b + C.lead) * this.fpb), x, y, n, res: null };
    });
    this.end = this.notes[this.notes.length - 1].f + this.fpb * 3;
    audio.play(C.song);
  },
  judge(note, res) {
    note.res = res; const col = { COOL: '#5aff8a', GOOD: '#ffd23a', MISS: '#ff4a5a' }[res];
    this.meter = clamp(this.meter + { COOL: 6, GOOD: 3, MISS: -14 }[res], 0, 100);
    fx({ type: 'text', s: res + (res === 'COOL' ? '!' : ''), x: note.x, y: note.y - 14, life: 26, color: col });
    if (res !== 'MISS') { sparkBurst(note.x, note.y, res === 'COOL' ? 10 : 5, col, 0.8); audio.sfx[res === 'COOL' ? 'coin' : 'select'](); } else audio.sfx.shield();
  },
  update() {
    const C = this.C, ok = this.notes.filter((n) => n.res && n.res !== 'MISS').length, k = ok / this.notes.length;
    if (this.res) return this.results(k);
    this.t++; const t = this.t;
    C.draw(k, sceneT, this);
    // captions (comic-panel style)
    const cap = C.caps.filter(([b]) => t >= (b + C.lead) * this.fpb - this.fpb * 2).slice(-1)[0] || C.caps[0];
    rect(ctx, 8, 8, W - 16, 22, '#f2e6c8'); rect(ctx, 8, 28, W - 16, 2, '#160e1e'); rect(ctx, 8, 8, 3, 22, '#ff5fd2');
    text(cap[1], 18, 16, { color: '#160e1e', shadow: false });
    // markers
    const app = this.fpb * 2;
    for (const n of [...this.notes].reverse()) {
      if (n.res || t < n.f - app) continue;
      const q = (n.f - t) / app;
      ctx.fillStyle = '#160e1e'; ctx.beginPath(); ctx.arc(n.x, n.y, 12, 0, Math.PI * 2); ctx.fill();
      ctx.fillStyle = ['#ff5fd2', '#3ad0f0', '#ffd23a', '#5aff8a'][n.n % 4]; ctx.beginPath(); ctx.arc(n.x, n.y, 10, 0, Math.PI * 2); ctx.fill();
      text(String(n.n), n.x + 1, n.y - 3, { align: 'center', color: '#160e1e', shadow: false });
      if (q > -0.1) pixelCircle(n.x, n.y, 11 + Math.max(0, q) * 30, '#ffffff');
    }
    drawFX();
    // meter
    rect(ctx, 150, 248, 180, 10, '#160e1e'); rect(ctx, 151, 249, 178 * this.meter / 100, 8, this.meter > 35 ? '#5aff8a' : '#ff4a5a'); text('HYPE', 144, 249, { align: 'right', color: '#ffffff' });
    text('PRESS ANY BUTTON (OR CLICK) AS THE RING MEETS THE MARKER', W / 2, 236, { align: 'center', color: '#ffffffaa' });
    // input + judging
    const press = anyPress() || menuPad().specialP || menuPad().shieldP || readDevice('k2').specialP;
    if (press) { const n = this.notes.find((m) => !m.res && Math.abs(t - m.f) <= 12); if (n) this.judge(n, Math.abs(t - n.f) <= 5 ? 'COOL' : 'GOOD'); }
    if (KN_AUTO.cheer) for (const n of this.notes) if (!n.res && t === n.f) this.judge(n, 'COOL');
    for (const n of this.notes) if (!n.res && t - n.f > 12) this.judge(n, 'MISS');
    if (this.meter <= 0) { go(Continue, 'cheer'); return; }
    if (t > this.end) { this.res = { k, cools: this.notes.filter((n) => n.res === 'COOL').length }; this.resT = 0; audio.sfx.go(); }
    if (keysP.has('Escape')) go(StoryMenu);
  },
  results(k) {
    this.resT++; this.C.draw(k, sceneT, this);
    ctx.fillStyle = '#000000aa'; ctx.fillRect(0, 0, W, H);
    const rank = k >= 0.95 ? 'S' : k >= 0.8 ? 'A' : k >= 0.6 ? 'B' : 'C';
    text(this.C.title, W / 2, 60, { size: 3, align: 'center', color: '#ffd23a', outline: '#160e1e' });
    text('RANK ' + rank, W / 2, 100, { size: 4, align: 'center', color: { S: '#5aff8a', A: '#3ad0f0', B: '#ffd23a', C: '#ff8a3a' }[rank], outline: '#160e1e' });
    text(`COOL ${this.res.cools}   HITS ${Math.round(k * this.notes.length)} / ${this.notes.length}`, W / 2, 140, { align: 'center' });
    if (this.resT > 40) { if ((sceneT >> 4) % 2) text('PRESS ENTER', W / 2, 200, { align: 'center' }); if (anyPress()) storyNext(); }
  },
};

// ---- progress (saved when the browser allows it; a sandboxed app window may not, so every chapter is always open) -------
const SAVE = (() => { let d = { cleared: [] }; try { d = JSON.parse(localStorage.getItem('arcade-knockout-story') || 'null') || d; } catch (e) {}
  return { d, save() { try { localStorage.setItem('arcade-knockout-story', JSON.stringify(d)); } catch (e) {} } }; })();
const KN_AUTO = { cheer: false };
const SR = { ch: 0, beat: 0, easy: false, coop: false };
function storyStart(i) { SR.ch = i; SR.beat = 0; go(Card, i); }
function storyNext() {
  const C = CHAPTERS[SR.ch];
  if (SR.beat >= C.beats.length) { if (!SAVE.d.cleared.includes(SR.ch)) { SAVE.d.cleared.push(SR.ch); SAVE.save(); } go(ChapterClear); return; }
  const b = C.beats[SR.beat++];
  if (b.talk) go(Talk, b.talk); else if (b.cheer) go(Cheer, b.cheer); else if (b.fight) go(Fight, b.fight.stage, storyFight(b.fight));
}
function storyRetry() { SR.beat = Math.max(0, SR.beat - 1); storyNext(); }
function storyFight(fc) {
  const hpMul = SR.easy ? 0.65 : 1, dmgMul = SR.easy ? 0.7 : 1;
  const roster = [{ who: fc.hero, ctrl: 'p1', team: 'good', stocks: (fc.stocks ?? 3) + (SR.easy ? 1 : 0) }];
  const allies = (fc.allies || []).map(([who, level]) => ({ who, ctrl: 'cpu', level: level + (SR.easy ? 1 : 0), team: 'good', stocks: 3 }));
  if (SR.coop) { if (allies.length) allies[0].ctrl = 'k2'; else allies.push({ who: fc.buddy || (fc.hero === 'lester' ? 'buster' : 'lester'), ctrl: 'k2', team: 'good', stocks: 3 }); }
  const foes = fc.foes.map((f) => f.boss ? { boss: f.boss, team: 'bad', hpMul: hpMul * (SR.coop ? 1.25 : 1), dmgMul, x: f.x } : { who: f.who, ctrl: 'cpu', level: Math.max(1, f.level - (SR.easy ? 2 : 0)), team: 'bad', stocks: f.stocks ?? 2 });
  return { roster: [...roster, ...allies, ...foes], goal: fc.goal, hint: fc.hint, time: fc.time, survive: fc.survive, clock: fc.clock, weather: fc.weather, crowd: fc.crowd, song: fc.song };
}

// ---- story menu ---------------------------------------------------------------------------------------------------------
const StoryMenu = {
  sel: 0,
  enter() { audio.play(STORY_SONG); },
  update() {
    backdrop(sceneT);
    text('THE UTXO CHRONICLES', W / 2, 8, { size: 2, align: 'center', color: '#ffd23a', outline: '#160e1e' });
    const p = P1(), n = CHAPTERS.length, total = n + 2;
    if (p.downP) { this.sel = (this.sel + 1) % total; audio.sfx.select(); }
    if (p.upP) { this.sel = (this.sel + total - 1) % total; audio.sfx.select(); }
    CHAPTERS.forEach((C, i) => {
      const y = 30 + i * 15, sel = i === this.sel, done = SAVE.d.cleared.includes(i);
      if (button(14, y, 236, 13, '', { sel })) { this.sel = i; audio.sfx.ok(); storyStart(i); return; }
      text(`${i + 1}. ${C.title}`, 20, y + 3, { color: sel ? '#160e1e' : '#ffffff', shadow: !sel });
      text(done ? 'CLEAR' : C.year, 244, y + 3, { align: 'right', color: sel ? '#160e1e' : done ? '#5aff8a' : '#9a86c0', shadow: !sel });
    });
    if (scene !== StoryMenu) return;
    const C = CHAPTERS[Math.min(this.sel, n - 1)];
    rect(ctx, 262, 30, 206, 150, '#160e1ecc'); rect(ctx, 262, 30, 206, 2, '#ff5fd2');
    if (this.sel < n) {
      text(C.title, 365, 38, { align: 'center', color: '#ffd23a' }); text(C.year, 365, 50, { align: 'center', color: '#9a86c0' });
      wrapText(C.blurb, 32).forEach((l, i) => text(l, 272, 64 + i * 10, { color: '#ffffff' }));
      drawSprite(C.hero, 'idle', (sceneT / 9 | 0) % 4, 330, 176); if (C.boss) drawBossStill(C.boss, 410, 176, Math.min(0.6, 56 / BOSSES[C.boss].h), sceneT, -1);
    } else {
      text(this.sel === n ? 'DIFFICULTY' : 'PLAYERS', 365, 40, { align: 'center', color: '#ffd23a' });
      (this.sel === n ? ['EASY: WEAKER BOSSES, ONE MORE LIFE.', 'NORMAL: AS INTENDED.'] : ['1P: YOU + CPU ALLIES.', '2P: A FRIEND ON KEYS 2 (ARROWS', '+ , . /) TAKES AN ALLY. BOSSES', 'GET A LITTLE TOUGHER.']).forEach((l, i) => text(l, 272, 60 + i * 12));
    }
    const opt = (i, label) => { if (button(14 + (i - n) * 122, 200, 114, 16, label, { sel: this.sel === i })) { this.sel = i; this.toggle(i); } };
    opt(n, 'DIFFICULTY: ' + (SR.easy ? 'EASY' : 'NORMAL')); opt(n + 1, 'PLAYERS: ' + (SR.coop ? '2P' : '1P'));
    text('UP/DOWN PICK · ENTER PLAY · ESC BACK', W / 2, 232, { align: 'center', color: '#9a86c0' });
    text('A FAN-MADE, MOSTLY TRUE HISTORY. NOT FINANCIAL ADVICE.', W / 2, 252, { align: 'center', color: '#6a5a8a' });
    if (p.startP || p.attackP) { audio.sfx.ok(); if (this.sel < n) storyStart(this.sel); else this.toggle(this.sel); return; }
    if ((p.leftP || p.rightP) && this.sel >= n) this.toggle(this.sel);
    if (p.backP) go(Menu);
  },
  toggle(i) { audio.sfx.select(); if (i === CHAPTERS.length) SR.easy = !SR.easy; else SR.coop = !SR.coop; },
};
const Card = {
  enter(i) { this.i = i; audio.play(HOPE_SONG); },
  update() {
    const C = CHAPTERS[this.i];
    backdrop(sceneT);
    text('CHAPTER ' + (this.i + 1), W / 2, 40, { size: 2, align: 'center', color: '#9a86c0' });
    text(C.title, W / 2, 64, { size: 3, align: 'center', color: '#ffd23a', outline: '#160e1e' });
    text(C.year, W / 2, 96, { align: 'center', color: '#ffffff' });
    drawSprite(C.hero, 'win', (sceneT >> 4) % 2, C.boss ? 170 : W / 2, 200);
    if (C.boss) drawBossStill(C.boss, 320, 200, Math.min(1, 90 / BOSSES[C.boss].h), sceneT, -1);
    rect(ctx, 0, 200, W, 2, '#ff5fd2');
    if ((sceneT >> 5) % 2) text('INSERT COIN TO CONTINUE HISTORY', W / 2, 226, { align: 'center', color: '#ffffff' });
    text('ENTER: START · ESC: CHAPTERS', W / 2, 248, { align: 'center', color: '#9a86c0' });
    if (sceneT < 20) return;
    if (P1().backP) { go(StoryMenu); return; }
    if (anyPress() || sceneT > 420) { audio.sfx.coin(); storyNext(); }
  },
};
const Continue = {
  enter(what) { this.what = what; audio.play({ bpm: 70, bass: [40, 0, 0, 0, 39, 0, 0, 0, 38, 0, 0, 0, 37, 0, 0, 0], lead: [0] }); },
  update() {
    backdrop(sceneT);
    const left = 10 - Math.floor(sceneT / 50);
    if (left < 0) { text('GAME OVER', W / 2, 100, { size: 4, align: 'center', color: '#ff4a5a', outline: '#160e1e' }); if (sceneT > 50 * 11 + 90 || anyPress()) go(StoryMenu); return; }
    text('CONTINUE?', W / 2, 60, { size: 4, align: 'center', color: '#ffd23a', outline: '#160e1e' });
    text(String(left), W / 2, 110, { size: 6, align: 'center', color: '#ffffff', outline: '#160e1e' });
    text('ENTER: TRY AGAIN (COINS ARE FREE HERE)', W / 2, 190, { align: 'center' }); text('ESC: BACK TO CHAPTERS', W / 2, 204, { align: 'center', color: '#9a86c0' });
    if (SR.easy === false) text('TIP: EASY MODE IS IN THE STORY MENU. NO SHAME. MUCH KIND.', W / 2, 236, { align: 'center', color: '#6a5a8a' });
    if (sceneT < 30) return;
    if (P1().backP) { go(StoryMenu); return; }
    if (anyPress()) { audio.sfx.coin(); storyRetry(); }
  },
};
const ChapterClear = {
  enter() { audio.play(HOPE_SONG); },
  update() {
    backdrop(sceneT);
    const C = CHAPTERS[SR.ch], last = SR.ch === CHAPTERS.length - 1;
    text('CHAPTER CLEAR!', W / 2, 40, { size: 3, align: 'center', color: '#5aff8a', outline: '#160e1e' });
    text(C.title, W / 2, 72, { size: 2, align: 'center', color: '#ffd23a' });
    drawSprite(C.hero, 'win', (sceneT >> 4) % 2, W / 2, 190);
    text(last ? 'ENTER: THE END?' : 'NEXT: ' + CHAPTERS[SR.ch + 1].title, W / 2, 214, { align: 'center' });
    text('ENTER: CONTINUE · ESC: CHAPTERS', W / 2, 240, { align: 'center', color: '#9a86c0' });
    if (sceneT < 40) return;
    if (P1().backP) { go(StoryMenu); return; }
    if (anyPress()) { audio.sfx.ok(); if (last) go(Credits); else storyStart(SR.ch + 1); }
  },
};
const Credits = {
  enter() { audio.play(HOPE_SONG); },
  update() {
    backdrop(sceneT);
    const L = ['THE UTXO CHRONICLES', '', 'THE END?', '', 'THE STORY IS STILL BEING WRITTEN,', 'ONE BLOCK AT A TIME. MAYBE BY YOU.', '', 'STARRING', 'BUSTER · LESTER · SHIBA · DIGI · PEP · ARCADE', '', 'WITH A CAMEO BY CHARLIE LEE', 'AND A THANK YOU TO HAL FINNEY', '', 'A FAN-MADE, MOSTLY TRUE HISTORY, TOLD WITH JOKES.', 'NOT AFFILIATED WITH OR ENDORSED BY ANYONE IN IT.', 'NOT FINANCIAL ADVICE. ESPECIALLY THE TORNADO.', '', 'A GAME BY @YOURFIRSTNAME ON ARCADE', '', 'RUN A NODE.', '', '', 'PRESS ENTER'];
    const y0 = Math.max(H - sceneT * 0.5, 150 - (L.length - 1) * 14);
    L.forEach((l, i) => { const y = y0 + i * 14; if (y > -10 && y < 212) text(l, W / 2, y, { size: i === 0 ? 2 : 1, align: 'center', color: i === 0 ? '#ffd23a' : l === 'RUN A NODE.' ? '#5aff8a' : '#ffffff' }); });
    ROSTER.forEach((w, i) => drawSprite(w, 'win', (sceneT >> 4) % 2, 50 + i * 76, 266 + Math.max(0, 80 - sceneT) ));
    if (sceneT > 60 && anyPress()) go(StoryMenu);
  },
};

// ---- the chapters -------------------------------------------------------------------------------------------------------
// a talk line = [speaker, text, {enter:[id,x,face,pose], exit, pose:[[id,pose]], term, set, sfx, shake, flash, bg, music}]
const CHAPTERS = [
  { title: 'GENESIS', year: '2008-2009', hero: 'buster', boss: 'printer', blurb: 'Banks are on fire. A mystery coder writes nine pages. Block zero hatches a very confused Buster.',
    beats: [
      { talk: { bg: 'void', actors: [['arcade', 240, 1]], lines: [
        ['arcade', 'WELCOME, PLAYER! I AM ARCADE, YOUR HOST. TONIGHT: THE TRUE-ISH HISTORY OF UTXO CHAINS!'],
        ['narr', 'OCTOBER 2008. THE BANKS ARE ON FIRE. NOT LITERALLY. MOSTLY.'],
        ['arcade', 'SOMEONE CALLING THEMSELVES SATOSHI NAKAMOTO POSTS A NINE-PAGE PAPER. NOBODY KNOWS WHO THAT IS. STILL.', { enter: ['satoshi', 360, -1] }],
        ['satoshi', '"A PURELY PEER-TO-PEER VERSION OF ELECTRONIC CASH." ...THAT IS ALL I WILL SAY.'],
        ['narr', 'JANUARY 3, 2009. BLOCK ZERO IS MINED. INSIDE IT: A NEWSPAPER HEADLINE.', { bg: 'news', exit: 'satoshi' }],
        ['arcade', 'A TIMESTAMP AND A MESSAGE. AND OUT OF THAT BLOCK HATCHES...'],
        ['buster', 'WHERE AM I? WHY IS EVERYTHING ON FIRE? WHY DO I HAVE 50 COINS?', { bg: 'genesis', enter: ['buster', 180, 1], flash: true, sfx: 'go' }],
        ['arcade', 'THAT IS YOUR BLOCK REWARD! EVERY 210,000 BLOCKS IT HALVES. REMEMBER THAT MOVE. IT IS YOUR SPECIAL.'],
        ['printer', 'BRRRRRRRRRRR.', { enter: ['printer', 360, -1, 'idle', 0.9], shake: 6, music: 'boss' }],
        ['buster', 'WHAT. IS. THAT.'],
        ['arcade', 'THE MONEY PRINTER! WHEN IT FEELS SAD, IT PRINTS MORE MONEY. IT IS ALWAYS SAD.'],
        ['printer', 'BRRR? BRRRRR! (TRANSLATION: NOBODY NEEDS A FIXED SUPPLY.)'],
        ['buster', '21 MILLION. NOT ONE MORE. COME HERE.'],
      ] } },
      { fight: { stage: 'genesis', hero: 'buster', foes: [{ boss: 'printer' }], goal: 'DEFEAT THE MONEY PRINTER', hint: 'MOVE A/D · JUMP W · ATTACK C · SPECIAL V · SHIELD B   (ARROWS + , . / WORK TOO)' } },
      { talk: { bg: 'genesis', actors: [['buster', 200, 1, 'win']], lines: [
        ['narr', 'THE PRINTER RAN OUT OF TONER. FOR NOW.'],
        ['narr', 'JANUARY 2009. A CYPHERPUNK NAMED HAL FINNEY DOWNLOADS THE SOFTWARE ON DAY ONE.', { bg: 'terminal' }],
        ['term', '> RUNNING BITCOIN', { term: '> RUNNING BITCOIN' }],
        ['narr', 'DAYS LATER, SATOSHI SENDS HAL 10 BTC. THE FIRST BITCOIN TRANSACTION EVER. THANK YOU, HAL.', { term: '> RECEIVED 10.00 BTC' }],
        ['buster', '...THANKS, HAL.', { bg: 'genesis', music: 'hope' }],
        ['arcade', 'NEXT TIME: SOMEBODY GETS VERY, VERY HUNGRY.'],
      ] } },
    ] },
  { title: 'PIZZA DAY', year: '2010', hero: 'buster', blurb: 'Bitcoin has never bought anything. It is a very shy money. Then a hungry dev gets an idea.',
    beats: [
      { talk: { bg: 'beach', actors: [['buster', 160, 1], ['pizza', 320, -1]], lines: [
        ['narr', 'MAY 2010. BITCOIN HAS NEVER BOUGHT ANYTHING. IT IS A VERY SHY MONEY.'],
        ['pizza', 'I WILL PAY 10,000 BITCOIN FOR TWO PIZZAS. ANYONE? HELLO? IS THIS THING ON?'],
        ['buster', '10,000?! THAT IS... ABOUT $41 RIGHT NOW. HONESTLY, A FAIR DEAL.'],
        ['arcade', 'HELP THE PIZZA HAPPEN! HIT ANY BUTTON WHEN THE RINGS CLOSE ON THE MARKERS!', { enter: ['arcade', 60, 1] }],
      ] } },
      { cheer: 'pizza' },
      { talk: { bg: 'beach', actors: [['buster', 160, 1, 'win'], ['pizza', 320, -1, 'pizza']], lines: [
        ['pizza', 'DELICIOUS. NO REGRETS.'],
        ['buster', 'WAIT. HOW MUCH WOULD THOSE COINS BE WORTH TODAY?'],
        ['arcade', 'DO NOT DO THE MATH. NEVER DO THE MATH. JUST ENJOY THE PIZZA.', { enter: ['arcade', 60, 1] }],
        ['arcade', 'NEXT TIME: A BUG WITH A LOT OF ZEROS.'],
      ] } },
    ] },
  { title: 'THE OVERFLOW', year: '2010', hero: 'buster', boss: 'overflow', blurb: 'Block 74,638. A bug lets someone create 184 billion coins out of thin air. Oops.',
    beats: [
      { talk: { bg: 'genesis', actors: [['buster', 150, 1]], lines: [
        ['narr', 'AUGUST 15, 2010. BLOCK 74,638.'],
        ['narr', 'A BUG LETS SOMEONE CREATE 184,467,440,737 BITCOIN. IN ONE TRANSACTION. OOPS.', { shake: 8, music: 'boss' }],
        ['overflow', 'HELLO. I AM 184 BILLION COINS. I WOULD LIKE TO BE YOUR ECONOMY NOW.', { enter: ['overflow', 340, -1, 'idle', 0.9] }],
        ['buster', 'THERE ARE ONLY SUPPOSED TO BE 21 MILLION OF ME!'],
        ['arcade', 'THE DEVS ARE WRITING A PATCH! HOLD THE BLOB OFF UNTIL THE NEW VERSION SHIPS!'],
      ] } },
      { fight: { stage: 'genesis', hero: 'buster', foes: [{ boss: 'overflow' }], goal: 'HOLD ON UNTIL THE PATCH SHIPS (OR POP THE BLOB)', time: 100, survive: true, clock: 'PATCH ETA' } },
      { talk: { bg: 'genesis', actors: [['buster', 200, 1, 'win']], lines: [
        ['narr', 'ABOUT FIVE HOURS LATER, A FIX IS OUT. NODES UPGRADE. THE BAD BLOCK GETS LEFT BEHIND.'],
        ['buster', 'LESSON: RUN A NODE. CHECK THE MATH.'],
        ['narr', 'DECEMBER 2010. SATOSHI POSTS FOR THE LAST TIME. BY SPRING 2011: "I HAVE MOVED ON TO OTHER THINGS."', { enter: ['satoshi', 360, -1] }],
        ['buster', 'WAIT. WHO IS IN CHARGE NOW?', { exit: 'satoshi' }],
        ['arcade', 'EVERYONE. THAT IS THE POINT.', { enter: ['arcade', 340, -1], music: 'hope' }],
      ] } },
    ] },
  { title: 'SILVER ARRIVES', year: '2011', hero: 'lester', blurb: 'A quiet engineer has an idea: what if Bitcoin had a faster little sibling? Featuring Charlie Lee.',
    beats: [
      { talk: { bg: 'room', actors: [['charlie', 100, 1, 'type']], lines: [
        ['narr', 'OCTOBER 2011. A QUIET ENGINEER HAS AN IDEA.'],
        ['charlie', 'WHAT IF BITCOIN HAD A LITTLE SIBLING? FASTER BLOCKS. A DIFFERENT MINING RECIPE.'],
        ['charlie', 'SCRYPT INSTEAD OF SHA-256. A BLOCK EVERY 2.5 MINUTES. 84 MILLION COINS. AND...'],
        ['narr', 'HE HITS ENTER.', { set: { glow: true }, flash: true, sfx: 'beam' }],
        ['lester', 'HELLO WORLD! WAIT. AM I SHINY? I AM SHINY!', { enter: ['lester', 220, -1], pose: [['charlie', 'idle']], music: 'hope' }],
        ['charlie', 'SILVER TO BITCOIN\'S GOLD. HERE, YOUR FIRST COIN. TOSS IT AT PROBLEMS.', { pose: [['charlie', 'flip']], sfx: 'coin' }],
        ['arcade', 'AND THAT IS HOW LESTER GOT HIS COIN TOSS!', { enter: ['arcade', 400, -1] }],
        ['buster', 'A COPY OF ME? SMALLER? FASTER? SHINIER? I DO NOT LIKE THIS.', { exit: 'arcade', enter: ['buster', 360, -1], pose: [['charlie', 'idle']] }],
        ['lester', 'NOT A COPY. A TEST PILOT. LET ME SHOW YOU.'],
      ] } },
      { fight: { stage: 'arcade', hero: 'lester', foes: [{ who: 'buster', level: 4, stocks: 2 }], goal: 'PROVE YOU ARE THE SILVER: BEAT BUSTER', hint: 'LESTER: V THROWS A COIN THAT COMES BACK · UP + V = SCRYPT SPIN' } },
      { talk: { bg: 'room', actors: [['charlie', 100, 1], ['lester', 220, 1], ['buster', 360, -1, 'hurt']], lines: [
        ['buster', 'OK. OK. YOU ARE FAST.'],
        ['lester', 'AND YOU ARE TOUGH. TEAM?', { pose: [['buster', 'idle']] }],
        ['charlie', 'GO HELP BUSTER. I WILL KEEP THE LIGHTS ON.', { pose: [['charlie', 'wave']] }],
        ['arcade', 'THE SILVER AND THE GOLD. THE BUDDY COP ERA BEGINS!', { enter: ['arcade', 440, -1] }],
      ] } },
    ] },
  { title: 'MUCH WOW', year: '2013', hero: 'shiba', boss: 'stuffer', blurb: 'Two devs make a coin as a joke. Then someone tries to steal Christmas. Wow.',
    beats: [
      { talk: { bg: 'rooftop', actors: [['devs', 120, 1]], lines: [
        ['narr', 'DECEMBER 2013. TWO DEVELOPERS DECIDE TO MAKE A COIN... AS A JOKE.'],
        ['devs', 'WHAT IF THE COIN WAS... THE DOG MEME? SO JOKE. MUCH FUNNY.', { pose: [['devs', 'laugh']] }],
        ['narr', 'THEY FORK LUCKYCOIN, WHICH FORKED LITECOIN. FAMILY TREES ARE WEIRD.'],
        ['shiba', 'WOW. SUCH BORN. VERY DOG.', { enter: ['shiba', 260, 1], pose: [['devs', 'idle']], sfx: 'go' }],
        ['lester', 'HEY, I THINK YOU ARE MY... GRAND-NEPHEW?', { enter: ['lester', 400, -1] }],
        ['narr', 'CHRISTMAS 2013. A WALLET SERVICE GETS ROBBED. PEOPLE\'S DOGE, GONE OVERNIGHT.', { exit: 'lester', music: 'boss' }],
        ['stuffer', 'HO HO HODL... MINE NOW! I STUFF STOCKINGS. WITH YOUR COINS.', { enter: ['stuffer', 400, -1, 'idle', 1], exit: 'devs', shake: 5 }],
        ['shiba', 'NOT ON DOGEMAS.'],
      ] } },
      { fight: { stage: 'rooftop', hero: 'shiba', foes: [{ boss: 'stuffer' }], goal: 'STOP THE STOCKING STUFFER', weather: 'snow', hint: 'SHIBA: V = MOON DASH · UP + V = TO THE MOON. SHIBA IS LIGHT: DO NOT GET HIT AT HIGH %' } },
      { talk: { bg: 'rooftop', actors: [['shiba', 200, 1, 'win']], lines: [
        ['narr', 'THE COINS ARE STILL MISSING. BUT THE COMMUNITY HAS AN IDEA.'],
        ['arcade', 'EVERYBODY CHIP IN! KEEP THE BEAT AND REFUND THE STOCKINGS!', { enter: ['arcade', 360, -1] }],
      ] } },
      { cheer: 'dogemas' },
      { talk: { bg: 'beach', actors: [['shiba', 200, 1, 'win']], lines: [
        ['narr', 'THEN THE DOGE COMMUNITY GETS WEIRD. IN A GOOD WAY.'],
        ['narr', '2014: THEY SEND A JAMAICAN BOBSLED TEAM TO THE OLYMPICS. THEY FUND WATER WELLS IN KENYA. THEY PUT A DOG ON A NASCAR CAR.'],
        ['shiba', 'DO ONLY GOOD EVERYDAY. ALSO MUCH SNACKS.'],
      ] } },
    ] },
  { title: 'HASHRATE PANIC', year: '2014', hero: 'shiba', boss: 'vacuum', blurb: 'Big mining pools hop in and out of Doge. Blocks go fast, then slow. Digi and Lester to the rescue.',
    beats: [
      { talk: { bg: 'arcade', actors: [['shiba', 160, 1]], lines: [
        ['narr', '2014. BIG MINING POOLS HOP ONTO DOGECOIN, MINE A PILE OF EASY BLOCKS, AND HOP OFF AGAIN.'],
        ['shiba', 'BLOCKS GO FAST. THEN SLOW. THEN FAST. MUCH DIZZY.'],
        ['vacuum', 'VWOOOOM. YOUR HASHRATE LOOKS DELICIOUS. I HOP IN, I HOP OUT.', { enter: ['vacuum', 380, -1, 'idle', 0.9], music: 'boss', shake: 5 }],
        ['digi', 'STAND BACK. I AM DIGI, FROM DIGIBYTE. I BROUGHT DIGISHIELD: DIFFICULTY THAT ADJUSTS EVERY SINGLE BLOCK.', { enter: ['digi', 60, 1] }],
        ['narr', 'DOGECOIN ADOPTS DIGISHIELD. THE WOBBLE CALMS DOWN.'],
        ['lester', 'AND I HAVE AN IDEA TOO. MERGED MINING! MY MINERS CAN SECURE YOUR CHAIN AT THE SAME TIME.', { enter: ['lester', 260, 1] }],
        ['shiba', 'TWO CHAINS, ONE HASH? WOW.'],
        ['arcade', 'TEAM-UP TIME! THREE CHAINS VERSUS ONE VERY RUDE VACUUM!'],
      ] } },
      { fight: { stage: 'arcade', hero: 'shiba', allies: [['digi', 6], ['lester', 6]], foes: [{ boss: 'vacuum' }], goal: 'TEAM UP: BEAT THE HASHRATE VACUUM', hint: 'WHEN IT SUCKS, RUN THE OTHER WAY. IT BLOWS EVERYONE AWAY AT THE END.' } },
      { talk: { bg: 'arcade', actors: [['digi', 120, 1, 'win'], ['shiba', 240, 1, 'win'], ['lester', 360, -1, 'win']], lines: [
        ['narr', 'SEPTEMBER 2014, BLOCK 371,337: DOGECOIN STARTS MERGED MINING WITH LITECOIN. YES, 1337. NERDS.'],
        ['digi', 'LATER THAT YEAR I SPLIT MY MINING INTO FIVE ALGORITHMS. NO SINGLE KIND OF MACHINE RUNS ME.'],
        ['shiba', 'BEST FRIENDS. VERY SECURE. WOW.'],
      ] } },
    ] },
  { title: 'GOXZILLA', year: '2014', hero: 'buster', boss: 'goxzilla', blurb: 'The biggest exchange in the world started as a trading-card site. Then the coins vanish.',
    beats: [
      { talk: { bg: 'rooftop', actors: [['buster', 140, 1], ['lester', 200, 1]], lines: [
        ['narr', 'MEANWHILE... THE BIGGEST BITCOIN EXCHANGE IN THE WORLD STARTED LIFE AS A TRADING-CARD WEBSITE.'],
        ['arcade', 'TRUE STORY. THE NAME STOOD FOR "MAGIC: THE GATHERING ONLINE EXCHANGE."', { enter: ['arcade', 60, 1] }],
        ['narr', 'FEBRUARY 2014. WITHDRAWALS STOP. ABOUT 850,000 BTC ARE GONE.', { exit: 'arcade', music: 'boss', shake: 6 }],
        ['goxzilla', 'ROOOAR! YOUR COINS? I ATE THEM. THEY WERE IN MY TUMMY THE WHOLE TIME.', { enter: ['goxzilla', 380, -1, 'idle', 0.9], shake: 8 }],
        ['buster', 'PEOPLE TRUSTED YOU WITH THEIR KEYS!'],
        ['lester', 'WHICH IS EXACTLY THE LESSON. ALL TOGETHER NOW...'],
        ['crowd', 'NOT YOUR KEYS, NOT YOUR COINS!', { enter: ['crowd', 290, 1] }],
      ] } },
      { fight: { stage: 'rooftop', hero: 'buster', allies: [['lester', 6]], foes: [{ boss: 'goxzilla' }], goal: 'TAKE DOWN GOXZILLA', hint: 'A RED LINE MEANS A BEAM IS COMING: JUMP OVER IT OR SHIELD (B).' } },
      { talk: { bg: 'rooftop', actors: [['buster', 160, 1, 'win'], ['lester', 230, 1, 'win']], lines: [
        ['goxzilla', 'BURP. (SOME COINS FALL OUT.)', { enter: ['goxzilla', 390, -1, 'idle', 0.7] }],
        ['narr', 'YEARS LATER, SOME OF THE COINS ARE FOUND, AND CREDITORS FINALLY START GETTING PAID BACK. A DECADE LATER.', { exit: 'goxzilla', music: 'hope' }],
        ['buster', 'HOLD YOUR OWN KEYS, KIDS.'],
      ] } },
    ] },
  { title: 'THE BLOCKSIZE WARS', year: '2015-2017', hero: 'lester', boss: 'bigblock', blurb: 'Two years of yelling about one number. Litecoin flips a switch first. Charlie Lee returns.',
    beats: [
      { talk: { bg: 'arcade', actors: [['buster', 240, 1]], lines: [
        ['narr', '2015. THE BITCOIN COMMUNITY STARTS ARGUING ABOUT ONE NUMBER: HOW BIG A BLOCK SHOULD BE.'],
        ['bigblock', 'BIGGER BLOCKS! MORE TRANSACTIONS! CHEAP FEES FOR EVERYONE!', { enter: ['bigblock', 400, -1, 'idle', 0.8] }],
        ['smallblock', 'SMALLER BLOCKS! SO ANYONE CAN RUN A NODE! DECENTRALIZE!', { enter: ['smallblock', 80, 1, 'idle', 1] }],
        ['buster', 'CAN YOU BOTH PLEASE STOP YELLING IN MY MEMPOOL?'],
        ['narr', 'THE ARGUMENT LASTS TWO YEARS. TWO. YEARS.'],
        ['lester', 'WHAT IF SOMEONE JUST... TRIED THE UPGRADE FIRST?', { enter: ['lester', 300, -1] }],
        ['charlie', 'SEGREGATED WITNESS. IT FIXES A BUG AND MAKES ROOM IN BLOCKS. LITECOIN CAN TURN IT ON FIRST.', { bg: 'switch', exit: ['bigblock', 'buster'], enter: ['charlie', 250, 1] }],
        ['arcade', 'CHEER HIM ON! EVERY HIT GETS THE MINERS SIGNALING!', { exit: 'smallblock', enter: ['arcade', 80, 1] }],
      ] } },
      { cheer: 'segwit' },
      { talk: { bg: 'switch', signal: 0.9, actors: [['charlie', 250, 1, 'wave'], ['lester', 130, 1, 'win']], lines: [
        ['narr', 'MAY 2017. LITECOIN ACTIVATES SEGWIT. NOTHING EXPLODES.', { set: { switchOn: true } }],
        ['crowd', 'TEST PILOT! TEST PILOT!', { enter: ['crowd', 420, -1] }],
        ['lester', 'SEE? IT WORKS. NOW LET\'S GO END THAT ARGUMENT.', { music: 'boss' }],
      ] } },
      { fight: { stage: 'arcade', hero: 'lester', allies: [['buster', 6]], foes: [{ boss: 'bigblock', x: 320 }, { boss: 'smallblock', x: 360 }], goal: 'CALM DOWN THE BLOCKSIZE TWINS', hint: 'TWO BOSSES. THE SMALL ONE IS FAST, THE BIG ONE HITS HARD.' } },
      { talk: { bg: 'arcade', actors: [['lester', 160, 1, 'win'], ['buster', 230, 1, 'win']], lines: [
        ['narr', 'AUGUST 2017. BITCOIN ACTIVATES SEGWIT TOO. THE BIG-BLOCK CROWD FORKS OFF AND STARTS ITS OWN CHAIN.'],
        ['bigblock', 'FINE! I WILL START MY OWN CHAIN! WITH HUGE BLOCKS!', { enter: ['bigblock', 400, -1, 'idle', 0.7] }],
        ['smallblock', 'GOOD! ...WAIT. WHO DO I ARGUE WITH NOW?', { enter: ['smallblock', 340, -1, 'idle', 1] }],
        ['arcade', 'BOTH SIDES THOUGHT THEY WERE SAVING BITCOIN. THAT IS THE FUNNY PART. AND THE SAD PART.', { enter: ['arcade', 60, 1], music: 'hope' }],
      ] } },
    ] },
  { title: 'TO THE MOON', year: '2021', hero: 'shiba', boss: 'tornado', blurb: 'Everyone is online and bored. Everyone discovers the dog coin. A tornado of hype arrives.',
    beats: [
      { talk: { bg: 'moon', actors: [['shiba', 180, 1]], lines: [
        ['narr', '2021. EVERYONE IS ONLINE. EVERYONE IS BORED. EVERYONE DISCOVERS THE DOG COIN.'],
        ['shiba', 'WHY IS EVERYONE LOOKING AT ME? WHY AM I ON THE MOON?'],
        ['tornado', 'HYYYYPE! ROCKET EMOJIS! TO THE MOOOOON!', { enter: ['tornado', 370, -1, 'idle', 0.7], music: 'boss', shake: 6 }],
        ['arcade', 'THE HYPE TORNADO! IT LIFTS EVERYTHING UP... AND DROPS IT FROM VERY HIGH.', { enter: ['arcade', 60, 1] }],
        ['digi', 'I WILL COVER YOU. FIVE ALGOS, ZERO HYPE.', { exit: 'arcade', enter: ['digi', 100, 1] }],
        ['shiba', 'I DO NOT WANT TO BE A ROCKET. I JUST WANT TO TIP PEOPLE.'],
      ] } },
      { fight: { stage: 'moon', hero: 'shiba', allies: [['digi', 6]], foes: [{ boss: 'tornado' }], goal: 'SURVIVE THE HYPE TORNADO', hint: 'V MARKS THE SPOT: ROCKETS FALL WHERE YOU SEE THEM.' } },
      { talk: { bg: 'moon', actors: [['shiba', 200, 1, 'win'], ['digi', 120, 1]], lines: [
        ['narr', 'MAY 2021. A ROCKET BILLIONAIRE MAKES A JOKE ABOUT THE DOG COIN ON A TV COMEDY SHOW. THE PRICE FALLS OFF A CLIFF.'],
        ['shiba', 'PRICE UP. PRICE DOWN. STILL A DOG. STILL MUCH WOW.'],
        ['arcade', 'REMEMBER, KIDS: NOTHING IN THIS GAME IS FINANCIAL ADVICE. ESPECIALLY THE TORNADO.', { enter: ['arcade', 360, -1], music: 'hope' }],
      ] } },
    ] },
  { title: 'INSCRIPTIONS', year: '2021-2024', hero: 'pep3d', boss: 'mempool', blurb: 'Taproot. Pictures written into blocks. Then, in January 2024, a frog gets a chain of its own, with its own genesis headline.',
    beats: [
      { talk: { bg: 'beach', actors: [['shiba', 120, 1]], lines: [
        ['narr', 'NOVEMBER 2021: BITCOIN GETS TAPROOT. JANUARY 2023: SOMEONE REALIZES YOU CAN WRITE PICTURES INTO IT.'],
        ['arcade', 'ORDINALS! INSCRIPTIONS! ART, TEXT AND GAMES, WRITTEN STRAIGHT INTO THE CHAIN. FOREVER!', { enter: ['arcade', 380, -1] }],
        ['narr', 'IT SPREADS FAST: DOGINALS ON DOGECOIN, LITECOIN ORDINALS, BELLINALS ON BELLSCOIN.'],
        ['narr', 'JANUARY 2024. A BRAND-NEW GENESIS BLOCK. AND INSIDE IT... A NEWSPAPER HEADLINE.', { bg: 'news', exit: ['arcade', 'shiba'], set: { news: { date: '22 JANUARY 2024', lines: ['FED REVIEW CLEARS', 'CENTRAL BANK OFFICIALS', 'OF VIOLATING RULES'] } } }],
        ['buster', 'HEY! A HEADLINE IN BLOCK ZERO? THAT IS MY MOVE!', { enter: ['buster', 60, 1] }],
        ['narr', 'JANUARY 30, 2024. MINING OPENS FOR EVERYONE. A FORK OF DOGECOIN: SCRYPT, ONE BLOCK A MINUTE.', { bg: 'beach', exit: 'buster' }],
        ['pep3d', 'HI. I AM PEP, FROM PEPECOIN. MADE OF MEMES AND PROOF OF WORK.', { enter: ['pep3d', 260, 1], sfx: 'go' }],
        ['shiba', 'A FORK OF ME? SO FAMILY. MUCH FROG.', { enter: ['shiba', 120, 1] }],
        ['pep3d', '500,000 PEP PER BLOCK AT FIRST. IT HALVES EVERY 100,000 BLOCKS, THEN STAYS AT 10,000 FOREVER. JUST LIKE YOU, SHIBA.'],
        ['mempool', 'BLOOOORP. SO MANY TRANSACTIONS. I AM FULL. FEES ARE NOW ONE MILLION.', { enter: ['mempool', 390, -1, 'idle', 0.8], music: 'boss', shake: 6 }],
        ['shiba', 'THE MEMPOOL MONSTER! IT EATS BLOCK SPACE FOR BREAKFAST!'],
      ] } },
      { fight: { stage: 'beach', hero: 'pep3d', allies: [['shiba', 6]], foes: [{ boss: 'mempool' }], goal: 'CLEAR THE MEMPOOL MONSTER', hint: 'PEP: THREE JUMPS · V = TONGUE LASH (PULLS ENEMIES IN) · UP + V = FROG LEAP' } },
      { talk: { bg: 'beach', actors: [['pep3d', 220, 1, 'win'], ['shiba', 140, 1, 'win']], lines: [
        ['mempool', 'BLORP... FEES... RETURNING... TO NORMAL...', { enter: ['mempool', 390, -1, 'idle', 0.6] }],
        ['narr', 'MAY 17, 2024. BLOCK 42,034: PEPECOIN MINES ITS FIRST MERGED BLOCK. LITECOIN AND DOGECOIN MINERS NOW GUARD THE FROG TOO.', { exit: 'mempool', music: 'hope' }],
        ['lester', 'THE MERGED MINING CLUB GROWS. WELCOME, FROG.', { enter: ['lester', 330, -1, 'win'] }],
        ['pep3d', 'ART ON THE CHAIN. FRIENDS ON THE HASH. FEELS GOOD, MAN.'],
        ['arcade', 'NEXT: THE FINAL CHAPTER. AND THE ONE ENEMY THAT NEVER WENT AWAY.', { enter: ['arcade', 420, -1] }],
      ] } },
    ] },
  { title: 'MANY NODES', year: 'TODAY', hero: 'arcade', boss: 'middleman', blurb: 'The chains grew up. One enemy never left: the guy who wants to hold your keys for you.',
    beats: [
      { talk: { bg: 'arcade', actors: [['arcade', 120, 1], ['buster', 200, 1], ['pep3d', 60, 1]], lines: [
        ['narr', 'TODAY. THE CHAINS ARE ALL GROWN UP. BUT ONE ENEMY NEVER WENT AWAY.'],
        ['middleman', 'HELLO! I AM THE MIDDLEMAN. JUST HAND ME YOUR KEYS. I WILL HOLD THEM FOR YOU. TRUST ME.', { enter: ['middleman', 380, -1, 'idle', 1], music: 'boss' }],
        ['buster', 'WE KNOW YOU. YOU WERE THE PRINTER. AND THE EXCHANGE MONSTER. AND THE FEES.'],
        ['middleman', 'I PREFER "TRUSTED THIRD PARTY." NOW, ABOUT YOUR SMALL MONTHLY FEE...'],
        ['arcade', 'THIS ONE IS MINE. I AM NOT JUST THE HOST... I AM A NODE. AND SO IS EVERYONE HERE.'],
        ['crowd', 'WE RUN NODES!', { enter: ['crowd', 290, 1] }],
        ['arcade', 'INSERT COIN!', { flash: true, sfx: 'coin' }],
      ] } },
      { fight: { stage: 'arcade', hero: 'arcade', allies: [['buster', 7], ['pep3d', 7]], foes: [{ boss: 'middleman' }], goal: 'BEAT THE MIDDLEMAN', crowd: ['lester', 'shiba', 'digi', 'charlie'], hint: 'ARCADE: V THROWS TOKENS · UP + V = JOYSTICK SPRING. AT HALF HP HE GETS BIGGER.' } },
      { talk: { bg: 'arcade', actors: [['arcade', 200, 1, 'win'], ['buster', 130, 1, 'win'], ['pep3d', 270, 1, 'win'], ['charlie', 420, -1]], lines: [
        ['middleman', 'NOOO! YOU CANNOT JUST... VERIFY THINGS YOURSELVES!', { enter: ['middleman', 350, -1, 'idle', 0.8] }],
        ['arcade', 'WE CAN. EVERY NODE CHECKS EVERY BLOCK. NO MIDDLEMAN NEEDED.', { exit: 'middleman', music: 'hope' }],
        ['charlie', 'NICE WORK, EVERYONE. THE LIGHTS ARE STILL ON.', { pose: [['charlie', 'wave']] }],
        ['lester', 'TEST PILOT REPORTING FOR DUTY!', { enter: ['lester', 340, -1, 'win'] }],
        ['shiba', 'WOW. MANY NODES. SUCH FRIENDS.', { enter: ['shiba', 60, 1, 'win'] }],
        ['digi', 'FIVE ALGOS AND ONE BIG FAMILY.', { enter: ['digi', 20, 1, 'win'] }],
        ['arcade', 'THE STORY IS NOT OVER. IT IS WRITTEN ONE BLOCK AT A TIME. MAYBE BY YOU.'],
      ] } },
    ] },
];
