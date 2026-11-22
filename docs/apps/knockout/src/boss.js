// boss.js - story-mode bosses: big drawn enemies with an HP bar, telegraphed attack patterns, and a rage phase at half HP.
'use strict';
// ---- projectile kinds bosses throw ------------------------------------------------------------------------------------
function ob(g, x, y, w, h, fill, ol = '#160e1e') { rect(g, x - 1, y - 1, w + 2, h + 2, ol); rect(g, x, y, w, h, fill); }
// text inside boss art: the art is mirrored when a boss faces left, so mirror the text once more to keep it readable
function bt(g, b, s, x, y, o = {}) {
  if (o.align !== 'center') { x += textW(s, o.size || 1) / 2; o = { ...o, align: 'center' }; }
  if (b.face < 0) { g.save(); g.scale(-1, 1); text(s, -x, y, { ...o, g }); g.restore(); } else text(s, x, y, { ...o, g });
}
const PTYPES = {
  bill:  { w: 12, h: 8, dmg: 6, kb: 2.4, kbg: 2.6, ang: 40, g: 0.16, life: 140, draw(p, x, y) { ob(ctx, x - 6, y - 3, 12, 7, '#7ad87a'); rect(ctx, x - 1, y - 2, 2, 5, '#2a6a2a'); } },
  digit: { w: 10, h: 10, dmg: 6, kb: 2.6, kbg: 2.4, ang: 45, life: 120, init(p) { p.ch = pick('1844674407'.split('')); }, draw(p, x, y) { text(p.ch, x, y - 7, { size: 2, align: 'center', color: '#b8ff6a', outline: '#160e1e' }); } },
  coal:  { w: 9, h: 9, dmg: 7, kb: 2.6, kbg: 2.8, ang: 45, g: 0.2, life: 140, bounce: true, draw(p, x, y) { ob(ctx, x - 4, y - 4, 8, 8, '#2a2a34'); rect(ctx, x - 2, y - 2, 2, 2, '#5a5a6a'); } },
  gift:  { w: 12, h: 12, dmg: 8, kb: 2.8, kbg: 3.0, ang: 60, g: 0.12, life: 160, draw(p, x, y) { ob(ctx, x - 6, y - 6, 12, 12, '#ff4a5a'); rect(ctx, x - 1, y - 6, 2, 12, '#ffd23a'); rect(ctx, x - 6, y - 1, 12, 2, '#ffd23a'); } },
  dust:  { w: 12, h: 10, dmg: 5, kb: 2.2, kbg: 2.2, ang: 40, life: 110, draw(p, x, y) { const s = (frameNo >> 3) % 2; ob(ctx, x - 6 + s, y - 4, 11, 9, '#9a90a8'); rect(ctx, x - 3, y - 2, 2, 2, '#160e1e'); rect(ctx, x + 1, y - 2, 2, 2, '#160e1e'); } },
  card:  { w: 10, h: 13, dmg: 7, kb: 2.6, kbg: 2.8, ang: 35, life: 120, draw(p, x, y) { const w = [5, 3, 1, 3][(frameNo >> 2) % 4]; ob(ctx, x - w, y - 6, w * 2, 13, '#f4f0e0'); if (w > 2) rect(ctx, x - 2, y - 3, 4, 6, '#ff4a5a'); } },
  rocket:{ w: 10, h: 16, dmg: 8, kb: 3.0, kbg: 3.0, ang: 70, g: 0.1, life: 160, draw(p, x, y) { ob(ctx, x - 3, y - 8, 6, 12, '#e8e8f4'); rect(ctx, x - 2, y - 10, 4, 3, '#ff4a5a'); rect(ctx, x - 5, y + 1, 10, 3, '#ff4a5a'); rect(ctx, x - 2, y + 5, 4, 3 + (frameNo >> 1) % 3, '#ffd23a'); } },
  cash:  { w: 10, h: 10, dmg: 5, kb: 2.4, kbg: 2.2, ang: 50, life: 110, draw(p, x, y) { text('$', x, y - 7, { size: 2, align: 'center', color: '#5aff8a', outline: '#160e1e' }); } },
  spam:  { w: 12, h: 12, dmg: 7, kb: 2.8, kbg: 2.8, ang: 55, g: 0.14, life: 160, bounce: true, init(p) { p.col = pick(['#ff5fd2', '#3ad0f0', '#ffd23a', '#5aff8a', '#ff8a3a']); }, draw(p, x, y) { ob(ctx, x - 6, y - 6, 12, 12, p.col); rect(ctx, x - 4, y - 3, 8, 1, '#160e1e'); rect(ctx, x - 4, y, 6, 1, '#160e1e'); } },
  fee:   { w: 10, h: 10, dmg: 6, kb: 2.6, kbg: 2.6, ang: 40, life: 120, draw(p, x, y) { const w = [5, 3, 1, 3][(frameNo >> 2) % 4]; ob(ctx, x - w, y - 5, w * 2, 10, '#ffd23a'); if (w > 2) text('F', x, y - 3, { align: 'center', color: '#7a4a1a', shadow: false }); } },
  form:  { w: 12, h: 14, dmg: 6, kb: 2.4, kbg: 2.6, ang: 45, life: 200, home: 0.08, draw(p, x, y) { ob(ctx, x - 6, y - 7, 12, 14, '#f4f4ec'); for (let i = 0; i < 4; i++) rect(ctx, x - 4, y - 5 + i * 3, 8 - (i % 2) * 3, 1, '#5a5a6a'); } },
  goo:   { w: 12, h: 10, dmg: 6, kb: 2.4, kbg: 2.4, ang: 45, g: 0.18, life: 140, bounce: true, draw(p, x, y) { ob(ctx, x - 5, y - 4, 10, 8, '#8aff5a'); rect(ctx, x - 2, y - 2, 2, 2, '#ffffff'); } },
};
function mkProj(b, type, x, y, vx, vy, A = {}) {
  const P = PTYPES[type], k = b.dmgMul;
  const p = { owner: b, type, x, y, vx, vy, w: P.w, h: P.h, dmg: Math.max(1, Math.round((A.dmg || P.dmg) * k)), kb: P.kb, kbg: P.kbg * (0.5 + 0.5 * k), ang: P.ang,
    g: A.g ?? P.g, life: P.life, bounce: P.bounce, home: P.home, draw: P.draw, hits: new Set() };
  if (P.init) P.init(p);
  return p;
}

