// game.js - fighters, moves, specials, projectiles, hits + knockback, KOs (incl. smacking into the screen), CPU, effects.
'use strict';
// ---- sprites ------------------------------------------------------------------------------------------------------
const SPR = {};
function loadSprites() {
  return Promise.all(Object.keys(SPRITE_DATA).map((w) => new Promise((res) => {
    const d = SPRITE_DATA[w], img = new Image();
    img.onload = () => { SPR[w] = { img, meta: d.meta }; res(); }; img.onerror = () => res(); img.src = d.src;
  })));
}
/** draw a sprite frame with its feet at (x, y). */
function drawSprite(who, anim, i, x, y, flip = false, o = {}) {
  const L = flip && SPR[who + '-left'];   // a real left-facing sheet (letters read right) beats a mirrored one
  if (L) flip = false;
  const s = L || SPR[who]; if (!s) return;
  const m = s.meta, [cw, ch] = m.cell, a = m.anims[anim] || m.anims.idle, fi = Math.min(a.frames - 1, Math.max(0, i | 0)), k = o.scale || 1;
  const g = o.g || ctx;
  g.save(); g.translate(Math.round(x), Math.round(y));
  if (o.rot) g.rotate(o.rot);
  if (flip) g.scale(-1, 1);
  if (o.alpha !== undefined) g.globalAlpha = o.alpha;
  g.drawImage(s.img, fi * cw, a.row * ch, cw, ch, -cw / 2 * k, -(ch - m.ground) * k, cw * k, ch * k);
  g.restore();
}

// ---- effects ------------------------------------------------------------------------------------------------------
let FX = [], shake = 0, flash = 0;
function fx(o) { FX.push({ t: 0, life: 20, ...o }); }
function sparkBurst(x, y, n, color, power = 1) {
  for (let i = 0; i < n; i++) { const a = rnd(Math.PI * 2), v = rnd(1, 3.5) * power; fx({ type: 'spark', x, y, vx: Math.cos(a) * v, vy: Math.sin(a) * v, life: rnd(10, 20) | 0, color }); }
  fx({ type: 'star', x, y, life: 8, size: 6 + power * 5, color: '#ffffff' });
}
function dust(x, y, n = 3) { for (let i = 0; i < n; i++) fx({ type: 'dust', x: x + rnd(-6, 6), y, vx: rnd(-0.6, 0.6), vy: rnd(-0.6, -0.1), life: rnd(14, 24) | 0 }); }
function drawFX() {
  for (const e of FX) {
    const k = e.t / e.life;
    if (e.type === 'spark') { rect(ctx, e.x, e.y, 2, 2, e.color); }
    else if (e.type === 'dust') { rect(ctx, e.x - 2, e.y - 2, 4 * (1 - k) + 1, 4 * (1 - k) + 1, '#e8dcc8' + (k < 0.5 ? 'cc' : '66')); }
    else if (e.type === 'star') {           // four-point impact star
      const s = e.size * (1 - k * 0.5);
      ctx.fillStyle = e.color; ctx.fillRect(e.x - s, e.y - 1, s * 2, 2); ctx.fillRect(e.x - 1, e.y - s, 2, s * 2);
      ctx.fillRect(e.x - s * 0.5, e.y - s * 0.5, s, s);
    }
    else if (e.type === 'ring') { pixelCircle(e.x, e.y, e.r0 + (e.r1 - e.r0) * k, e.color); }
    else if (e.type === 'arc') { pixelArc(e.x, e.y, e.r, e.a0, e.a1, e.color, e.w || 2, 1 - k); }
    else if (e.type === 'text') { text(e.s, e.x, e.y - k * 12, { size: e.size || 1, align: 'center', color: e.color, outline: '#160e1e' }); }
    else if (e.type === 'beam') { const a = 1 - k; ctx.fillStyle = e.color; ctx.globalAlpha = a; ctx.fillRect(e.x, e.y - e.h / 2 * a, e.w, e.h * a); ctx.globalAlpha = 1; }
    else if (e.type === 'blast') {         // KO blast: a wedge of light from the edge where they left
      ctx.save(); ctx.globalAlpha = 1 - k; ctx.fillStyle = e.color; ctx.translate(e.x, e.y); ctx.rotate(e.ang);
      ctx.beginPath(); ctx.moveTo(0, 0); ctx.lineTo(-260, -22 - 30 * k); ctx.lineTo(-260, 22 + 30 * k); ctx.closePath(); ctx.fill();
      ctx.fillStyle = '#ffffff'; ctx.beginPath(); ctx.moveTo(0, 0); ctx.lineTo(-200, -6); ctx.lineTo(-200, 6); ctx.closePath(); ctx.fill(); ctx.restore();
    }
    else if (e.type === 'splat') drawSplat(e, k);
    if (e.vx !== undefined) { e.x += e.vx; e.y += e.vy; e.vy += e.g || 0; }
    e.t++;
  }
  FX = FX.filter((e) => e.t < e.life);
}
function pixelCircle(cx, cy, r, col) { ctx.fillStyle = col; const n = Math.max(12, r * 4 | 0); for (let i = 0; i < n; i++) { const a = i / n * Math.PI * 2; ctx.fillRect(Math.round(cx + Math.cos(a) * r), Math.round(cy + Math.sin(a) * r), 1, 1); } }
function pixelArc(cx, cy, r, a0, a1, col, w, alpha = 1) {
  ctx.globalAlpha = alpha; ctx.fillStyle = col;
  const n = Math.max(8, Math.abs(a1 - a0) * r | 0);
  for (let i = 0; i <= n; i++) { const a = a0 + (a1 - a0) * i / n, ww = w * Math.sin(Math.PI * i / n) + 1; ctx.fillRect(Math.round(cx + Math.cos(a) * r), Math.round(cy + Math.sin(a) * r), ww, ww); }
  ctx.globalAlpha = 1;
}
// the screen smack: the fighter flies INTO the glass, sticks, the glass cracks, then they slide down and off
function drawSplat(e, k) {
  const f = e.f, hold = 0.55, sl = k < hold ? 0 : (k - hold) / (1 - hold);
  const x = e.x, y = e.y + sl * sl * 220, sc = k < 0.06 ? 1 + k / 0.06 * 2.2 : 3.2;
  // cracks
  ctx.strokeStyle = '#ffffffcc'; ctx.lineWidth = 1; ctx.beginPath();
  for (const c of e.cracks) { ctx.moveTo(e.x, e.y - 30); let px = e.x, py = e.y - 30; for (const [dx, dy] of c) { px += dx; py += dy; ctx.lineTo(px, py); } }
  ctx.stroke();
  ctx.fillStyle = '#ffffff22'; ctx.beginPath(); ctx.arc(e.x, e.y - 30, 26, 0, Math.PI * 2); ctx.fill();
  drawSprite(f.who, 'splat', 0, x, y, false, { scale: sc, alpha: 1 - Math.max(0, sl - 0.6) * 2.5 });
  if (k < hold) text(e.word, e.x, e.y - 30 - 60, { size: 2, align: 'center', color: '#ffd23a', outline: '#160e1e' });
}

