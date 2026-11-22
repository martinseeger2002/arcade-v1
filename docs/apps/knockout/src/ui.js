// ui.js - scenes: title, main menu, character select, stage select, the fight, results, how to play, story (stub).
'use strict';
let scene = null, sceneT = 0;
function go(s, ...args) { scene = s; sceneT = 0; if (s.enter) s.enter(...args); }
const P1 = () => menuPad();
function button(x, y, w, h, label, o = {}) {   // a clickable box; returns true when clicked this frame
  const hot = o.sel;
  rect(ctx, x, y, w, h, hot ? (o.hotBg || '#ff5fd2') : (o.bg || '#2a1a40')); rect(ctx, x, y + h - 2, w, 2, hot ? '#ffffff' : '#160e1e');
  text(label, x + w / 2, y + (h - 7 * (o.size || 1)) / 2, { align: 'center', color: hot ? '#160e1e' : (o.color || '#ffffff'), size: o.size || 1, shadow: !hot });
  return clicks.some((c) => c.x >= x && c.x < x + w && c.y >= y && c.y < y + h);
}
function backdrop(t) {
  gradSky(ctx, ['#120a20', '#24123c', '#3a1a5a']);
  for (let i = 0; i < 40; i++) { const x = (i * 97 + t * (0.2 + (i % 3) * 0.15)) % (W + 20) - 10, y = (i * 53) % H; rect(ctx, x, y, 1 + (i % 2), 1, '#ffffff44'); }
}
function logo(y, k = 3) {
  text('ARCADE', W / 2, y, { size: k, align: 'center', color: '#ffd23a', outline: '#160e1e' });
  text('KNOCKOUT', W / 2, y + 8 * k + 2, { size: k + 1, align: 'center', color: '#ff5fd2', outline: '#160e1e' });
}

// ---- title ---------------------------------------------------------------------------------------------------------
const Title = {
  enter() { audio.play(STAGES.arcade.song); },
  update() {
    backdrop(sceneT); logo(34, 4);
    ROSTER.forEach((w, i) => drawSprite(w, 'idle', (sceneT / 9 | 0 + i) % 4, 70 + i * 68, 214, i >= 3));
    rect(ctx, 0, 214, W, 2, '#ff5fd2');
    if ((sceneT >> 5) % 2) text('PRESS ENTER OR CLICK', W / 2, 234, { align: 'center', color: '#ffffff' });
    text('PUBLISHED BY @YOURFIRSTNAME ON ARCADE', W / 2, 256, { align: 'center', color: '#9a86c0' });
    const p = P1();
    if (p.startP || p.attackP || p.jumpP || clicks.length || padsConnected().some((g) => readDevice(g).startP)) { audio.sfx.ok(); go(Menu); }
  },
};

// ---- main menu -------------------------------------------------------------------------------------------------------
const Menu = {
  sel: 0,
  items: [['VS MODE', () => go(CharSelect)], ['ONLINE', () => go(Online)], ['STORY MODE', () => go(StoryMenu)], ['HOW TO PLAY', () => go(HowTo)], ['SOUND', () => audio.toggle()]],
  update() {
    backdrop(sceneT); logo(24, 3);
    const p = P1();
    if (p.downP) { this.sel = (this.sel + 1) % this.items.length; audio.sfx.select(); }
    if (p.upP) { this.sel = (this.sel + this.items.length - 1) % this.items.length; audio.sfx.select(); }
    this.items.forEach(([label, fn], i) => {
      const l = label === 'SOUND' ? 'SOUND: ' + (audio.muted ? 'OFF' : 'ON') : label;
      if (button(170, 92 + i * 26, 140, 20, l, { sel: i === this.sel, size: 1 })) { this.sel = i; audio.sfx.ok(); fn(); }
    });
    if (p.startP || p.attackP) { audio.sfx.ok(); this.items[this.sel][1](); }
    if (p.backP) go(Title);
    drawSprite(ROSTER[(sceneT / 120 | 0) % 6], 'win', (sceneT >> 4) % 2, 80, 230);
    drawSprite(ROSTER[(sceneT / 120 + 3 | 0) % 6], 'idle', (sceneT >> 3) % 4, 400, 230, true);
  },
};