// ---- the bosses ------------------------------------------------------------------------------------------------------
// draw(g, b, t): pixel art with the feet at (0, 0), facing right. b.pose: idle | wind | shoot | charge | air | land | swipe | pull | beam
const BOSSES = {
  printer: {
    name: 'THE MONEY PRINTER', color: '#ff5a5a', hp: 170, w: 64, h: 60, speed: 0.7, cd: 64, keep: 70,
    attacks: [{ k: 'shoot', proj: 'bill', n: 5, every: 6, arc: true, speed: 4.5, far: true }, { k: 'slam', h: 8 }, { k: 'charge', speed: 3.6, dmg: 10, near: true }, { k: 'rain', proj: 'bill', n: 9, every: 8, far: true }],
    say: ['BRRRR!', 'BRRR BRRR!', 'PRINTING...', 'PC LOAD LETTER?'],
    draw(g, b, t) {
      const p = b.pose, bob = p === 'idle' ? (t >> 3) % 2 : p === 'wind' ? (t % 4 < 2 ? -1 : 1) : 0, step = (t >> 3) % 2;
      ob(g, -26, -8 - (step ? 2 : 0), 12, 8, '#4a4a5a'); ob(g, 14, -8 - (step ? 0 : 2), 12, 8, '#4a4a5a');
      ob(g, -24, -72 + bob, 44, 12, '#f4f4ec'); rect(g, -20, -70 + bob, 36, 1, '#c8c8c0');
      ob(g, -32, -60 + bob, 64, 52, '#c8c8d4'); rect(g, -32, -60 + bob, 64, 6, '#e0e0ea'); rect(g, -32, -14 + bob, 64, 6, '#9a9aaa');
      const eye = b.rage ? '#ff3a4a' : '#5aff8a';
      ob(g, -16, -48 + bob, 12, 8, '#160e1e'); ob(g, 8, -48 + bob, 12, 8, '#160e1e');
      rect(g, -14, -46 + bob, 8, 4, eye); rect(g, 10, -46 + bob, 8, 4, eye);
      rect(g, -18, -52 + bob, 14, 2, '#160e1e'); rect(g, 6, -52 + bob, 14, 2, '#160e1e');
      ob(g, -24, -28 + bob, 48, 7, '#2a2a34');
      if (p === 'shoot' || p === 'charge') { rect(g, -18 + (t % 6), -26 + bob, 30, 4, '#7ad87a'); }
      bt(g, b, 'BRRR', 0, -38 + bob, { align: 'center', color: '#5a5a6a', shadow: false, g });
      rect(g, 22, -56 + bob, 4, 4, (t >> 4) % 2 ? '#ffd23a' : '#7a6a2a');
    },
  },
  overflow: {
    name: 'THE OVERFLOW BLOB', color: '#8aff5a', hp: 200, w: 76, h: 50, speed: 0.8, cd: 58, keep: 40,
    attacks: [{ k: 'slam', h: 9 }, { k: 'shoot', proj: 'digit', n: 6, every: 7, speed: 3.2, spread: 1.2, far: true }, { k: 'charge', speed: 3.4, dmg: 9, near: true }, { k: 'shoot', proj: 'goo', n: 4, every: 8, arc: true, speed: 4 }],
    say: ['BLORP.', '184 BILLION!', 'I AM THE ECONOMY NOW.', 'SO MANY ZEROS.'],
    draw(g, b, t) {
      const wob = Math.sin(t / 8) * 3 * (b.pose === 'wind' ? 2 : 1), w = 76 + wob, h = 50 - wob;
      for (let y = 0; y < h; y++) { const hw = w / 2 * Math.sqrt(1 - Math.pow(1 - y / h, 2)); rect(g, -hw - 1, -y - 1, hw * 2 + 2, 2, '#160e1e'); }
      for (let y = 0; y < h - 1; y++) { const hw = w / 2 * Math.sqrt(1 - Math.pow(1 - y / h, 2)) - 1; rect(g, -hw, -y - 1, hw * 2, 1, y > h * 0.7 ? '#b8ff8a' : y < 6 ? '#4ac83a' : '#7aef5a'); }
      g.save(); g.beginPath(); g.rect(-w / 2 + 8, -h * 0.45, w - 16, 9); g.clip();
      bt(g, b, '184467440737.09', -w / 2 + 8 - (t % 96), -h * 0.45 + 1, { color: '#2a6a2a', shadow: false, g });
      bt(g, b, '184467440737.09', -w / 2 + 8 - (t % 96) + 96, -h * 0.45 + 1, { color: '#2a6a2a', shadow: false, g }); g.restore();
      const ey = -h * 0.72; ob(g, 6, ey, 7, 9, '#ffffff'); ob(g, 18, ey, 7, 9, '#ffffff'); rect(g, 9, ey + 3, 3, 4, '#160e1e'); rect(g, 21, ey + 3, 3, 4, '#160e1e');
      if (b.rage) { rect(g, 4, ey - 3, 10, 2, '#160e1e'); rect(g, 17, ey - 3, 10, 2, '#160e1e'); }
    },
  },
  stuffer: {
    name: 'THE STOCKING STUFFER', color: '#ff4a5a', hp: 210, w: 30, h: 56, speed: 1.6, cd: 52, keep: 30,
    attacks: [{ k: 'charge', speed: 5.2, dmg: 9, near: true }, { k: 'shoot', proj: 'coal', n: 3, every: 10, arc: true, speed: 4.2, far: true }, { k: 'swipe', reach: 44, dmg: 12, near: true }, { k: 'rain', proj: 'gift', n: 7, every: 12 }],
    say: ['HO HO HODL... MINE!', 'NAUGHTY LIST!', 'THANKS FOR THE COINS!', 'NO REFUNDS!'],
    draw(g, b, t) {
      const p = b.pose, run = p === 'charge' ? (t >> 2) % 2 : 0;
      ob(g, -24, -50, 22, 34, '#c83a4a'); rect(g, -22, -50, 18, 5, '#f4f4ec'); rect(g, -18, -40, 6, 5, '#ffd23a'); rect(g, -12, -30, 5, 4, '#ffd23a');   // the sack (a giant stocking)
      ob(g, -8, -16, 6, 16 - run * 3, '#2a2a3a'); ob(g, 3, -16, 6, 16 - (1 - run) * 3, '#2a2a3a');
      ob(g, -10, -38, 20, 22, '#f4f4ec'); for (let y = -36; y < -16; y += 5) rect(g, -10, y, 20, 2, '#2a2a3a');
      ob(g, -9, -54, 18, 16, '#e8b890'); rect(g, -9, -49, 18, 5, '#160e1e'); rect(g, 1, -48, 3, 2, '#ffffff'); rect(g, 6, -48, 3, 2, '#ffffff');
      rect(g, -2, -42, 9, 2, '#7a3a2a');
      ob(g, -11, -60, 22, 7, '#d83a4a'); rect(g, -11, -56, 22, 3, '#f4f4ec'); ob(g, 12, -66, 5, 5, '#f4f4ec');
      const arm = p === 'swipe' ? -6 : p === 'wind' ? 4 : 0; ob(g, 8, -34 + arm, 12, 5, '#2a2a3a');
    },
  },
  vacuum: {
    name: 'THE HASHRATE VACUUM', color: '#c8a2ff', hp: 330, w: 54, h: 70, speed: 0.8, cd: 60, keep: 60,
    attacks: [{ k: 'pull', dur: 110, force: 1.15, range: 230 }, { k: 'charge', speed: 4.2, dmg: 11, near: true }, { k: 'shoot', proj: 'dust', n: 4, every: 9, speed: 3.6, spread: 0.6, far: true }, { k: 'slam', h: 7 }],
    say: ['VWOOOOM!', 'YOUR HASHRATE LOOKS DELICIOUS.', 'SLLLRRRP!', 'I HOP IN, I HOP OUT.'],
    draw(g, b, t) {
      const p = b.pose, puff = p === 'pull' ? 6 + Math.sin(t / 3) * 2 : 0;
      ob(g, -26, -12, 52, 12, '#4a4a5a'); ob(g, -22, -6, 8, 6, '#160e1e'); ob(g, 14, -6, 8, 6, '#160e1e');
      ob(g, 18, -10, 16, 8, '#5a5a6a');   // nozzle
      if (p === 'pull' && (t >> 1) % 2) for (let i = 0; i < 3; i++) rect(g, 40 + i * 10 + (t * 3 % 10), -8 + i * 2 - 4, 6, 1, '#ffffffaa');
      ob(g, -16 - puff / 2, -58 - puff, 30 + puff, 46 + puff, '#9a7ad8'); rect(g, -12 - puff / 2, -54 - puff, 6, 38 + puff, '#b89aff');
      bt(g, b, 'HASH', -1, -40 - puff / 2, { align: 'center', color: '#3a2a6a', shadow: false, g });
      ob(g, -4, -76, 6, 18, '#5a5a6a'); ob(g, -12, -80, 22, 6, '#2a2a34');
      const eye = b.rage ? '#ff3a4a' : '#ffffff'; ob(g, -8, -30, 6, 6, eye); ob(g, 4, -30, 6, 6, eye); rect(g, -6, -28, 3, 3, '#160e1e'); rect(g, 6, -28, 3, 3, '#160e1e');
    },
  },
  goxzilla: {
    name: 'GOXZILLA', color: '#5aff8a', hp: 350, w: 56, h: 92, speed: 0.6, cd: 66, keep: 60,
    attacks: [{ k: 'shoot', proj: 'card', n: 5, every: 7, speed: 4.2, spread: 0.8, far: true }, { k: 'beam', tele: 46, dur: 26, hy: 0.78, h: 16, dmg: 14 }, { k: 'slam', h: 6 }, { k: 'swipe', reach: 54, dmg: 13, near: true, hy: 0.3, hh: 22 }],
    say: ['ROOOAR!', 'YOUR COINS? I ATE THEM.', 'WITHDRAWALS... PAUSED.', 'TRUST ME, THEY ARE SAFE.'],
    draw(g, b, t) {
      const p = b.pose, sw = p === 'swipe' ? 10 : 0, jaw = p === 'beam' || p === 'shoot' ? 6 : p === 'wind' ? 3 : 0;
      ob(g, -44 - sw, -22, 30, 10, '#3a9a4a'); ob(g, -54 - sw, -16, 14, 6, '#3a9a4a');   // tail
      ob(g, -18, -18, 12, 18, '#2a7a3a'); ob(g, 6, -18, 12, 18, '#2a7a3a');
      ob(g, -22, -70, 40, 54, '#3aaa4a'); rect(g, -6, -62, 18, 44, '#a8e88a');
      for (let i = 0; i < 5; i++) ob(g, -26, -66 + i * 10, 6, 6, '#e8f4a8');
      ob(g, 14, -50, 12, 6, '#3aaa4a'); rect(g, 24, -48, 3, 2, '#f4f4ec');
      ob(g, -10, -92, 34, 22, '#3aaa4a'); ob(g, 4, -78 + jaw, 24, 6, '#2a7a3a'); rect(g, 6, -78, 18, 1 + jaw, '#160e1e');
      for (let x = 6; x < 24; x += 4) rect(g, x, -78, 2, 2, '#f4f4ec');
      ob(g, 10, -88, 6, 5, b.rage ? '#ff3a4a' : '#ffd23a'); rect(g, 13, -87, 2, 3, '#160e1e');
      ob(g, -18, -84, 14, 9, '#f4f0e0'); bt(g, b, 'MT', -11, -82, { align: 'center', color: '#c83a4a', shadow: false, g });
    },
  },
  bigblock: {
    name: 'BIG BLOCK', color: '#ff8a3a', hp: 200, w: 58, h: 58, speed: 0.55, cd: 76, keep: 50,
    attacks: [{ k: 'slam', h: 7 }, { k: 'charge', speed: 3.2, dmg: 12, near: true }, { k: 'shoot', proj: 'spam', n: 4, every: 9, arc: true, speed: 4 }],
    say: ['BIGGER BLOCKS!', 'MORE ROOM!', 'CHEAP FEES FOR ALL!', 'SIZE MATTERS!'],
    draw(g, b, t) {
      const sq = b.pose === 'land' ? 4 : b.pose === 'wind' ? 2 : 0;
      ob(g, -29 - sq, -58 + sq, 58 + sq * 2, 58 - sq, '#ff8a3a'); rect(g, -29 - sq, -58 + sq, 58 + sq * 2, 6, '#ffb06a'); rect(g, -29 - sq, -6, 58 + sq * 2, 6, '#c86a2a');
      bt(g, b, '32MB', 0, -18, { align: 'center', color: '#7a3a1a', shadow: false, g });
      ob(g, 0, -44 + sq, 9, 9, '#ffffff'); ob(g, 14, -44 + sq, 9, 9, '#ffffff'); rect(g, 4, -41 + sq, 4, 5, '#160e1e'); rect(g, 18, -41 + sq, 4, 5, '#160e1e');
      rect(g, 2, -30 + sq, 20, 3, '#160e1e'); if (b.rage) { rect(g, -2, -48 + sq, 12, 2, '#160e1e'); rect(g, 13, -48 + sq, 12, 2, '#160e1e'); }
    },
  },
  smallblock: {
    name: 'SMALL BLOCK', color: '#3ad0f0', hp: 130, w: 26, h: 26, speed: 1.9, cd: 40, keep: 24,
    attacks: [{ k: 'slam', h: 7.5, tele: 10 }, { k: 'charge', speed: 5.4, dmg: 7, near: true, tele: 18 }, { k: 'shoot', proj: 'digit', n: 3, every: 8, speed: 3.6 }],
    say: ['SMALLER BLOCKS!', 'RUN YOUR OWN NODE!', 'DECENTRALIZE!', 'NO, YOU STOP YELLING!'],
    draw(g, b, t) {
      const sq = b.pose === 'land' ? 3 : 0;
      ob(g, -13 - sq, -26 + sq, 26 + sq * 2, 26 - sq, '#3ad0f0'); rect(g, -13 - sq, -26 + sq, 26 + sq * 2, 4, '#8ae8ff');
      bt(g, b, '1MB', 0, -9, { align: 'center', color: '#1a5a7a', shadow: false, g });
      ob(g, 1, -21 + sq, 4, 5, '#ffffff'); ob(g, 7, -21 + sq, 4, 5, '#ffffff'); rect(g, 3, -19 + sq, 2, 3, '#160e1e'); rect(g, 9, -19 + sq, 2, 3, '#160e1e');
      ob(g, -14, -32 + sq, 28, 5, '#ffd23a');   // a little hard hat: it builds things
    },
  },
  tornado: {
    name: 'THE HYPE TORNADO', color: '#c8c8e8', hp: 310, w: 50, h: 104, speed: 1.1, cd: 56, keep: 10,
    attacks: [{ k: 'pull', dur: 100, force: 1.1, range: 220 }, { k: 'charge', speed: 3.4, dmg: 10, dur: 90 }, { k: 'rain', proj: 'rocket', n: 8, every: 10, far: true }, { k: 'shoot', proj: 'cash', n: 6, every: 6, speed: 3.6, spread: 1.4 }],
    say: ['TO THE MOOOON!', 'ROCKET EMOJIS!', 'HYYYYPE!', 'NUMBER ONLY GO UP!'],
    draw(g, b, t) {
      for (let i = 0; i < 13; i++) { const y = -8 - i * 7.6, hw = 6 + i * 2.4 + Math.sin(t / 5 + i) * 2, sx = Math.sin(t / 9 + i * 0.6) * (3 + i * 0.5);
        rect(g, sx - hw - 1, y - 1, hw * 2 + 2, 7, '#160e1e'); rect(g, sx - hw, y, hw * 2, 5, i % 2 ? '#c8c8e8' : '#9a9ab8');
        rect(g, sx - hw + ((t * 2 + i * 7) % (hw * 2 | 1)), y + 1, 6, 2, '#ffffff'); }
      for (let i = 0; i < 4; i++) { const a = t / 12 + i * 1.57, x = Math.cos(a) * 34, y = -50 - i * 12 + Math.sin(a) * 6;
        if (i % 2) bt(g, b, i === 1 ? '$' : '!', x, y, { color: '#5aff8a', outline: '#160e1e', g }); else { rect(g, x - 2, y - 4, 4, 8, '#e8e8f4'); rect(g, x - 2, y - 6, 4, 2, '#ff4a5a'); } }
      if (b.rage) for (let i = 0; i < 3; i++) rect(g, rnd(-30, 30), rnd(-100, -10), 2, 2, '#ffd23a');
    },
  },
  mempool: {
    name: 'THE MEMPOOL MONSTER', color: '#ff5fd2', hp: 330, w: 84, h: 56, speed: 0.6, cd: 60, keep: 60,
    attacks: [{ k: 'rain', proj: 'spam', n: 10, every: 7 }, { k: 'shoot', proj: 'spam', n: 5, every: 7, arc: true, speed: 4.4, far: true }, { k: 'slam', h: 6 }, { k: 'pull', dur: 80, force: 0.9, range: 180 }],
    say: ['BLOOORP. SO FULL.', 'FEES ARE NOW ONE MILLION.', 'PENDING... PENDING...', 'MORE TRANSACTIONS!'],
    init(b) { const r = seeded(9); b.tiles = Array.from({ length: 46 }, () => [r(), r(), pick(['#ff5fd2', '#3ad0f0', '#ffd23a', '#5aff8a', '#ff8a3a', '#c8a2ff'])]); },
    draw(g, b, t) {
      const sq = b.pose === 'land' ? 4 : 0, bob = (t >> 4) % 2;
      for (const [u, v, c] of b.tiles) { const y = v * 54, hw = 42 * Math.sqrt(1 - Math.pow(y / 56, 2)), x = (u * 2 - 1) * hw; ob(g, x - 5, -y - 10 + sq * (y / 56) + bob, 10, 9, c); }
      const ey = -38 + sq; ob(g, -10, ey, 10, 11, '#ffffff'); ob(g, 6, ey, 10, 11, '#ffffff'); rect(g, -6, ey + 4, 4, 5, '#160e1e'); rect(g, 10, ey + 4, 4, 5, '#160e1e');
      ob(g, -14, -20 + sq, 34, b.pose === 'pull' ? 10 : 5, '#160e1e');
      bt(g, b, 'FEE: ' + (b.rage ? '9999' : '420') + ' SAT/B', 0, -68, { align: 'center', color: '#ffd23a', outline: '#160e1e', g });
    },
  },
  middleman: {
    name: 'THE MIDDLEMAN', color: '#c8c8d4', hp: 650, w: 26, h: 60, speed: 1.4, cd: 50, keep: 50,
    attacks: [{ k: 'shoot', proj: 'fee', n: 5, every: 6, speed: 4.4, spread: 0.5, far: true }, { k: 'shoot', proj: 'form', n: 3, every: 14, speed: 2.2, aim: true }, { k: 'beam', tele: 44, dur: 24, hy: 0.6, h: 14, dmg: 13 }, { k: 'charge', speed: 5, dmg: 11, near: true }, { k: 'rain', proj: 'form', n: 8, every: 9 }],
    say: ['TRUST ME.', 'JUST A SMALL FEE.', 'PLEASE FILL OUT THIS FORM.', 'YOUR KEYS ARE SAFE WITH ME.', 'I PREFER "TRUSTED THIRD PARTY".'],
    onRage(b) { b.sc = 1.6; b.c.hw = Math.round(b.B.w * 1.6); b.c.hh = Math.round(b.B.h * 1.6); fx({ type: 'text', s: 'FEES WENT UP!', x: b.x, y: b.y - 110, life: 90, color: '#ff4a5a', size: 2 }); },
    draw(g, b, t) {
      const p = b.pose, run = p === 'charge' ? (t >> 2) % 2 : 0;
      ob(g, -8, -22, 7, 22 - run * 3, '#2a2a3a'); ob(g, 2, -22, 7, 22 - (1 - run) * 3, '#2a2a3a'); rect(g, -9, -2, 9, 2, '#160e1e'); rect(g, 1, -2, 10, 2, '#160e1e');
      ob(g, -12, -46, 24, 26, '#3a3a4e'); rect(g, -3, -46, 8, 16, '#f4f4ec'); rect(g, 0, -45, 3, 14, '#d83a4a');
      ob(g, -8, -60, 17, 15, '#e8c0a0'); rect(g, -9, -62, 19, 5, '#2a1a10'); rect(g, -9, -58, 4, 4, '#2a1a10');
      rect(g, -2, -55, 12, 3, '#160e1e'); rect(g, 0, -49, 7, 1, '#7a3a2a');   // shades + smirk
      const arm = p === 'wind' ? -8 : p === 'shoot' ? -4 : 0;
      ob(g, 8, -42 + arm, 6, 14, '#3a3a4e'); ob(g, 10, -28 + arm, 14, 11, '#7a4a2a'); rect(g, 15, -30 + arm, 4, 2, '#ffd23a');
    },
  },
};