// ---- fighters -------------------------------------------------------------------------------------------------------
const STATS = { kos: 0, splats: 0 };
let F = [], PROJ = [], STAGE = null, frameNo = 0;
class Fighter {
  constructor(slot, who, ctrl) {
    Object.assign(this, { slot, who, ctrl, c: CHARS[who], color: PCOLORS[slot] });
    this.stocks = 3; this.score = 0; this.kos = 0; this.falls = 0; this.dmg = 0;
    this.reset(...STAGE.spawn[slot]);
  }
  reset(x, y) {
    Object.assign(this, { x, y, vx: 0, vy: 0, face: x < W / 2 ? 1 : -1, state: 'idle', t: 0, move: null, mt: 0, hits: new Set(), jumps: this.c.jumps, onGround: true, plat: null,
      inv: 0, shieldHP: 60, hitlag: 0, stun: 0, dropT: 0, ledgeCD: 0, ledge: null, fastfall: false, sp: null, lastHit: null, lastHitT: -999, dead: false, ai: { t: 0, pad: {} } });
  }
  get hb() { return { x: this.x - this.c.hw / 2, y: this.y - this.c.hh, w: this.c.hw, h: this.c.hh }; }
  get mid() { return [this.x, this.y - this.c.hh / 2]; }
  actionable() { return ['idle', 'run', 'crouch', 'air', 'land0', 'squat'].includes(this.state); }
  setState(s) { this.state = s; this.t = 0; }
}

function startMove(f, key) {
  f.move = f.c.moves[key]; f.moveKey = key; f.mt = 0; f.hits = new Set(); f.setState('attack');
  if (f.move.swing) audio.sfx.swing();
}
function moveFrame(m, mt) { let acc = 0; for (const [fi, d] of m.seq) { acc += d; if (mt < acc) return fi; } return m.seq[m.seq.length - 1][0]; }
const moveLen = (m) => m.seq.reduce((a, [, d]) => a + d, 0);