// ---- character select ---------------------------------------------------------------------------------------------------
const RULES = { mode: 'stock', stocks: 3, minutes: 2 };
const CharSelect = {
  slots: null,
  enter() {
    if (!this.slots) this.slots = [
      { ctrl: 'k1', who: 'buster', ready: false, level: 5 }, { ctrl: 'cpu', who: 'random', ready: true, level: 5 },
      { ctrl: 'off', who: 'random', ready: true, level: 5 }, { ctrl: 'off', who: 'random', ready: true, level: 5 }];
    for (const s of this.slots) if (s.ctrl !== 'cpu' && s.ctrl !== 'off') s.ready = false;
  },
  claim(dev) {   // a new keyboard / pad joins: take the first CPU or empty slot
    if (this.slots.some((s) => s.ctrl === dev)) return;
    const s = this.slots.find((s, i) => i > 0 && s.ctrl === 'off') || this.slots.find((s, i) => i > 0 && s.ctrl === 'cpu');
    if (s) { s.ctrl = dev; s.ready = false; s.joinT = sceneT; if (s.who === 'random') s.who = ROSTER[0]; audio.sfx.ok(); }
  },
  update() {
    backdrop(sceneT);
    text('CHOOSE YOUR FIGHTER', W / 2, 8, { size: 2, align: 'center', color: '#ffd23a' });
    // rules
    const rl = RULES.mode === 'stock' ? `STOCK: ${RULES.stocks}` : `TIME: ${RULES.minutes}:00`;
    if (button(8, 8, 92, 16, rl) || keysP.has('KeyR')) {   // STOCK 1..5, then TIME 1..5 minutes, then around
      const opts = [1, 2, 3, 4, 5].map((n) => ['stock', n]).concat([1, 2, 3, 4, 5].map((n) => ['time', n]));
      const k = opts.findIndex(([m, n]) => m === RULES.mode && n === (m === 'stock' ? RULES.stocks : RULES.minutes));
      const [m, n] = opts[(k + 1) % opts.length]; RULES.mode = m; if (m === 'stock') RULES.stocks = n; else RULES.minutes = n; audio.sfx.select();
    }
    if (button(W - 100, 8, 92, 16, 'BACK') ) { go(Menu); return; }
    // roster strip
    ROSTER.forEach((w, i) => {
      const x = 48 + i * 66, y = 32;
      rect(ctx, x, y, 54, 54, '#1a0f2e'); rect(ctx, x, y + 52, 54, 2, CHARS[w].color);
      ctx.save(); ctx.beginPath(); ctx.rect(x, y, 54, 52); ctx.clip(); drawSprite(w, 'idle', 0, x + 27, y + 62); ctx.restore();
      text(CHARS[w].name, x + 27, y + 56 + 2, { align: 'center', color: CHARS[w].color });
      if (clicks.some((c) => c.x >= x && c.x < x + 54 && c.y >= y && c.y < y + 54)) { const s = this.slots.find((s) => s.ctrl === 'k1') || this.slots[0]; s.who = w; s.ready = false; audio.sfx.select(); }
    });
    // join with other devices
    for (const g of padsConnected()) { const d = readDevice(g); if (d.startP || d.jumpP) this.claim(g); }
    if (keysP.has('Comma') || keysP.has('Numpad1')) this.claim('k2');
    // slots
    const live = padsConnected();
    for (const s of this.slots) if (s.ctrl[0] === 'g' && !live.includes(s.ctrl)) { s.ctrl = 'off'; s.ready = true; }   // a pad was unplugged
    this.slots.forEach((s, i) => {
      const x = 8 + i * 118, y = 106, w = 110;
      rect(ctx, x, y, w, 132, s.ctrl === 'off' ? '#160e1e' : '#22143a'); rect(ctx, x, y, w, 14, s.ctrl === 'off' ? '#2a1a40' : PCOLORS[i]);
      const head = s.ctrl === 'off' ? `P${i + 1}: OFF` : s.ctrl === 'cpu' ? `CPU  LV${s.level}` : `P${i + 1}  ${s.ctrl[0] === 'k' ? LAYOUTS[s.ctrl].name : 'PAD ' + (+s.ctrl[1] + 1)}`;
      text(head, x + w / 2, y + 4, { align: 'center', color: '#160e1e', shadow: false });
      if (clicks.some((c) => c.x >= x && c.x < x + w && c.y >= y && c.y < y + 14) || keysP.has('Digit' + (i + 1))) {   // cycle OFF -> CPU -> (back to the device that had it)
        if (i === 0 && s.ctrl === 'k1') { s.ctrl = 'cpu'; s.ready = true; } else if (s.ctrl === 'off') { s.ctrl = 'cpu'; s.ready = true; } else if (s.ctrl === 'cpu') { s.ctrl = i === 0 ? 'k1' : 'off'; s.ready = i !== 0; } else { s.ctrl = 'cpu'; s.ready = true; }
        audio.sfx.select();
      }
      if (s.ctrl === 'off') { text('CLICK OR PRESS', x + w / 2, y + 56, { align: 'center', color: '#6a5a8a' }); text(`${i + 1} TO ADD CPU`, x + w / 2, y + 66, { align: 'center', color: '#6a5a8a' }); return; }
      const human = s.ctrl !== 'cpu';
      if (human) {
        const d = readDevice(s.ctrl);
        if (!s.ready && s.joinT !== sceneT) {   // not on the frame it joined (the join key is also its attack key)
          let k = s.who === 'random' ? -1 : ROSTER.indexOf(s.who);
          if (d.leftP) { k = (k + ROSTER.length - 1 + (k < 0 ? 1 : 0)) % ROSTER.length; s.who = ROSTER[k]; audio.sfx.select(); }
          if (d.rightP) { k = (k + 1) % ROSTER.length; s.who = ROSTER[k]; audio.sfx.select(); }
          if (d.attackP || d.jumpP && s.ctrl[0] === 'g') { s.ready = true; audio.sfx.ok(); }
        } else if (d.specialP || d.backP) { s.ready = false; audio.sfx.select(); }
      }
      // arrows (mouse) to change fighter / CPU level
      if (button(x + 2, y + 50, 14, 20, '<')) { const k = s.who === 'random' ? 0 : ROSTER.indexOf(s.who); s.who = ROSTER[(k + ROSTER.length - 1) % ROSTER.length]; audio.sfx.select(); }
      if (button(x + w - 16, y + 50, 14, 20, '>')) { const k = s.who === 'random' ? -1 : ROSTER.indexOf(s.who); s.who = ROSTER[(k + 1) % ROSTER.length]; audio.sfx.select(); }
      if (s.ctrl === 'cpu') {
        if (button(x + 4, y + 114, 30, 14, 'LV-')) s.level = Math.max(1, s.level - 1);
        if (button(x + w - 34, y + 114, 30, 14, 'LV+')) s.level = Math.min(9, s.level + 1);
        if (button(x + 38, y + 114, 34, 14, '?')) s.who = 'random';
      }
      if (s.who === 'random') { text('?', x + w / 2, y + 44, { size: 4, align: 'center', color: '#9a86c0' }); text('RANDOM', x + w / 2, y + 92, { align: 'center' }); }
      else {
        const C = CHARS[s.who];
        drawSprite(s.who, s.ready ? 'win' : 'idle', s.ready ? (sceneT >> 4) % 2 : (sceneT / 9 | 0) % 4, x + w / 2, y + 86);
        text(C.name, x + w / 2, y + 92, { align: 'center', color: C.color });
        text(C.tag, x + w / 2, y + 102, { align: 'center', color: '#9a86c0' });
      }
      if (human) text(s.ready ? 'READY!' : 'ATTACK = READY', x + w / 2, y + 118, { align: 'center', color: s.ready ? '#5aff8a' : '#ffffff' });
    });
    const active = this.slots.filter((s) => s.ctrl !== 'off');
    const allReady = active.length >= 2 && active.every((s) => s.ready);
    text('P1: A/D PICK · C READY · 1-4 SLOT ON/OFF · R RULES', W / 2, 238, { align: 'center', color: '#9a86c0' });
    text('2ND KEYBOARD: PRESS ,  ·  GAMEPADS: PRESS A' + (padsBlocked ? ' (BLOCKED BY THIS WINDOW)' : ''), W / 2, 246, { align: 'center', color: '#9a86c0' });
    if (allReady) { if (button(170, 254, 140, 15, 'FIGHT!  (ENTER)', { sel: (sceneT >> 3) % 2 }) || P1().startP) { audio.sfx.ok(); go(StageSelect); } }
    else text(active.length < 2 ? 'ADD AT LEAST TWO FIGHTERS' : 'WAITING FOR PLAYERS...', W / 2, 258, { align: 'center', color: '#6a5a8a' });
    if (P1().backP) go(Menu);
  },
};