// ---- the Boss object: a Fighter (so hits / HUD / AI targeting just work) with its own update + drawing -------------------
class Boss extends Fighter {
  constructor(slot, kind, o = {}) {
    super(slot, 'arcade', 'cpu');
    const B = BOSSES[kind];
    Object.assign(this, { kind, B, boss: true, team: 'bad', sc: 1, pose: 'idle', rage: 0, hurtT: 0, dying: 0 });
    this.c = { name: B.name, hw: B.w, hh: B.h, weight: 999, jumps: 0, color: B.color };
    this.color = B.color; this.who = 'arcade';
    this.hpMax = this.hp = Math.round(B.hp * (o.hpMul || 1)); this.dmgMul = o.dmgMul || 1;
    this.stocks = 1;
    this.x = o.x ?? STAGE.main.x2 - 60; this.y = STAGE.main.y; this.face = -1;
    this.bs = { atk: null, cd: 80, vy: 0, last: null, sayT: 120 };
    if (B.init) B.init(this);
  }
}
const bossFoes = (b) => F.filter((o) => o !== b && !o.dead && o.state !== 'respawn' && o.team !== b.team);
function bossMouth(b) { return [b.x + b.face * b.c.hw * 0.45, b.y - b.c.hh * 0.6]; }
function updateBoss(b) {
  const s = b.bs, B = b.B, M = STAGE.main;
  b.boxes = null; b.vx = 0; b.vy = 0;
  if (b.dying) { b.dying++; if (b.dying % 8 === 0) { sparkBurst(b.x + rnd(-b.c.hw / 2, b.c.hw / 2), b.y - rnd(b.c.hh), 10, pick(['#ffd23a', '#ffffff', B.color]), 1.4); audio.sfx.hit(8); shake = 6; }
    if (b.dying > 100 && !b.out) { b.out = true; b.dead = true; b.outAt = frameNo; MATCH.order.unshift(b); flash = 6; audio.sfx.ko(); fx({ type: 'ring', x: b.x, y: b.y - b.c.hh / 2, r0: 4, r1: 120, color: '#ffffff', life: 30 }); }
    return; }
  if (b.hitlag > 0) { b.hitlag--; return; }
  if (b.hurtT > 0) b.hurtT--;
  const foes = bossFoes(b), tgt = foes.sort((p, q) => Math.abs(p.x - b.x) - Math.abs(q.x - b.x))[0];
  const spd = (B.speed || 1) * (b.rage ? 1.3 : 1);
  s.vy += 0.4; b.y += s.vy;
  if (b.y >= M.y) { b.y = M.y; s.vy = 0; b.onGround = true; } else b.onGround = false;
  if (--s.sayT <= 0) { s.sayT = 260 + rnd(200) | 0; fx({ type: 'text', s: pick(B.say), x: b.x, y: b.y - b.c.hh - 14, life: 80, color: B.color }); }
  if (s.atk) {
    const A = s.atk; A.t++;
    if (ATTACKS[A.k](b, A, tgt)) { s.atk = null; s.cd = Math.round((B.cd || 60) * (b.rage ? 0.65 : 1) * rnd(0.8, 1.2)); b.pose = 'idle'; }
  } else {
    b.pose = 'idle';
    if (tgt) { const dx = tgt.x - b.x; b.face = Math.sign(dx) || b.face; if (Math.abs(dx) > (B.keep ?? 50)) { b.x += Math.sign(dx) * spd; b.pose = 'walk'; } }
    if (--s.cd <= 0 && tgt) {
      const dist = Math.abs(tgt.x - b.x) - b.c.hw / 2;
      let opts = B.attacks.filter((a) => !(a.near && dist > 80) && !(a.far && dist < 50) && a !== s.last);
      if (!opts.length) opts = B.attacks.filter((a) => a !== s.last);
      const a = pick(opts); s.last = a; s.atk = { ...a, t: 0 }; b.hits = new Set();
      fx({ type: 'text', s: '!', x: b.x, y: b.y - b.c.hh - 6, life: 18, color: '#ff4a5a', size: 2 });
    }
  }
  b.x = clamp(b.x, M.x1 + b.c.hw / 2, M.x2 - b.c.hw / 2);
}
const bx = (b, x, y, w, h, A, d) => [{ x, y, w, h, dmg: Math.max(1, Math.round((A.dmg || d.dmg) * b.dmgMul)), kb: d.kb, kbg: d.kbg * (0.5 + 0.5 * b.dmgMul), ang: d.ang, radial: d.radial }];
const ATTACKS = {
  shoot(b, A, tgt) {
    const tele = A.tele ?? 24, n = A.n ?? 3, ev = A.every ?? 8;
    if (A.t < tele) { b.pose = 'wind'; if (tgt) b.face = Math.sign(tgt.x - b.x) || b.face; return false; }
    b.pose = 'shoot'; const k = A.t - tele;
    if (k % ev === 0 && k / ev < n) {
      const [mx, my] = bossMouth(b); let vx, vy;
      if (A.arc) { vx = b.face * rnd(A.speed * 0.45, A.speed); vy = rnd(-5, -2); }
      else if (A.aim && tgt) { const dx = tgt.x - mx, dy = tgt.y - tgt.c.hh / 2 - my, d = Math.hypot(dx, dy) || 1; vx = dx / d * A.speed; vy = dy / d * A.speed; }
      else { vx = b.face * A.speed; vy = A.spread ? rnd(-A.spread, A.spread) : 0; }
      PROJ.push(mkProj(b, A.proj, mx, my, vx, vy, {})); audio.sfx.swing();
    }
    return k > n * ev + 18;
  },
  slam(b, A, tgt) {
    const tele = A.tele ?? 18;
    if (A.t < tele) { b.pose = 'wind'; return false; }
    if (A.t === tele) { b.bs.vy = -(A.h ?? 8); b.onGround = false; A.vx = tgt ? clamp((tgt.x - b.x) / 38, -3.2, 3.2) : 0; A.air = true; audio.sfx.jump(); return false; }
    if (A.air) {
      b.x += A.vx; b.pose = 'air';
      if (b.bs.vy > 0) b.boxes = bx(b, -b.c.hw / 2, -b.c.hh * 0.6, b.c.hw, b.c.hh * 0.6 + 4, A, { dmg: 12, kb: 3.4, kbg: 4.2, ang: 60, radial: true });
      if (b.onGround) { A.air = false; A.land = A.t; shake = 7; audio.sfx.heavy(); dust(b.x, b.y, 10);
        for (const d of [-1, 1]) PROJ.push({ owner: b, type: 'wave', x: b.x + d * (b.c.hw / 2 + 6), y: b.y, vx: d * 3.2, vy: 0, w: 14, h: 12, dmg: Math.round(7 * b.dmgMul), kb: 3.0, kbg: 3.0, ang: 60, life: 46, ground: true, hits: new Set() }); }
      return false;
    }
    b.pose = 'land'; return A.t > A.land + (A.rest ?? 22);
  },
  charge(b, A, tgt) {
    const tele = A.tele ?? 30;
    if (A.t < tele) { b.pose = 'wind'; if (tgt) b.face = Math.sign(tgt.x - b.x) || b.face; if (A.t % 6 === 0) dust(b.x - b.face * 10, b.y, 1); return false; }
    const k = A.t - tele, M = STAGE.main, hw = b.c.hw / 2;
    if (A.done) { b.pose = 'land'; return A.t > A.done + 22; }
    b.pose = 'charge'; b.x += b.face * (A.speed ?? 4.5) * (b.rage ? 1.15 : 1);
    b.boxes = bx(b, -hw * 0.3, -b.c.hh * 0.85, hw * 1.3 + 6, b.c.hh * 0.85, A, { dmg: 11, kb: 3.4, kbg: 4.4, ang: 35 });
    if (k % 5 === 0) dust(b.x - b.face * hw, b.y, 2);
    if ((b.face < 0 && b.x <= M.x1 + hw + 2) || (b.face > 0 && b.x >= M.x2 - hw - 2) || k > (A.dur ?? 70)) A.done = A.t;
    return false;
  },
  swipe(b, A) {
    const tele = A.tele ?? 16;
    if (A.t < tele) { b.pose = 'wind'; return false; }
    const k = A.t - tele; b.pose = 'swipe';
    if (k === 0) audio.sfx.swing();
    if (k < 8) { b.boxes = bx(b, b.c.hw * 0.2, -b.c.hh * (A.hy ?? 0.7), A.reach ?? 40, A.hh ?? 30, A, { dmg: 12, kb: 3.8, kbg: 5, ang: 40 }); if (k === 2) fx({ type: 'arc', x: b.x + b.face * b.c.hw * 0.4, y: b.y - b.c.hh * (A.hy ?? 0.7) + 12, r: (A.reach ?? 40) * 0.7, a0: b.face > 0 ? -1.2 : Math.PI + 1.2, a1: b.face > 0 ? 1.0 : Math.PI - 1.0, color: '#ffffff', w: 3, life: 10 }); }
    return k > (A.rest ?? 26);
  },
  pull(b, A) {
    const tele = A.tele ?? 24, dur = A.dur ?? 100, range = A.range ?? 200;
    if (A.t < tele) { b.pose = 'wind'; return false; }
    const k = A.t - tele;
    if (k < dur) {
      b.pose = 'pull';
      for (const f of bossFoes(b)) { if (f.state === 'ledge') continue; const dx = b.x - f.x, d = Math.abs(dx); if (d < range && d > 4) f.x += Math.sign(dx) * (A.force ?? 1) * (1.2 - d / range); }
      if (k % 3 === 0) { const side = pick([-1, 1]); fx({ type: 'spark', x: b.x + side * rnd(60, 200), y: b.y - rnd(4, 60), vx: -side * 4, vy: 0, life: 22, color: '#ffffff88' }); }
      if (k % 14 === 0) b.hits = new Set();
      b.boxes = bx(b, -b.c.hw / 2 - 4, -b.c.hh * 0.8, b.c.hw + 8, b.c.hh * 0.8, A, { dmg: 2, kb: 1.2, kbg: 0.4, ang: 80, radial: true });
      return false;
    }
    if (k === dur) { b.hits = new Set(); shake = 8; audio.sfx.heavy(); fx({ type: 'ring', x: b.x, y: b.y - b.c.hh / 2, r0: 6, r1: 70, color: b.B.color, life: 18 }); }
    if (k < dur + 6) { b.pose = 'swipe'; b.boxes = bx(b, -b.c.hw / 2 - 26, -b.c.hh - 8, b.c.hw + 52, b.c.hh + 12, A, { dmg: 13, kb: 4.2, kbg: 5.2, ang: 50, radial: true }); }
    else b.pose = 'land';
    return k > dur + 30;
  },
  rain(b, A) {
    const tele = A.tele ?? 30, n = A.n ?? 8, ev = A.every ?? 10, M = STAGE.main;
    if (A.t === 1) A.xs = Array.from({ length: n }, () => rnd(M.x1 + 10, M.x2 - 10));
    if (A.t < tele) { b.pose = 'wind'; return false; }
    const k = A.t - tele; b.pose = 'shoot';
    if (k % ev === 0 && k / ev < n) { const x = A.xs[k / ev]; PROJ.push(mkProj(b, A.proj, x, -16, rnd(-0.3, 0.3), 2.2, {})); fx({ type: 'text', s: 'V', x, y: 16, life: 26, color: '#ff4a5a' }); }
    return k > n * ev + 30;
  },
  beam(b, A, tgt) {
    const tele = A.tele ?? 40, dur = A.dur ?? 24, hh = A.h ?? 16;
    A.y = b.y - b.c.hh * (A.hy ?? 0.6);
    if (A.t < tele) { b.pose = 'wind'; if (tgt && A.t < tele - 14) b.face = Math.sign(tgt.x - b.x) || b.face; return false; }
    const k = A.t - tele; b.pose = 'beam';
    if (k === 0) { audio.sfx.beam(); shake = 5; }
    if (k < dur) b.boxes = bx(b, b.c.hw * 0.3, -b.c.hh * (A.hy ?? 0.6) - hh / 2, W, hh, A, { dmg: 14, kb: 4.0, kbg: 5.0, ang: 22 });
    return k > dur + 20;
  },
};
// a hit landed on a boss: it loses HP instead of flying
function bossHurt(t, a, dmg, hx, hy, proj) {
  if (t.dying) return;
  const before = t.hp; t.hp = Math.max(0, t.hp - dmg); t.hurtT = 8; t.lastHit = a; t.lastHitT = frameNo;
  if (!proj) a.hitlag = Math.min(8, 2 + Math.floor(dmg * 0.3));
  audio.sfx.hit(dmg); sparkBurst(hx, hy, 5 + dmg, '#ffffff', 0.7);
  fx({ type: 'text', s: '-' + Math.round(dmg), x: hx, y: hy - 8, life: 24, color: '#ffd23a' });
  if (before > t.hpMax / 2 && t.hp <= t.hpMax / 2 && t.hp > 0) { t.rage = 1; shake = 8; fx({ type: 'text', s: 'IT\'S ANGRY!', x: t.x, y: t.y - t.c.hh - 24, life: 80, color: '#ff4a5a', size: 2 }); if (t.B.onRage) t.B.onRage(t); }
  if (t.hp <= 0) { t.dying = 1; t.bs.atk = null; t.inv = 9999; if (a && a.kos !== undefined && a !== t) a.kos++; STATS.kos++; }
}
// drawing: render to a small offscreen canvas so a hurt boss can flash white
const bossCanvas = {};
function bossImage(b, t) {
  const key = b.kind, cw = 200, ch = 200;
  const c = bossCanvas[key] || (bossCanvas[key] = offscreen(cw, ch, () => {}));
  const g = c.getContext('2d'); g.setTransform(1, 0, 0, 1, 0, 0); g.clearRect(0, 0, cw, ch);
  g.translate(cw / 2, ch - 8); b.B.draw(g, b, t);
  if (b.hurtT > 0 && b.hurtT % 4 < 2) { g.setTransform(1, 0, 0, 1, 0, 0); g.globalCompositeOperation = 'source-atop'; g.fillStyle = '#ffffffcc'; g.fillRect(0, 0, cw, ch); g.globalCompositeOperation = 'source-over'; }
  return c;
}
function drawBoss(b) {
  if (b.dead) return;
  const A = b.bs.atk;
  if (A && A.k === 'beam' && A.y !== undefined) {   // telegraph line, then the beam
    const x0 = b.x + b.face * b.c.hw * 0.3, x1 = b.face > 0 ? W : 0, hh = A.h ?? 16;
    if (b.pose === 'wind' && (frameNo >> 2) % 2) rect(ctx, Math.min(x0, x1), A.y - 1, Math.abs(x1 - x0), 2, '#ff4a5a');
    if (b.pose === 'beam' && A.t - (A.tele ?? 40) < (A.dur ?? 24)) { rect(ctx, Math.min(x0, x1), A.y - hh / 2, Math.abs(x1 - x0), hh, b.B.color); rect(ctx, Math.min(x0, x1), A.y - hh / 4, Math.abs(x1 - x0), hh / 2, '#ffffff'); }
  }
  const img = bossImage(b, frameNo), k = b.sc || 1, sh = b.dying ? rnd(-2, 2) : 0;
  ctx.save(); ctx.translate(Math.round(b.x + sh), Math.round(b.y)); if (b.face < 0) ctx.scale(-1, 1);
  if (b.dying) ctx.globalAlpha = Math.max(0.2, 1 - b.dying / 110);
  ctx.drawImage(img, -100 * k, -(200 - 8) * k, 200 * k, 200 * k); ctx.restore();
  if (b.pose === 'wind' && (frameNo >> 3) % 2) text('!', b.x, b.y - b.c.hh - 14, { size: 2, align: 'center', color: '#ff4a5a', outline: '#160e1e' });
}
/** a boss drawn small and still (talk portraits, chapter cards) */
function drawBossStill(kind, x, y, scale = 1, t = 0, face = 1) {
  const fake = { kind: kind + '_still', B: BOSSES[kind], pose: 'idle', rage: 0, hurtT: 0, sc: 1, face, bs: {} };
  if (BOSSES[kind].init) BOSSES[kind].init(fake);
  const img = bossImage(fake, t);
  ctx.save(); ctx.translate(Math.round(x), Math.round(y)); if (face < 0) ctx.scale(-1, 1); ctx.drawImage(img, -100 * scale, -192 * scale, 200 * scale, 200 * scale); ctx.restore();
}