// ---- specials: one neutral + one up special per fighter ----------------------------------------------------------------
// each is { start(f), tick(f) -> true when finished }, using f.sp = { t, ... }; sprites from the 'special' / 'upspecial' rows
function hitboxNow(f, box) { return { owner: f, ...box }; }
const SPECIALS = {
  buster: {
    neutral: {
      start(f) { f.sp = { t: 0, air: !f.onGround }; if (f.sp.air) { f.vy = -2; f.vx *= 0.3; } },
      tick(f) {   // ground: wind up, slam, two shockwaves. air: plummet with a hitbox, slam on landing
        const s = f.sp; s.t++;
        f.spFrame = s.t < 10 ? 0 : s.t < 18 ? 1 : s.t < 24 ? 2 : 3;
        if (s.air && !s.landed) { if (s.t > 10) { f.vy = 7; f.vx *= 0.9; f.boxes = [{ x: -12, y: -20, w: 24, h: 24, dmg: 12, kb: 3, kbg: 4, ang: -75 }]; } if (f.onGround && s.t > 10) { s.landed = true; s.t = 18; } return s.t > 60; }
        if (s.t === 18) { shake = 6; audio.sfx.heavy(); dust(f.x, f.y, 8);
          for (const d of [-1, 1]) PROJ.push({ owner: f, type: 'wave', x: f.x + d * 14, y: f.y, vx: d * 3.6, vy: 0, w: 14, h: 12, dmg: 7, kb: 3.2, kbg: 3.2, ang: 60, life: 40, ground: true, hits: new Set() }); }
        f.boxes = s.t >= 17 && s.t < 22 ? [{ x: -4, y: -28, w: 34, h: 28, dmg: 15, kb: 4, kbg: 6, ang: 70 }] : null;
        f.vx *= 0.8; return s.t > 40;
      },
    },
    up: {   // Block Reward: rising uppercut
      start(f) { f.sp = { t: 0 }; f.vy = -9.2; f.vx = f.face * 1.2; audio.sfx.jump(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 8 ? 0 : 1; f.boxes = s.t < 16 ? [{ x: -6, y: -60, w: 26, h: 40, dmg: 10, kb: 3.4, kbg: 4.4, ang: 80 }] : null; return s.t > 22; },
    },
  },
  lester: {
    neutral: {   // Coin Toss: a silver coin that flies out and comes back
      start(f) { f.sp = { t: 0 }; },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 6 ? 0 : s.t < 10 ? 1 : s.t < 18 ? 2 : 3;
        if (s.t === 10) { audio.sfx.coin(); PROJ.push({ owner: f, type: 'coin', x: f.x + f.face * 14, y: f.y - 40, vx: f.face * 5.2, vy: 0, w: 10, h: 10, dmg: 6, kb: 2.6, kbg: 2.4, ang: 40, life: 70, boomerang: true, hits: new Set() }); }
        if (!f.onGround) f.vy *= 0.9; return s.t > 26; },
    },
    up: {   // Scrypt Spin: spinning rise with several small hits
      start(f) { f.sp = { t: 0 }; f.vy = -8.2; audio.sfx.jump(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = (s.t >> 2) % 2; if (s.t % 5 === 0) f.hits = new Set();
        f.boxes = s.t < 24 ? [{ x: -24, y: -50, w: 48, h: 44, dmg: 2, kb: 1.6, kbg: 0.6, ang: 80, multi: true }] : null; if (s.t < 18) f.vy = Math.min(f.vy, -5); return s.t > 26; },
    },
  },
  shiba: {
    neutral: {   // Moon Dash: curl up and roll forward
      start(f) { f.sp = { t: 0 }; audio.sfx.swing(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = (s.t >> 2) % 4;
        if (s.t > 6 && s.t < 30) { f.vx = f.face * 5.4; if (!f.onGround) f.vy = Math.min(f.vy, 1); f.boxes = [{ x: -16, y: -30, w: 32, h: 28, dmg: 9, kb: 3.2, kbg: 3.4, ang: 35 }]; } else f.boxes = null;
        if (s.t >= 30) f.vx *= 0.8; return s.t > 36; },
    },
    up: {   // To the Moon: a rocket leap straight up
      start(f) { f.sp = { t: 0 }; f.vy = -11; f.vx *= 0.3; audio.sfx.go(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 6 ? 0 : 1; if (s.t % 2) fx({ type: 'spark', x: f.x, y: f.y, vx: rnd(-0.5, 0.5), vy: 2, life: 14, color: '#ffd23a' });
        f.boxes = s.t < 14 ? [{ x: -14, y: -40, w: 28, h: 36, dmg: 8, kb: 3, kbg: 4, ang: 85 }] : null; return s.t > 20; },
    },
  },
  pep3d: {
    neutral: {   // Tongue Lash: a long tongue that pulls you in
      start(f) { f.sp = { t: 0 }; },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 6 ? 0 : s.t < 18 ? 1 + ((s.t >> 2) % 2) : 3;
        const len = s.t < 6 ? 0 : s.t < 14 ? (s.t - 6) * 11 : s.t < 22 ? 88 - (s.t - 14) * 11 : 0; s.len = len;
        f.boxes = len > 10 ? [{ x: 4, y: -34, w: len, h: 8, dmg: 7, kb: 3.4, kbg: 1.4, ang: 170, pull: true }] : null;
        if (!f.onGround) f.vy *= 0.85; f.vx *= 0.7; return s.t > 26; },
    },
    up: {   // Frog Leap: a huge jump, kick on the way up
      start(f) { f.sp = { t: 0 }; f.vy = -9; audio.sfx.jump(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 10 ? 0 : 1; f.boxes = s.t < 12 ? [{ x: -12, y: -14, w: 24, h: 18, dmg: 9, kb: 3, kbg: 4, ang: 75 }] : null; return s.t > 18; },
    },
  },
  digi: {
    neutral: {   // Visor Beam: charge, then a long beam
      start(f) { f.sp = { t: 0 }; },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 16 ? 0 : s.t < 30 ? 1 + ((s.t >> 1) % 2) : 3;
        if (s.t < 16 && s.t % 3 === 0) fx({ type: 'spark', x: f.x + f.face * 16 + rnd(-8, 8), y: f.y - 30 + rnd(-8, 8), vx: 0, vy: 0, life: 6, color: '#5aebf5' });
        if (s.t === 16) { audio.sfx.beam(); fx({ type: 'beam', x: f.face > 0 ? f.x + 14 : f.x - 214, y: f.y - 30, w: 200, h: 10, life: 16, color: '#5aebf5' }); }
        f.boxes = s.t >= 16 && s.t < 26 ? [{ x: 14, y: -35, w: 200, h: 10, dmg: 11, kb: 3.6, kbg: 3.6, ang: 20 }] : null;
        if (!f.onGround) f.vy = Math.min(f.vy, 0.6); f.vx *= 0.6; return s.t > 38; },
    },
    up: {   // DigiShield Jet: a steerable hover upward
      start(f) { f.sp = { t: 0 }; audio.sfx.jump(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = (s.t >> 3) % 2; f.vy = s.t < 34 ? -3.6 : f.vy;
        if (s.t % 2) fx({ type: 'spark', x: f.x + rnd(-4, 4), y: f.y, vx: 0, vy: 2, life: 10, color: '#5aebf5' });
        f.boxes = s.t < 40 ? [{ x: -10, y: -6, w: 20, h: 12, dmg: 3, kb: 2, kbg: 1, ang: -80, multi: true }] : null; if (s.t % 8 === 0) f.hits = new Set(); return s.t > 40; },
    },
  },
  arcade: {
    neutral: {   // Insert Coin: the coin door opens and three tokens fly out
      start(f) { f.sp = { t: 0 }; audio.sfx.coin(); },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 6 ? 0 : s.t < 26 ? 1 + ((s.t >> 3) % 2) : 3;
        if (s.t === 10 || s.t === 16 || s.t === 22) { const k = (s.t - 10) / 6; audio.sfx.coin();
          PROJ.push({ owner: f, type: 'token', x: f.x + f.face * 10, y: f.y - 28, vx: f.face * (2.6 + k * 1.2), vy: -4.4 + k * 0.6, g: 0.22, w: 10, h: 10, dmg: 5, kb: 2.4, kbg: 2.6, ang: 45, life: 90, hits: new Set() }); }
        f.vx *= 0.7; if (!f.onGround) f.vy *= 0.92; return s.t > 34; },
    },
    up: {   // Joystick Spring: squat, then launch
      start(f) { f.sp = { t: 0 }; },
      tick(f) { const s = f.sp; s.t++; f.spFrame = s.t < 8 ? 0 : 1; if (s.t === 8) { f.vy = -10.4; audio.sfx.jump(); dust(f.x, f.y, 6); }
        f.boxes = s.t >= 8 && s.t < 20 ? [{ x: -18, y: -64, w: 36, h: 30, dmg: 11, kb: 3.4, kbg: 4.6, ang: 85 }] : null; if (s.t < 8) f.vy = Math.min(f.vy, 0.5); return s.t > 26; },
    },
  },
};

// ---- per-frame fighter update -------------------------------------------------------------------------------------
const GROUND_STATES = ['idle', 'run', 'crouch', 'shield', 'land0', 'land', 'dizzy', 'roll'];
function updateFighter(f, pad) {
  if (f.dead) return;
  if (f.hitlag > 0) { f.hitlag--; return; }
  const c = f.c; f.t++; f.boxes = null;
  if (f.inv > 0) f.inv--; if (f.dropT > 0) f.dropT--; if (f.ledgeCD > 0) f.ledgeCD--;
  if (f.state !== 'shield' && f.shieldHP < 60) f.shieldHP += 0.12;

  // --- respawn platform ---
  if (f.state === 'respawn') {
    f.vx = f.vy = 0;
    if (f.t > 150 || (f.t > 30 && (pad.x || pad.jumpP || pad.attackP || pad.specialP || pad.y > 0.5))) { f.setState('air'); f.jumps = c.jumps; f.inv = 90; }
    return;
  }
  // --- ledge hang ---
  if (f.state === 'ledge') {
    const L = f.ledge;
    f.x = L.x + L.side * 6; f.y = L.y + c.hh * 0.75; f.vx = f.vy = 0; f.face = -L.side;
    if (pad.jumpP || pad.upP) { f.setState('air'); f.vy = -c.jump; f.jumps = c.jumps - 1; f.ledge.busy = null; f.ledgeCD = 20; audio.sfx.jump(); }
    // fresh presses only (still holding the direction you fell with must not drop you straight off), and a short grace period
    const toward = f.face > 0 ? pad.rightP : pad.leftP, away = f.face > 0 ? pad.leftP : pad.rightP;
    if (f.t > 6 && (toward || pad.attackP)) { f.x = L.x - L.side * 12; f.y = L.y; f.setState('idle'); f.onGround = true; L.busy = null; f.ledgeCD = 20; }
    else if ((f.t > 6 && (away || pad.downP)) || f.t > 300) { f.setState('air'); L.busy = null; f.ledgeCD = 40; }
    return;
  }

  const actionable = f.actionable() || (f.state === 'hurt' && f.t > f.stun) || (f.state === 'tumble' && f.t > f.stun);
  if ((f.state === 'hurt' || f.state === 'tumble') && f.t > f.stun) f.state = f.onGround ? 'idle' : 'air';

  // --- inputs ---
  if (actionable) {
    if (f.onGround && pad.x && f.state !== 'attack') f.face = Math.sign(pad.x);
    if (pad.specialP) {
      const key = pad.y < -0.5 ? 'up' : 'neutral';
      if (pad.x) f.face = Math.sign(pad.x);
      const S = SPECIALS[f.who][key]; f.spKey = key; f.setState('special'); f.hits = new Set(); S.start(f); return;
    }
    if (pad.attackP) {
      if (f.onGround) startMove(f, pad.y < -0.5 ? 'up' : pad.y > 0.5 ? 'low' : Math.abs(pad.x) > 0.5 ? 'smash' : 'jab');
      else startMove(f, pad.y < -0.5 ? 'uair' : pad.y > 0.5 ? 'dair' : 'air');
      if (pad.x && f.onGround) f.face = Math.sign(pad.x);
      return;
    }
    if (pad.jumpP && f.jumps > 0 && f.state !== 'squat') {
      const first = f.onGround;
      if (first) { f.setState('squat'); return; }   // 3-frame jump squat: up + attack/special pressed with the jump key = up moves, not a jump
      f.vy = -c.jump * (first ? 1 : 0.92); f.jumps--; f.onGround = false; f.setState('air'); f.fastfall = false; audio.sfx.jump();
      if (!first) { f.vx = pad.x * c.air; fx({ type: 'ring', x: f.x, y: f.y, r0: 4, r1: 14, life: 10, color: '#ffffffaa' }); } else dust(f.x, f.y, 2);
      return;
    }
    if (f.onGround && pad.shield) { f.setState('shield'); }
  }

  // --- states ---
  if (f.state === 'shield') {
    f.shieldHP -= 0.22; f.vx *= 0.7;
    if (pad.leftP || pad.rightP) { f.setState('roll'); f.rollDir = pad.leftP ? -1 : 1; f.inv = 16; }
    else if (!pad.shield) f.setState('idle');
    else if (pad.jumpP && f.jumps > 0) { f.vy = -c.jump; f.jumps--; f.onGround = false; f.setState('air'); }
    if (f.shieldHP <= 0) { f.shieldHP = 30; f.setState('dizzy'); f.vy = -5; f.onGround = false; audio.sfx.heavy(); sparkBurst(f.x, f.y - 20, 16, '#ffffff', 1.4); }
  } else if (f.state === 'roll') {
    f.vx = f.rollDir * 3.2 * (1 - f.t / 20); if (f.t > 20) { f.setState('idle'); f.face = -f.rollDir; }
  } else if (f.state === 'dizzy') {
    f.vx *= 0.8; if (f.t > 150) f.setState('idle');
  } else if (f.state === 'attack') {
    f.mt++;
    const m = f.move;
    for (const h of m.hit) if (f.mt >= h.t0 && f.mt <= h.t1) { (f.boxes = f.boxes || []).push(h); if (f.mt === h.t0) swingFX(f, m); }
    if (!m.air && f.onGround) f.vx *= 0.82;
    if (m.air || !f.onGround) f.vx = clamp(f.vx + pad.x * 0.18, -c.air, c.air);
    if (f.mt >= moveLen(m)) f.setState(f.onGround ? 'idle' : 'air');
  } else if (f.state === 'special') {
    const S = SPECIALS[f.who][f.spKey];
    if (!f.onGround) f.vx = clamp(f.vx + pad.x * 0.12, -c.air * 1.1, c.air * 1.1);
    if (f.who === 'digi' && f.spKey === 'up') f.vx = clamp(f.vx + pad.x * 0.3, -2.4, 2.4);
    if (S.tick(f)) { if (f.spKey === 'up' && !f.onGround) f.setState('helpless'); else f.setState(f.onGround ? 'idle' : 'air'); }
  } else if (f.state === 'helpless') {
    f.vx = clamp(f.vx + pad.x * 0.15, -c.air * 0.8, c.air * 0.8);
  } else if (f.state === 'land' || f.state === 'land0') {
    f.vx *= 0.7; if (f.t > (f.state === 'land' ? 12 : 3)) f.setState('idle');
  } else if (f.state === 'squat') {
    f.vx *= 0.9;
    if (f.t >= 3) { f.vy = -c.jump; f.jumps--; f.onGround = false; f.setState('air'); f.fastfall = false; audio.sfx.jump(); dust(f.x, f.y, 2); }
  } else if (f.state === 'hurt' || f.state === 'tumble') {
    // drift only
  } else if (f.onGround) {
    if (pad.y > 0.5 && !pad.x) { f.state = 'crouch'; f.vx *= 0.7; if (pad.downP && f.plat && !f.plat.main) { f.dropT = 14; f.onGround = false; f.plat = null; f.setState('air'); } }
    else if (pad.x) { f.state = 'run'; f.vx = lerp(f.vx, pad.x * c.run, 0.35); if (f.t % 14 === 0) dust(f.x - f.face * 6, f.y, 1); }
    else { f.state = 'idle'; f.vx *= 0.7; }
  } else {
    f.state = 'air';
    f.vx = clamp(f.vx + pad.x * 0.22, -c.air, c.air); if (!pad.x) f.vx *= 0.98;
    if (pad.downP && f.vy > -1.5) f.fastfall = true;
  }

  // --- physics ---
  const tumbling = f.state === 'tumble' || f.state === 'hurt';
  const maxFall = f.fastfall ? c.fall * 1.5 : c.fall;
  if (!f.onGround) { f.vy = Math.min(f.vy + c.grav * (tumbling ? 0.85 : 1), tumbling ? 9 : maxFall); }
  if (tumbling) { f.vx *= 0.975; if (f.state === 'tumble' && f.t % 3 === 0) fx({ type: 'dust', x: f.x, y: f.y - c.hh / 2, vx: 0, vy: 0, life: 14 }); }
  collide(f, tumbling);
  if (f.onGround && f.plat && f.plat.dx !== undefined) { f.x += f.plat.dx || 0; }
}

function collide(f, tumbling) {
  const c = f.c, S = STAGE, oy = f.y, ox = f.x;
  let nx = f.x + f.vx, ny = f.y + f.vy;
  const wasGround = f.onGround;
  f.onGround = false;
  const surfaces = [{ ...S.main, main: true, ref: S.main }, ...S.plats.map((p) => ({ ...p, ref: p }))];
  for (const p of surfaces) {
    if (nx < p.x1 - 2 || nx > p.x2 + 2) continue;
    if (!p.main && (f.dropT > 0 || f.vy < 0)) continue;
    const top = p.y;
    if (oy <= top + (p.ref.dy > 0 ? p.ref.dy + 1 : 1) && ny >= top - 0.01 && f.vy >= -0.01 || (wasGround && f.plat === p.ref && Math.abs(oy - top) < 4 + Math.abs(p.ref.dy || 0) && f.vy >= 0)) {
      ny = top; f.onGround = true; f.plat = p.ref; f.plat.main = !!p.main;
      if (!wasGround) land(f, tumbling);
      break;
    }
  }
  // the main block's sides and bottom are solid
  const M = S.main;
  if (!f.onGround && ny > M.y + 1 && ny - c.hh < (M.bottom || H) && nx > M.x1 && nx < M.x2) {
    if (oy - c.hh >= (M.bottom || H) - 1) { ny = (M.bottom || H) + c.hh; f.vy = Math.max(0, f.vy); }
    else { nx = ox <= M.x1 ? M.x1 - 0.5 : ox >= M.x2 ? M.x2 + 0.5 : (nx - M.x1 < M.x2 - nx ? M.x1 - 0.5 : M.x2 + 0.5); f.vx = tumbling ? -f.vx * 0.5 : 0; }
  }
  f.x = nx; f.y = ny;
  if (f.onGround) { f.vy = 0; f.jumps = c.jumps; f.fastfall = false; }
  // ledge grab
  if (!f.onGround && f.vy >= 0 && !f.ledgeCD && ['air', 'helpless', 'special'].includes(f.state) && !(f.state === 'special' && f.spKey !== 'up')) {
    for (const L of S.ledges) {
      if (L.busy && L.busy !== f && !L.busy.dead && L.busy.state === 'ledge') continue;
      const dx = (f.x - L.x) * L.side;
      if (dx > -6 && dx < 20 && f.y > L.y + 4 && f.y < L.y + c.hh + 18) {
        f.setState('ledge'); f.ledge = L; L.busy = f; f.jumps = c.jumps; f.inv = Math.max(f.inv, 40); f.vx = f.vy = 0; break;
      }
    }
  }
}
function land(f, tumbling) {
  f.vy = 0;
  if (f.state === 'attack' && f.move.air) { f.setState('land'); }
  else if (f.state === 'helpless') f.setState('land');
  else if (f.state === 'tumble') { f.setState('land'); dust(f.x, f.y, 6); audio.sfx.land(); }
  else if (f.state === 'air') { f.setState('land0'); dust(f.x, f.y, 2); audio.sfx.land(); }
}

function swingFX(f, m) {
  const [cx, cy] = f.mid, d = f.face, col = '#ff7ad8', r = m.swing === 'big' ? 26 : 18;
  if (m.swing === 'up') fx({ type: 'arc', x: cx, y: cy - 6, r, a0: Math.PI * (d > 0 ? 0.1 : 0.9), a1: Math.PI * (d > 0 ? -1.1 : 2.1), color: col, life: 9 });
  else if (m.swing === 'low') fx({ type: 'arc', x: cx, y: f.y - 6, r, a0: d > 0 ? -0.6 : Math.PI + 0.6, a1: d > 0 ? 0.6 : Math.PI - 0.6, color: col, life: 8 });
  else fx({ type: 'arc', x: cx, y: cy - 4, r, a0: d > 0 ? -1.2 : Math.PI + 1.2, a1: d > 0 ? 0.9 : Math.PI - 0.9, color: m.heavy ? '#ffd23a' : col, w: m.heavy ? 3 : 2, life: 9 });
}

// ---- hits ------------------------------------------------------------------------------------------------------------
function worldBox(f, b) {   // a box relative to fighter feet, facing-aware
  const x = f.face > 0 ? f.x + b.x : f.x - b.x - b.w;
  return { x, y: f.y + b.y, w: b.w, h: b.h };
}
const overlap = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
function resolveHits(onKoCredit) {
  for (const a of F) {
    if (a.dead || !a.boxes || a.hitlag) continue;
    for (const b of a.boxes) {
      const wb = worldBox(a, b);
      for (const t of F) {
        if (t === a || t.dead || t.inv > 0 || t.state === 'respawn' || a.hits.has(t) || (a.team && a.team === t.team)) continue;
        if (!overlap(wb, t.hb)) continue;
        a.hits.add(t);
        applyHit(a, t, b, b.radial ? (Math.sign(t.x - a.x) || a.face) : a.face, wb, false);
      }
    }
  }
  for (const p of PROJ) {
    for (const t of F) {
      if (t === p.owner || t.dead || t.inv > 0 || t.state === 'respawn' || p.hits.has(t) || (p.owner.team && p.owner.team === t.team)) continue;
      if (!overlap({ x: p.x - p.w / 2, y: p.y - p.h / 2, w: p.w, h: p.h }, t.hb)) continue;
      p.hits.add(t); applyHit(p.owner, t, p, Math.sign(p.vx) || p.owner.face, { x: p.x - p.w / 2, y: p.y - p.h / 2, w: p.w, h: p.h }, true);
      if (!p.ground && p.type !== 'coin') p.life = 0;
    }
  }
}
function applyHit(a, t, b, dir, wb, proj) {   // proj: a projectile hit freezes only the target, not the thrower far away
  const hx = clamp(t.x, wb.x, wb.x + wb.w), hy = clamp(t.y - t.c.hh / 2, wb.y, wb.y + wb.h);
  if (t.boss) { bossHurt(t, a, b.dmg, hx, hy, proj); return; }
  if (t.state === 'shield') {
    t.shieldHP -= b.dmg * 1.3; t.vx = dir * Math.min(3.2, 0.8 + b.dmg * 0.2); t.hitlag = 3; if (!proj) a.hitlag = 3;
    const broke = t.shieldHP <= 0;
    if (broke) { t.shieldHP = 30; t.setState('dizzy'); t.vy = -5; t.onGround = false; }
    shieldFX(hx, hy, broke); NET.event(['b', 0, Math.round(hx), Math.round(hy), broke ? 1 : 0]);
    return;
  }
  if (t.state === 'ledge' && t.ledge && t.ledge.busy === t) t.ledge.busy = null;
  t.dmg = Math.min(999, t.dmg + b.dmg);
  const wf = 100 / (t.c.weight * 0.5 + 50);
  let L = (b.kb + b.kbg * 1.3 * t.dmg / 100) * wf;
  let ang = b.ang * Math.PI / 180;
  if (b.pull) { L = 2.4; ang = Math.PI * 0.95; }
  t.vx = Math.cos(ang) * L * dir; t.vy = -Math.sin(ang) * L;
  if (t.onGround && t.vy > 0) t.vy = -t.vy * 0.6;
  if (t.vy < -1) t.onGround = false;
  t.stun = Math.floor(L * 3.4 + 4);
  t.setState(L > 5.6 ? 'tumble' : 'hurt'); t.move = null; t.sp = null; t.fastfall = false;
  t.lastHit = a; t.lastHitT = frameNo;
  const lag = Math.min(14, 3 + Math.floor(b.dmg * 0.4)); if (!proj) a.hitlag = lag; t.hitlag = lag;
  hitFX(hx, hy, b.dmg, L); NET.event(['h', 0, Math.round(hx), Math.round(hy), Math.round(b.dmg), Math.round(L * 10) / 10]);
}
// what a hit / a shield block / a KO looks and sounds like (the host plays them and sends them on; online players replay them)
function hitFX(hx, hy, dmg, L) {
  if (L > 8) { shake = Math.min(10, L * 0.8); audio.sfx.heavy(); fx({ type: 'text', s: pick(['POW!', 'WHAM!', 'BONK!', 'KAPOW!', 'MUCH HIT', 'SMASH!']), x: hx, y: hy - 18, life: 30, color: '#ffd23a' }); }
  else audio.sfx.hit(dmg);
  sparkBurst(hx, hy, 6 + dmg, L > 8 ? '#ffd23a' : '#ffffff', 0.6 + L * 0.08);
}
function shieldFX(hx, hy, broke) {
  audio.sfx.shield(); fx({ type: 'star', x: hx, y: hy, life: 6, size: 5, color: '#aee8ff' });
  if (broke) { audio.sfx.heavy(); sparkBurst(hx, hy, 16, '#ffffff', 1.4); }
}
function koFX(f, x, y, splat, word) {
  if (splat) {
    const cracks = Array.from({ length: 7 }, (_, i) => { const a = i / 7 * Math.PI * 2 + rnd(0.4); return Array.from({ length: 4 }, () => [Math.cos(a + rnd(-0.5, 0.5)) * rnd(8, 18), Math.sin(a + rnd(-0.5, 0.5)) * rnd(8, 18)]); });
    fx({ type: 'splat', f: { who: f.who }, x, y, cracks, life: 110, word });
    audio.sfx.splat(); shake = 12; flash = 4;
  } else {
    fx({ type: 'blast', x, y, ang: Math.atan2(y - H / 2, x - W / 2), color: f.color, life: 40 });
    audio.sfx.ko(); shake = 8; flash = 3;
  }
}

// ---- projectiles -----------------------------------------------------------------------------------------------------
function updateProj() {
  for (const p of PROJ) {
    p.life--;
    if (p.boomerang) { p.vx -= Math.sign(p.vx0 = p.vx0 ?? p.vx) * 0.16; if (p.life < 40 && Math.abs(p.x - p.owner.x) < 12 && Math.abs(p.y - (p.owner.y - 40)) < 30) p.life = 0; p.y += ((p.owner.y - 40) - p.y) * 0.03; if (p.vx * p.vx0 < 0 && !p.flipped) { p.flipped = true; p.hits = new Set(); } }
    if (p.g) p.vy += p.g;
    if (p.home) { const o = F.filter((t) => t !== p.owner && !t.dead && t.team !== p.owner.team).sort((u, v) => Math.abs(u.x - p.x) - Math.abs(v.x - p.x))[0];
      if (o) { const dx = o.x - p.x, dy = o.y - o.c.hh / 2 - p.y, d = Math.hypot(dx, dy) || 1, sp = Math.hypot(p.vx, p.vy) || 2; p.vx += (dx / d * sp - p.vx) * p.home; p.vy += (dy / d * sp - p.vy) * p.home; } }
    p.x += p.vx; p.y += p.vy;
    if (p.ground) { const M = STAGE.main; if (p.x < M.x1 || p.x > M.x2) p.life = 0; if (p.life % 3 === 0) dust(p.x, p.y, 1); }
    if (p.bounce) { const M = STAGE.main; if (p.y > M.y - p.h / 2 && p.y < M.y + 12 && p.x > M.x1 && p.x < M.x2 && p.vy > 0) { p.y = M.y - p.h / 2; p.vy *= -0.55; p.vx *= 0.85; if (Math.abs(p.vy) < 1) p.life = Math.min(p.life, 12); } }
    if (p.type === 'token') { const M = STAGE.main; if (p.y > M.y && p.x > M.x1 && p.x < M.x2) { p.y = M.y; p.vy *= -0.5; p.vx *= 0.8; if (Math.abs(p.vy) < 1) p.life = Math.min(p.life, 10); } }
    if (p.x < -60 || p.x > W + 60 || p.y > H + 40) p.life = 0;
  }
  PROJ = PROJ.filter((p) => p.life > 0);
}
function drawProj() {
  for (const p of PROJ) {
    const x = Math.round(p.x), y = Math.round(p.y), sp = (frameNo >> 2) % 4;
    if (p.type === 'coin') { const w = [5, 3, 1, 3][sp]; rect(ctx, x - w, y - 5, w * 2, 10, '#160e1e'); rect(ctx, x - w + 1, y - 4, Math.max(1, w * 2 - 2), 8, '#c8d4e8'); if (w > 2) rect(ctx, x - 1, y - 3, 2, 6, '#5a8ad8'); }
    else if (p.type === 'token') { rect(ctx, x - 5, y - 5, 10, 10, '#160e1e'); rect(ctx, x - 4, y - 4, 8, 8, '#e8b84a'); rect(ctx, x - 1, y - 2, 2, 4, '#7a4a1a'); rect(ctx, x - 2, y - 1, 4, 2, '#7a4a1a'); }
    else if (p.draw) p.draw(p, x, y);
    else if (p.type === 'wave') { for (let i = 0; i < 4; i++) rect(ctx, x - 6 + i * 3, y - 4 - (i % 2) * 5 - sp, 3, 4 + (i % 2) * 5, i % 2 ? '#ffd23a' : '#f7931a'); }
  }
}

// ---- CPU ---------------------------------------------------------------------------------------------------------------
function aiPad(f, level) {
  const A = f.ai, pad = { x: 0, y: 0 }, S = STAGE, M = S.main, c = f.c;
  const foes = F.filter((o) => o !== f && !o.dead && o.state !== 'respawn' && (!o.team || o.team !== f.team));
  const tgt = foes.sort((p, q) => Math.abs(p.x - f.x) + Math.abs(p.y - f.y) - Math.abs(q.x - f.x) - Math.abs(q.y - f.y))[0];
  A.t--;
  const off = f.x < M.x1 - 2 || f.x > M.x2 + 2 || (f.y > M.y + 4 && !f.onGround);
  // recovery comes first, whatever the level
  if (off && !f.onGround && f.state !== 'ledge') {
    pad.x = f.x < W / 2 ? 1 : -1;
    const below = f.y > M.y - 10;
    if (f.vy > 0.5 && below && f.jumps > 0 && f.state === 'air') pad.jumpP = true;
    else if (below && f.jumps === 0 && f.state === 'air' && f.y > M.y + 10) { pad.specialP = true; pad.y = -1; }
    return pad;
  }
  if (f.state === 'ledge') { if (f.t > 10 + (9 - level) * 4) { if (Math.random() < 0.5) pad.jumpP = true; else { pad.x = f.face; pad[f.face > 0 ? 'rightP' : 'leftP'] = true; } } return pad; }
  if (!tgt) return pad;
  const dx = tgt.x - f.x, dy = (tgt.y - tgt.c.hh / 2) - (f.y - c.hh / 2), dist = Math.max(0, Math.abs(dx) - (tgt.boss ? tgt.c.hw / 2 - 4 : 0));
  if (A.t > 0 && A.hold) { const h = A.hold; A.hold = { x: h.x, y: h.y, shield: h.shield }; return h; }
  A.t = Math.max(2, 16 - level * 1.5 + rnd(0, 6) | 0);
  const aggro = 0.25 + level * 0.07, reach = 18 + c.hw / 2 + 14 * c.reach;
  const tgtThreat = (tgt.state === 'attack' || tgt.state === 'special' || (tgt.boss && tgt.bs.atk && tgt.pose !== 'wind')) && dist < 50;
  if (tgtThreat && f.onGround && Math.random() < level * 0.045) { A.hold = { shield: true, x: 0, y: 0 }; A.t = 12; return A.hold; }
  if (dist > reach || Math.abs(dy) > 40) {
    pad.x = Math.sign(dx);
    const ranged = { lester: [70, 200], digi: [60, 210], arcade: [50, 140] }[f.who];
    if (ranged && dist > ranged[0] && dist < ranged[1] && Math.abs(dy) < 30 && Math.random() < aggro * 0.25) { pad.specialP = true; pad.x = Math.sign(dx); }
    else if (dy < -30 && f.onGround && Math.random() < 0.5) pad.jumpP = true;
    else if (f.who === 'shiba' && dist < 140 && Math.abs(dy) < 20 && Math.random() < aggro * 0.12) pad.specialP = true;
    else if (dy > 30 && f.plat && !f.plat.main && f.onGround) { pad.y = 1; pad.downP = true; pad.x = 0; }
  } else if (Math.random() < aggro) {
    f.face = Math.sign(dx) || f.face; pad.attackP = true;
    if (dy < -24) pad.y = -1;
    else if (!f.onGround && dy > 20) pad.y = 1;
    else { const r = Math.random(); if (r < (tgt.dmg > 90 ? 0.5 : 0.25)) pad.x = Math.sign(dx); else if (r < 0.4) pad.y = 1; else if (r < 0.48 && f.onGround) { pad.attackP = false; pad.jumpP = true; } else if (r < 0.56) { pad.attackP = false; pad.specialP = true; } }
  } else pad.x = Math.random() < 0.4 ? -Math.sign(dx) : 0;
  // never walk off the stage on purpose
  if (f.onGround && f.plat === M && ((f.x < M.x1 + 14 && pad.x < 0) || (f.x > M.x2 - 14 && pad.x > 0)) && !pad.attackP) pad.x = 0;
  A.hold = { x: pad.x, y: pad.y };
  return pad;
}

// ---- drawing a fighter -------------------------------------------------------------------------------------------------
function fighterAnim(f) {
  const st = f.state, t = f.t;
  if (st === 'attack') return [f.move.anim, moveFrame(f.move, f.mt)];
  if (st === 'special') return [f.spKey === 'up' ? 'upspecial' : 'special', f.spFrame || 0];
  if (st === 'run') return ['run', (frameNo / 5 | 0) % 6];
  if (st === 'idle' || st === 'land0') return ['idle', (frameNo / 9 | 0) % 4];
  if (st === 'crouch' || st === 'land' || st === 'roll' || st === 'squat') return ['crouch', 0];
  if (st === 'shield') return ['shield', 0];
  if (st === 'hurt') return ['hurt', t < 6 ? 0 : 1];
  if (st === 'tumble') return ['tumble', (t >> 3) % 2];
  if (st === 'dizzy') return ['hurt', (t >> 4) % 2];
  if (st === 'ledge' || st === 'helpless') return ['fall', 0];
  if (st === 'respawn') return ['idle', 0];
  if (!f.onGround) return f.vy < -1.5 ? ['jump', f.vy < -4 ? 0 : 1] : ['fall', 0];
  return ['idle', 0];
}
function drawFighter(f) {
  if (f.boss) return drawBoss(f);
  if (f.dead) return;
  const [anim, fi] = f.netAnim || fighterAnim(f);
  if (f.state === 'respawn') { rect(ctx, f.x - 18, f.y, 36, 4, f.color); rect(ctx, f.x - 14, f.y + 4, 28, 2, '#ffffff55'); }
  // tongue
  if (f.state === 'special' && f.who === 'pep3d' && f.spKey === 'neutral' && f.sp && f.sp.len > 0) {
    const y = f.y - 32, x0 = f.x + f.face * 6, x1 = x0 + f.face * f.sp.len;
    rect(ctx, Math.min(x0, x1), y - 2, Math.abs(x1 - x0), 5, '#160e1e'); rect(ctx, Math.min(x0, x1), y - 1, Math.abs(x1 - x0), 3, '#ff6a8a'); rect(ctx, x1 - 3, y - 3, 6, 7, '#ff6a8a');
  }
  const blink = f.netAnim ? f.netBlink : f.inv > 0 && (frameNo >> 2) % 2 && f.state !== 'ledge';
  const shakeX = f.hitlag > 0 && f.state !== 'attack' && f.state !== 'special' ? (frameNo % 2 ? 1 : -1) * 2 : 0;
  drawSprite(f.who, anim, fi, f.x + shakeX, f.y, f.face < 0, { alpha: blink ? 0.45 : 1 });
  if (f.state === 'dizzy') for (let i = 0; i < 3; i++) { const a = frameNo / 10 + i * 2.1; rect(ctx, f.x + Math.cos(a) * 12, f.y - f.c.hh - 6 + Math.sin(a) * 3, 3, 3, '#ffd23a'); }
  if (f.state === 'shield') { const r = 10 + f.c.hh * 0.42 * (0.4 + 0.6 * f.shieldHP / 60); ctx.globalAlpha = 0.35; ctx.fillStyle = f.color; ctx.beginPath(); ctx.arc(f.x, f.y - f.c.hh / 2, r, 0, Math.PI * 2); ctx.fill(); ctx.globalAlpha = 1; pixelCircle(f.x, f.y - f.c.hh / 2, r, '#ffffffaa'); }
  // player tag
  const tag = f.tag || (f.ctrl === 'cpu' ? (f.team === 'good' ? 'ALLY' : 'CPU') : 'P' + (f.slot + 1)), ty = f.y - (f.state === 'ledge' ? f.c.hh * 0.3 : f.c.hh) - 14;
  text(tag, f.x, ty, { align: 'center', color: f.color, outline: '#160e1e', shadow: false });
  rect(ctx, f.x - 1, ty + 8, 3, 2, f.color);
  // off-screen pointer bubble
  if (f.x < 0 || f.x > W || f.y < 0) {
    const bx = clamp(f.x, 14, W - 14), by = clamp(f.y - f.c.hh / 2, 14, H - 30);
    ctx.fillStyle = '#160e1e'; ctx.beginPath(); ctx.arc(bx, by, 12, 0, Math.PI * 2); ctx.fill(); pixelCircle(bx, by, 12, f.color);
    ctx.save(); ctx.beginPath(); ctx.arc(bx, by, 11, 0, Math.PI * 2); ctx.clip(); drawSprite(f.who, anim, fi, bx, by + 12, f.face < 0, { scale: 0.5 }); ctx.restore();
  }
}

// ---- KOs ---------------------------------------------------------------------------------------------------------------
const BLAST = { l: -56, r: W + 56, t: -84, b: H + 44 };
function checkKO(f, match) {
  if (f.dead || f.state === 'respawn' || f.boss) return;
  // the top only KOs someone who was knocked up there; flying up on your own just bumps the ceiling
  if (f.y < BLAST.t && f.state !== 'tumble' && f.state !== 'hurt') { f.y = BLAST.t; f.vy = Math.max(0, f.vy); }
  const out = f.x < BLAST.l ? 'l' : f.x > BLAST.r ? 'r' : f.y > BLAST.b ? 'b' : f.y < BLAST.t ? 't' : null;
  if (!out) return;
  const speed = Math.hypot(f.vx, f.vy), ex = clamp(f.x, 0, W), ey = clamp(f.y - f.c.hh / 2, 0, H);
  // sometimes they smack right into the screen (fast launches off the top or sides)
  const splat = out !== 'b' && speed > 3.2 && Math.random() < (out === 't' ? 0.55 : 0.35);
  const wi = Math.floor(Math.random() * NET_WORDS.length), kx = splat ? clamp(f.x, 80, W - 80) : ex, ky = splat ? clamp(f.y, 110, H - 50) : ey;
  koFX(f, kx, ky, splat, NET_WORDS[wi]); NET.event(['k', F.indexOf(f), Math.round(kx), Math.round(ky), splat ? 1 : 0, wi]);
  STATS.kos++; if (splat) STATS.splats++;
  match.onKO(f, out, splat);
}