// ---- stage select ----------------------------------------------------------------------------------------------------------
const thumbs = {};
const StageSelect = {
  sel: 0,
  update() {
    backdrop(sceneT);
    text('CHOOSE A STAGE', W / 2, 10, { size: 2, align: 'center', color: '#ffd23a' });
    const p = P1();
    if (p.rightP) { this.sel = (this.sel + 1) % 5; audio.sfx.select(); } if (p.leftP) { this.sel = (this.sel + 4) % 5; audio.sfx.select(); }
    if (p.downP) { this.sel = Math.min(4, this.sel + 2); audio.sfx.select(); } if (p.upP) { this.sel = Math.max(0, this.sel - 2); audio.sfx.select(); }
    let picked = null;
    STAGE_ORDER.forEach((k, i) => {
      const s = STAGES[k], x = 40 + (i % 2) * 210, y = 34 + (i / 2 | 0) * 98;
      if (!thumbs[k]) thumbs[k] = offscreen(W, H, (g) => { stageReset(s); s.paint.call(s, g); });
      rect(ctx, x - 3, y - 3, 186, 95, i === this.sel ? '#ff5fd2' : '#2a1a40');
      ctx.drawImage(thumbs[k], 0, 0, W, H, x, y, 180, 76);
      text(s.name, x + 90, y + 80, { align: 'center', color: i === this.sel ? '#ffd23a' : '#ffffff' });
      if (clicks.some((c) => c.x >= x && c.x < x + 180 && c.y >= y && c.y < y + 89)) { this.sel = i; picked = k; }
    });
    if (button(190, 232, 100, 16, 'RANDOM', { sel: this.sel === 4 })) picked = pick(STAGE_ORDER);
    const cur = STAGE_ORDER[this.sel];
    text(cur ? STAGES[cur].blurb : 'Let the arcade decide.', W / 2, 254, { align: 'center', color: '#9a86c0' });
    if (p.startP || p.attackP) picked = cur || pick(STAGE_ORDER);
    if (p.backP || p.specialP) go(CharSelect);
    if (picked) { audio.sfx.ok(); go(Fight, picked); }
  },
};

// ---- the fight --------------------------------------------------------------------------------------------------------------
let MATCH = null;
const Fight = {
  enter(stageKey, story, net) {
    STAGE = STAGES[stageKey]; stageReset(STAGE); FX = []; PROJ = []; frameNo = 0; shake = 0; flash = 0;
    if (!STAGE.bg) STAGE.bg = offscreen(W, H, (g) => STAGE.paint.call(STAGE, g));
    this.story = story || null; this.net = net || null; this.leaveT = 0;
    if (net) {   // online: everyone builds the same fighters from the host's roster; only the host simulates them
      const myId = NET.me && NET.me.id;
      F = net.roster.map((r, k) => { const [id, who, level] = r, f = new Fighter(k, ROSTER.includes(who) ? who : 'buster', 'cpu');
        if (id === 'cpu') { f.level = level || 6; f.tag = 'CPU'; }
        else if (id === myId) { f.ctrl = net.host ? 'p1' : 'netview'; f.tag = 'YOU'; }
        else { f.ctrl = net.host ? 'net' : 'netview'; f.netId = id; f.tag = 'P' + (k + 1); }
        f.reset(...STAGE.spawn[k]); f.stocks = net.stocks || 3; if (!net.host) { f.x = undefined; } return f; });
      if (!net.host) F.forEach((f, k) => { f.x = STAGE.spawn[k][0]; f.y = STAGE.spawn[k][1]; });
    } else if (story) {   // story: a hero (+ allies) on the left, the bad team on the right
      const M = STAGE.main; let gi = 0, bi = 0;
      F = story.roster.map((r, k) => {
        if (r.boss) { const b = new Boss(k, r.boss, { ...r, x: r.x ?? M.x2 - 60 - bi++ * 60 }); return b; }
        const f = new Fighter(k, r.who, r.ctrl); f.level = r.level || 6; f.team = r.team; f.stocks = r.stocks ?? 1;
        f.reset(r.team === 'good' ? M.x1 + 40 + gi++ * 32 : M.x2 - 40 - bi++ * 32, M.y); return f;
      });
    } else {
      const slots = CharSelect.slots.filter((s) => s.ctrl !== 'off');
      F = slots.map((s) => { const i = CharSelect.slots.indexOf(s); const f = new Fighter(i, s.who === 'random' ? pick(ROSTER) : s.who, s.ctrl); f.level = s.level; return f; });
      F.forEach((f, k) => { f.reset(...STAGE.spawn[k]); f.stocks = RULES.mode === 'stock' ? RULES.stocks : 99; });
    }
    MATCH = { net: net || null, mode: story ? 'story' : net ? 'stock' : RULES.mode, timeLeft: story ? (story.time || 0) * 60 : RULES.minutes * 60 * 60, timeTotal: story ? (story.time || 0) * 60 : 0, timed: !!(story && story.time), clock: story && story.clock,
      countdown: 200, end: 0, order: [], paused: false, win: false, onKO: (f, out, splat) => this.onKO(f, out, splat) };
    audio.play(story ? (story.song || (F.some((f) => f.boss) ? BOSS_SONG : STAGE.song)) : STAGE.song);
  },
  onKO(f, out, splat) {
    const credit = f.lastHit && f.lastHit !== f && frameNo - f.lastHitT < 480 ? f.lastHit : null;
    if (credit) { credit.kos++; credit.score++; }
    f.falls++; f.score--; f.dead = true; f.respawnT = splat ? 120 : 80;
    if (f.ledge && f.ledge.busy === f) f.ledge.busy = null;
    if (MATCH.mode !== 'time') { f.stocks--; if (f.stocks <= 0) { f.out = true; f.outAt = frameNo; MATCH.order.unshift(f); } }
  },
  update() {
    const M = MATCH;
    if (M.net) NET.watch();
    if (scene !== Fight) return;
    if (M.net) {   // online: no pausing; Esc twice leaves the match
      if (keysP.has('Escape')) { if (this.leaveT > 0) { this.leaveNet(); return; } this.leaveT = 120; }
      if (this.leaveT > 0) this.leaveT--;
      if (!M.net.host) { NET.clientTick(); if (scene === Fight) this.draw(); return; }
    }
    if (!M.paused && !M.end) {
      if (M.countdown > 0) M.countdown--;
      else {
        frameNo++;
        stageTick(STAGE, frameNo);
        for (const f of F) {
          if (f.boss) { if (!f.dead) updateBoss(f); continue; }
          if (f.dead) { if (!f.out && --f.respawnT <= 0) { f.reset(W / 2 + (f.slot - 1.5) * 30, 60); f.dmg = 0; f.setState('respawn'); f.onGround = true; } continue; }
          const pad = f.ctrl === 'cpu' ? aiPad(f, f.level) : f.ctrl === 'p1' ? heroPad() : f.ctrl === 'net' ? NET.padFor(f.netId) : readDevice(f.ctrl);
          updateFighter(f, pad);
        }
        updateProj(); resolveHits(); for (const f of F) checkKO(f, M);
        if (M.mode === 'story') {
          if (F.filter((f) => f.team === 'bad').every((f) => f.out)) { M.end = 1; M.win = true; }
          else if ((F.some((f) => f.team === 'good' && f.ctrl !== 'cpu') ? F.filter((f) => f.team === 'good' && f.ctrl !== 'cpu') : F.filter((f) => f.team === 'good')).every((f) => f.out)) { M.end = 1; M.win = false; }
          else if (M.timed && --M.timeLeft <= 0) { M.end = 1; M.win = !!this.story.survive; M.survived = M.win; }
          if (M.end) audio.sfx[M.win ? 'go' : 'ko']();
        }
        else if (M.mode === 'time') { M.timeLeft--; if (M.timeLeft <= 0) M.end = 1; }
        else if (F.filter((f) => !f.out).length <= 1) M.end = 1;
      }
      const pd = menuPad(); const anyStart = !M.net && (pd.startP || F.some((f) => f.ctrl !== 'cpu' && f.ctrl !== 'p1' && f.ctrl !== 'net' && readDevice(f.ctrl).startP));
      if (!M.net && (anyStart || keysP.has('Escape') || keysP.has('KeyP')) && !M.countdown) { M.paused = true; audio.sfx.select(); }
    } else if (M.paused) {
      const pd = P1();
      if (pd.startP || keysP.has('KeyP')) M.paused = false;
      if (keysP.has('Escape') || keysP.has('KeyQ')) { audio.play(STAGES.arcade.song); go(M.mode === 'story' ? StoryMenu : Menu); return; }
    } else if (M.end) { M.end++; if (M.end > 150) { if (M.net) { NET.sendSnap(); go(Results); return; } if (M.mode === 'story') { if (M.win) storyNext(); else go(Continue, 'fight'); } else go(Results); return; } }
    if (M.net && M.net.host && sceneT % 2 === 0) NET.sendSnap();
    this.draw();
  },
  leaveNet() {
    const M = MATCH;
    if (M.net.host) NET.send(['X', M.net.mid], true); else NET.send(['q', M.net.mid], true);
    NET.match = null; NET.lobby.phase = 'lobby'; NET.lobbyDirty = true; go(Online);
  },
  draw() {
    const M = MATCH;
    ctx.save();
    if (shake > 0) { ctx.translate(rnd(-shake, shake) | 0, rnd(-shake, shake) | 0); shake *= 0.85; if (shake < 0.5) shake = 0; }
    ctx.drawImage(STAGE.bg, 0, 0); STAGE.live(frameNo);
    const S = this.story;
    if (S && S.crowd) S.crowd.forEach((w, i) => { const x = [24, 58, 422, 456][i], y = STAGE.main.y; if (CHARS[w]) drawSprite(w, 'win', ((frameNo >> 4) + i) % 2, x, y, i >= 2, { scale: 0.7 }); else drawNPC(w, x, y, i >= 2 ? -1 : 1, frameNo, 'wave'); });
    drawProj();
    const order = [...F].sort((a, b) => (a.state === 'attack' || a.state === 'special') - (b.state === 'attack' || b.state === 'special'));
    for (const f of order) drawFighter(f);
    drawFX();
    if (S && S.weather === 'snow') for (let i = 0; i < 60; i++) rect(ctx, (i * 53 + frameNo * 0.3 * (1 + i % 3)) % W, (i * 31 + frameNo * (0.5 + (i % 4) * 0.2)) % H, 2, 2, '#ffffffaa');
    ctx.restore();
    if (flash > 0) { ctx.fillStyle = '#ffffff'; ctx.globalAlpha = flash / 6; ctx.fillRect(0, 0, W, H); ctx.globalAlpha = 1; flash--; }
    hud();
    if (M.countdown > 0) { const n = Math.ceil(M.countdown / 60) - 0; const s = M.countdown > 20 ? String(Math.min(3, Math.ceil((M.countdown - 20) / 60))) : 'GO!';
      if (M.countdown % 60 === 20 && M.countdown > 20) audio.sfx.select(); if (M.countdown === 20) audio.sfx.go();
      text(s, W / 2, 96, { size: 5, align: 'center', color: s === 'GO!' ? '#5aff8a' : '#ffd23a', outline: '#160e1e' }); }
    if (S && (M.countdown > 0 || frameNo < 150)) { rect(ctx, 0, 124, W, 18, '#160e1ecc'); text(S.goal, W / 2, 129, { align: 'center', color: '#ffd23a' }); }
    if (S && S.hint && frameNo < 420 && M.countdown === 0) { const w = textW(S.hint) + 12; rect(ctx, W / 2 - w / 2, 200, w, 13, '#160e1ecc'); text(S.hint, W / 2, 203, { align: 'center', color: '#5aebf5' }); }
    if (M.end) { const s = M.mode !== 'story' ? 'GAME!' : M.survived ? 'YOU MADE IT!' : M.win ? 'VICTORY!' : 'DEFEATED...';
      text(s, W / 2, 96, { size: s.length > 8 ? 4 : 5, align: 'center', color: M.mode === 'story' && !M.win ? '#ff4a5a' : M.mode === 'story' ? '#5aff8a' : '#ff5fd2', outline: '#160e1e' }); }
    if (M.net && this.leaveT > 0) text('PRESS ESC AGAIN TO LEAVE THE MATCH', W / 2, 60, { align: 'center', color: '#ff8a3a', outline: '#160e1e' });
    if (M.net && !M.net.host && !NET.snap) text('WAITING FOR THE HOST...', W / 2, 150, { align: 'center', color: '#ffffff', outline: '#160e1e' });
    if (NET.noteT > 0 && M.net) { NET.noteT--; text(NET.note, W / 2, 46, { align: 'center', color: '#ff8a3a', outline: '#160e1e' }); }
    if (M.paused) { ctx.fillStyle = '#000000aa'; ctx.fillRect(0, 0, W, H); text('PAUSED', W / 2, 100, { size: 3, align: 'center' }); text('ENTER/P: RESUME · ESC/Q: QUIT', W / 2, 130, { align: 'center', color: '#9a86c0' }); }
  },
};
function hud() {
  const M = MATCH, P = F.filter((f) => !f.boss), n = P.length, pw = 100, gap = (W - n * pw) / (n + 1);
  const bosses = F.filter((f) => f.boss);
  bosses.forEach((b, i) => { const w = 150, x = 8, y = 4 + i * 17;
    text(b.c.name, x, y, { color: b.color }); rect(ctx, x, y + 8, w, 6, '#160e1e'); rect(ctx, x + 1, y + 9, (w - 2) * b.hp / b.hpMax, 4, b.rage ? '#ff4a5a' : b.color); });
  if (M.timed) { const k = Math.max(0, M.timeLeft) / M.timeTotal, mins = Math.ceil(k * 300), s = `${M.clock || 'TIME'} ${Math.floor(mins / 60)}:${String(mins % 60).padStart(2, '0')}`;
    text(s, W - 8, 6, { align: 'right', color: k < 0.15 ? '#5aff8a' : '#ffffff', outline: '#160e1e' }); }
  P.forEach((f, i) => {
    const x = Math.round(gap + i * (pw + gap)), y = H - 30;
    rect(ctx, x, y, pw, 28, '#160e1ecc'); rect(ctx, x, y, 3, 28, f.color);
    ctx.save(); ctx.beginPath(); ctx.rect(x + 4, y + 1, 26, 26); ctx.clip(); drawSprite(f.who, f.out ? 'hurt' : 'idle', 0, x + 17, y + 2 + Math.min(f.c.hh * 0.75, 42), false, { scale: 0.75, alpha: f.out ? 0.4 : 1 }); ctx.restore();
    text(f.c.name, x + 32, y + 3, { color: f.color });
    const d = Math.round(f.dmg), col = d < 50 ? '#ffffff' : d < 100 ? '#ffd23a' : d < 150 ? '#ff8a3a' : '#ff3a4a';
    const jig = f.hitlag > 0 ? rnd(-1, 1) | 0 : 0;
    if (!f.out) text(d + '%', x + pw - 4 + jig, y + 12, { size: 2, align: 'right', color: col, outline: '#160e1e' });
    else text('OUT', x + pw - 4, y + 12, { size: 2, align: 'right', color: '#6a5a8a' });
    if (M.mode !== 'time') for (let s = 0; s < Math.min(f.stocks, 5); s++) rect(ctx, x + 32 + s * 6, y + 12, 4, 4, f.color);
    else text((f.score >= 0 ? '+' : '') + f.score, x + 32, y + 12, { color: '#ffffff' });
  });
  if (M.mode === 'time') { const s = Math.max(0, Math.ceil(M.timeLeft / 60)); text(`${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`, W / 2, 6, { size: 2, align: 'center', color: s <= 10 ? '#ff3a4a' : '#ffffff', outline: '#160e1e' }); }
}

// ---- results ---------------------------------------------------------------------------------------------------------------
const Results = {
  enter() {
    const M = MATCH;
    if (M.mode === 'stock') { const alive = F.filter((f) => !f.out).sort((a, b) => b.stocks - a.stocks || a.dmg - b.dmg); this.rank = [...alive, ...M.order]; }
    else this.rank = [...F].sort((a, b) => b.score - a.score || a.dmg - b.dmg);
    this.tie = M.mode === 'time' ? this.rank.length > 1 && this.rank[0].score === this.rank[1].score
      : this.rank.length > 1 && this.rank[0].out && this.rank[1].out && this.rank[0].outAt === this.rank[1].outAt;   // last two KO'd on the same frame
    audio.play({ bpm: 140, bass: [48, 0, 52, 0, 55, 0, 52, 0], lead: [72, 76, 79, 84, 0, 79, 84, 0, 0, 0, 0, 0, 0, 0, 0, 0] });
  },
  update() {
    backdrop(sceneT);
    const w = this.rank[0];
    text(this.tie ? 'DRAW!' : `${w.c.name} WINS!`, W / 2, 14, { size: 3, align: 'center', color: this.tie ? '#ffffff' : w.color, outline: '#160e1e' });
    if (!this.tie) text({ buster: 'NUMBER GO UP.', lester: 'FASTER BLOCKS, FASTER FISTS.', shiba: 'MUCH WIN. VERY SKILL.', pep3d: 'FEELS GOOD MAN.', digi: 'FIVE ALGOS. ONE WINNER.', arcade: 'THE HOUSE ALWAYS WINS.' }[w.who], W / 2, 42, { align: 'center', color: '#9a86c0' });
    this.rank.forEach((f, i) => {
      const x = W / 2 + (i - (this.rank.length - 1) / 2) * 110, y = 170 - (i === 0 ? 20 : 0);
      rect(ctx, x - 40, y, 80, 80, i === 0 ? '#3a1f58' : '#22143a'); rect(ctx, x - 40, y, 80, 3, f.color);
      drawSprite(f.who, i === 0 && !this.tie ? 'win' : 'idle', i === 0 ? (sceneT >> 4) % 2 : (sceneT / 9 | 0) % 4, x, y);
      text(`${i + 1}${['ST', 'ND', 'RD', 'TH'][Math.min(i, 3)]}`, x, y + 8, { size: 2, align: 'center', color: i === 0 ? '#ffd23a' : '#ffffff' });
      text((f.ctrl === 'cpu' ? 'CPU ' : 'P' + (f.slot + 1) + ' ') + f.c.name, x, y + 30, { align: 'center', color: f.color });
      text(`KOS ${f.kos}  FALLS ${f.falls}`, x, y + 42, { align: 'center', color: '#9a86c0' });
      if (MATCH.mode === 'time') text(`SCORE ${f.score >= 0 ? '+' : ''}${f.score}`, x, y + 54, { align: 'center' });
    });
    if (MATCH.net && sceneT % 60 === 0) { const mp = NET.myPick(); NET.send(['p', mp.who, 0]); NET.watch(); }   // still here
    if (sceneT > 60 && MATCH.net) { text('ENTER: BACK TO THE LOBBY', W / 2, 258, { align: 'center', color: '#ffffff' });
      if (P1().startP || P1().attackP || P1().backP || clicks.length) { NET.match = null; NET.lobby.phase = 'lobby'; NET.lobbyDirty = true; go(Online); } return; }
    if (sceneT > 60) { text('ENTER: REMATCH · ESC: CHARACTERS', W / 2, 258, { align: 'center', color: '#ffffff' });
      const p = P1(); if (p.startP || p.attackP) go(Fight, STAGE_ORDER.find((k) => STAGES[k] === STAGE)); if (p.backP || clicks.length) go(CharSelect); }
  },
};

// ---- how to play -------------------------------------------------------------------------------------------------------------
const HowTo = {
  update() {
    backdrop(sceneT);
    text('HOW TO PLAY', W / 2, 10, { size: 2, align: 'center', color: '#ffd23a' });
    const L = [
      'HIT PEOPLE TO RAISE THEIR %. THE HIGHER IT IS, THE FARTHER THEY FLY.',
      'KNOCK THEM OFF THE SCREEN TO TAKE A STOCK. FAST ONES HIT THE GLASS.',
      'STOCK: LAST ONE STANDING WINS.  TIME: +1 PER KO, -1 PER FALL.', '',
      'ATTACK + DIRECTION: JAB, SMASH (SIDE), UP, LOW. IN THE AIR: KICKS.',
      'SPECIAL: YOUR SIGNATURE MOVE.  UP + SPECIAL: RECOVERY.',
      'SHIELD BLOCKS (IT SHRINKS). SHIELD + LEFT/RIGHT ROLLS.',
      'JUMP TWICE (PEP: THREE). GRAB LEDGES BY FALLING NEAR THEM.', '',
      'KEYS 1: ' + LAYOUTS.k1.help, 'KEYS 2: ' + LAYOUTS.k2.help, 'PADS: STICK MOVE · A JUMP · X ATTACK · B SPECIAL · SHOULDERS SHIELD', '',
    ];
    L.forEach((l, i) => text(l, 16, 40 + i * 12, { color: l.startsWith('KEYS') || l.startsWith('PADS') ? '#5aebf5' : '#ffffff' }));
    ROSTER.forEach((w, i) => { const C = CHARS[w]; text(`${C.name}: ${C.special} / ${C.up}`, 16 + (i % 2) * 232, 200 + (i >> 1) * 12, { color: C.color }); });
    if (P1().backP || P1().attackP || P1().startP || clicks.length) go(Menu);
  },
};
