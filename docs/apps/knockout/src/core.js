// core.js - screen, input (2 keyboard layouts + gamepads + mouse), tiny synth audio, helpers.
'use strict';
const W = 480, H = 270;                       // internal pixels; scaled up by a whole number
const cv = document.getElementById('game'), ctx = cv.getContext('2d');
cv.width = W; cv.height = H; ctx.imageSmoothingEnabled = false;
function fit() {
  const k = Math.max(1, Math.floor(Math.min(innerWidth / W, innerHeight / H))) || 1;
  const kk = Math.min(innerWidth / W, innerHeight / H) < 1 ? Math.min(innerWidth / W, innerHeight / H) : k;
  cv.style.width = W * kk + 'px'; cv.style.height = H * kk + 'px';
}
addEventListener('resize', fit); fit();

const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
const lerp = (a, b, t) => a + (b - a) * t;
const rnd = (a = 1, b) => b === undefined ? Math.random() * a : a + Math.random() * (b - a);
const pick = (arr) => arr[Math.floor(Math.random() * arr.length)];

// ---- input -----------------------------------------------------------------------------------------------------
// a "device" gives a pad state each frame: { x, y (-1..1), jump, attack, special, shield, start, back } + edges (*P = pressed this frame)
const keys = new Set(), keysP = new Set();
let GAME_KEYS = null;   // every key the game uses: browsers must not act on them (Firefox opens Quick Find on '/', Backspace, Space scrolls)
addEventListener('keydown', (e) => { if (!keys.has(e.code)) keysP.add(e.code); keys.add(e.code);
  GAME_KEYS = GAME_KEYS || new Set(Object.values(LAYOUTS).flatMap((L) => Object.values(L).filter(Array.isArray).flat()).concat(['KeyN', 'KeyP', 'KeyQ', 'KeyR', 'KeyM', 'Digit1', 'Digit2', 'Digit3', 'Digit4']));
  if (GAME_KEYS.has(e.code) || e.code.startsWith('Arrow')) e.preventDefault(); audio.unlock(); });
addEventListener('keyup', (e) => keys.delete(e.code));
addEventListener('blur', () => keys.clear());
const LAYOUTS = {
  k1: { name: 'KEYS 1', left: ['KeyA'], right: ['KeyD'], up: ['KeyW'], down: ['KeyS'], jump: ['KeyW', 'Space'], attack: ['KeyC', 'KeyJ'], special: ['KeyV', 'KeyK'], shield: ['KeyB', 'KeyL'],
        start: ['Enter'], back: ['Escape'], help: 'WASD move · W/Space jump · C attack · V special · B shield' },
  k2: { name: 'KEYS 2', left: ['ArrowLeft'], right: ['ArrowRight'], up: ['ArrowUp'], down: ['ArrowDown'], jump: ['ArrowUp'], attack: ['Comma', 'Numpad1'], special: ['Period', 'Numpad2'], shield: ['Slash', 'Numpad3'],
        start: ['NumpadEnter'], back: ['Backspace'], help: 'Arrows move · Up jump · , attack · . special · / shield' },
};
const anyKey = (list) => list.some((c) => keys.has(c) || keysP.has(c));   // a tap shorter than a frame still counts
const anyKeyP = (list) => list.some((c) => keysP.has(c));
function readKeys(id) {
  const L = LAYOUTS[id];
  return { x: (anyKey(L.right) ? 1 : 0) - (anyKey(L.left) ? 1 : 0), y: (anyKey(L.down) ? 1 : 0) - (anyKey(L.up) ? 1 : 0),
           jump: anyKey(L.jump), attack: anyKey(L.attack), special: anyKey(L.special), shield: anyKey(L.shield), start: anyKey(L.start), back: anyKey(L.back) };
}
// gamepads can be blocked by the host frame's permissions policy (getGamepads throws): treat that as no pads
let padsBlocked = false;
function allPads() { try { return [...(navigator.getGamepads ? navigator.getGamepads() : [])]; } catch (e) { padsBlocked = true; return []; } }
function readPad(i) {
  const g = allPads()[i];
  if (!g) return null;
  const b = (n) => !!(g.buttons[n] && g.buttons[n].pressed);
  let x = g.axes[0] || 0, y = g.axes[1] || 0;
  if (Math.abs(x) < 0.3) x = 0; if (Math.abs(y) < 0.3) y = 0;
  if (b(14)) x = -1; if (b(15)) x = 1; if (b(12)) y = -1; if (b(13)) y = 1;
  return { x: Math.sign(x) * (Math.abs(x) > 0.6 ? 1 : 0.5), y: Math.sign(y) * (Math.abs(y) > 0.6 ? 1 : 0.5), jump: b(0) || b(3), attack: b(2), special: b(1),
           shield: b(4) || b(5) || b(6) || b(7), start: b(9), back: b(8) };
}
const prevState = {}, devCache = {};
/** pad state with edges for device id ('k1' | 'k2' | 'g0'..'g3' | 'cpu'). Read once per frame and cached, so every caller
 *  in the same frame sees the same presses (a second read would otherwise lose the edge). */
function readDevice(id) {
  if (devCache[id]) return devCache[id];
  const s = id[0] === 'k' ? readKeys(id) : id[0] === 'g' ? (readPad(+id[1]) || { x: 0, y: 0 }) : { x: 0, y: 0 };
  const p = prevState[id] || {};
  for (const k of ['jump', 'attack', 'special', 'shield', 'start', 'back']) s[k + 'P'] = !!s[k] && !p[k];
  s.upP = s.y < -0.5 && !(p.y < -0.5); s.downP = s.y > 0.5 && !(p.y > 0.5);
  s.leftP = s.x < -0.5 && !(p.x < -0.5); s.rightP = s.x > 0.5 && !(p.x > 0.5);
  prevState[id] = s; devCache[id] = s;
  return s;
}
/** menus: keyboard 1 plus every connected pad, merged (any of them can drive the menus) */
function menuPad() {
  const m = { ...readDevice('k1') };
  for (const g of padsConnected()) { const d = readDevice(g); for (const k in d) if (k.endsWith('P') && d[k]) m[k] = true; if (d.attackP || d.jumpP) m.attackP = true; if (d.specialP) m.backP = true; }
  return m;
}
/** the story hero: keys 1, every pad, and keys 2 too unless a second player has it */
function heroPad() {
  const ids = ['k1', ...padsConnected()]; if (!SR.coop) ids.push('k2');
  const m = { ...readDevice('k1') };
  for (const id of ids.slice(1)) { const d = readDevice(id); if (!m.x && d.x) m.x = d.x; if (!m.y && d.y) m.y = d.y; for (const k in d) if (d[k] === true) m[k] = true; }
  return m;
}
function padsConnected() { return allPads().map((g, i) => g ? 'g' + i : null).filter(Boolean); }
// mouse / touch -> clicks in game pixels (menus)
let clicks = [];
cv.addEventListener('pointerdown', (e) => {
  const r = cv.getBoundingClientRect();
  clicks.push({ x: (e.clientX - r.left) * W / r.width, y: (e.clientY - r.top) * H / r.height }); audio.unlock();
});
function endInputFrame() { keysP.clear(); clicks = []; for (const k in devCache) delete devCache[k]; }

// ---- audio: tiny synth, no files ----------------------------------------------------------------------------------
const audio = (() => {
  let ac = null, master = null, musicGain = null, muted = false, song = null, songT = 0, nextNote = 0;
  const unlock = () => { if (ac) { if (ac.state === 'suspended') ac.resume(); return; } try { ac = new (window.AudioContext || window.webkitAudioContext)(); master = ac.createGain(); master.gain.value = 0.35; master.connect(ac.destination); musicGain = ac.createGain(); musicGain.gain.value = 0.32; musicGain.connect(master); } catch (e) { ac = null; } };
  function tone(f, d, type = 'square', vol = 0.3, slide = 0, when = 0, dest = null) {
    if (!ac || muted) return;
    const t = ac.currentTime + when, o = ac.createOscillator(), g = ac.createGain();
    o.type = type; o.frequency.setValueAtTime(f, t); if (slide) o.frequency.exponentialRampToValueAtTime(Math.max(20, f * slide), t + d);
    g.gain.setValueAtTime(vol, t); g.gain.exponentialRampToValueAtTime(0.001, t + d);
    o.connect(g); g.connect(dest || master); o.start(t); o.stop(t + d + 0.02);
  }
  function noise(d, vol = 0.3, hp = 800) {
    if (!ac || muted) return;
    const n = ac.sampleRate * d, buf = ac.createBuffer(1, n, ac.sampleRate), ch = buf.getChannelData(0);
    for (let i = 0; i < n; i++) ch[i] = (Math.random() * 2 - 1) * (1 - i / n);
    const s = ac.createBufferSource(), f = ac.createBiquadFilter(), g = ac.createGain();
    f.type = 'highpass'; f.frequency.value = hp; g.gain.value = vol; s.buffer = buf; s.connect(f); f.connect(g); g.connect(master); s.start();
  }
  const sfx = {
    jump: () => tone(300, 0.12, 'square', 0.15, 2),
    swing: () => noise(0.08, 0.12, 2500),
    hit: (k) => { tone(160 - k * 4, 0.12, 'square', 0.25, 0.4); noise(0.1 + k * 0.01, 0.25, 600); },
    heavy: () => { tone(90, 0.3, 'sawtooth', 0.35, 0.3); noise(0.3, 0.4, 200); },
    shield: () => tone(700, 0.08, 'triangle', 0.2, 0.7),
    ko: () => { noise(0.8, 0.45, 100); tone(880, 0.6, 'square', 0.2, 0.2); },
    splat: () => { noise(0.25, 0.5, 300); tone(70, 0.4, 'square', 0.4, 0.5); tone(1400, 0.05, 'square', 0.15, 1, 0.12); },
    land: () => noise(0.04, 0.1, 1500),
    select: () => tone(660, 0.07, 'square', 0.18),
    ok: () => { tone(523, 0.08, 'square', 0.2); tone(784, 0.12, 'square', 0.2, 1, 0.08); },
    coin: () => { tone(988, 0.06, 'square', 0.2); tone(1319, 0.2, 'square', 0.2, 1, 0.06); },
    beam: () => tone(220, 0.4, 'sawtooth', 0.25, 4),
    go: () => { tone(392, 0.15, 'square', 0.25); tone(523, 0.15, 'square', 0.25, 1, 0.15); tone(784, 0.35, 'square', 0.25, 1, 0.3); },
  };
  // music: a song = { bpm, bass: [midi|0...], lead: [...] } 16th steps, looped
  const mf = (m) => 440 * Math.pow(2, (m - 69) / 12);
  function tick() {
    if (!ac || !song || muted) return;
    const step = 60 / song.bpm / 4;
    if (nextNote < ac.currentTime) nextNote = ac.currentTime + 0.05;   // after a hidden tab or mute: skip the missed notes, don't play them all at once
    while (nextNote < ac.currentTime + 0.12) {
      const i = songT % song.lead.length, when = Math.max(0, nextNote - ac.currentTime);
      const b = song.bass[i % song.bass.length], l = song.lead[i];
      if (b) tone(mf(b), step * 1.8, 'triangle', 0.5, 0, when, musicGain);
      if (l) tone(mf(l), step * 0.9, 'square', 0.16, 0, when, musicGain);
      if (song.drum && i % 4 === 0) { /* kick */ tone(110, 0.08, 'sine', 0.5, 0.3, when, musicGain); }
      nextNote += step; songT++;
    }
  }
  return {
    unlock, sfx, tick,
    play(s) { song = s; songT = 0; if (ac) nextNote = ac.currentTime + 0.05; },
    stop() { song = null; },
    toggle() { muted = !muted; return muted; }, get muted() { return muted; },
  };
})();

// ---- text: a tiny built-in pixel font (5x7), so no font files are needed --------------------------------------------
const FONT = (() => {
  const g = {}, src = {
    A: '01110100011000111111100011000110001', B: '11110100011000111110100011000111110', C: '01110100011000010000100001000101110', D: '11110100011000110001100011000111110',
    E: '11111100001000011110100001000011111', F: '11111100001000011110100001000010000', G: '01110100011000010111100011000101111', H: '10001100011000111111100011000110001',
    I: '01110001000010000100001000010001110', J: '00111000100001000010000101001001100', K: '10001100101010011000101001001010001', L: '10000100001000010000100001000011111',
    M: '10001110111010110101100011000110001', N: '10001100011100110101100111000110001', O: '01110100011000110001100011000101110', P: '11110100011000111110100001000010000',
    Q: '01110100011000110001101011001001101', R: '11110100011000111110101001001010001', S: '01111100001000001110000010000111110', T: '11111001000010000100001000010000100',
    U: '10001100011000110001100011000101110', V: '10001100011000110001100010101000100', W: '10001100011000110101101011010101010', X: '10001100010101000100010101000110001',
    Y: '10001100010101000100001000010000100', Z: '11111000010001000100010001000011111',
    0: '01110100011001110101110011000101110', 1: '00100011000010000100001000010001110', 2: '01110100010000100010001000100011111', 3: '11111000100010000010000011000101110',
    4: '00010001100101010010111110001000010', 5: '11111100001111000001000011000101110', 6: '00110010001000011110100011000101110', 7: '11111000010001000100010000100001000',
    8: '01110100011000101110100011000101110', 9: '01110100011000101111000010001001100',
    '%': '11000110010001000100010001001100011', '!': '00100001000010000100001000000000100', '?': '01110100010000100010001000000000100', '.': '00000000000000000000000000110001100',
    ',': '00000000000000000000001100010001000', ':': '00000011000110000000011000110000000', '-': '00000000000000011111000000000000000', '+': '00000001000010011111001000010000000',
    "'": '00100001000100000000000000000000000', '/': '00001000010001000100010001000010000', '(': '00010001000100001000010000010000010', ')': '01000001000001000010000100010001000',
    '>': '01000001000001000001000100010001000', '<': '00010001000100010000010000010000010', '"': '01010010100000000000000000000000000', '#': '01010111110101001010111110101000000',
    '=': '00000000001111100000111110000000000', '*': '00000001001010101110101010010000000', '_': '00000000000000000000000000000011111', '&': '01100100101010001000101011001001101',
    ' ': '00000000000000000000000000000000000', '·': '00000000000000000100000000000000000', '$': '00100011111010001110001011111000100', '@': '01110100011011110101101111000001110',
  };
  for (const [k, v] of Object.entries(src)) g[k] = v;
  return g;
})();
const glyphCache = {};
function glyphCanvas(ch, color) {
  const key = ch + color; if (glyphCache[key]) return glyphCache[key];
  const c = document.createElement('canvas'); c.width = 5; c.height = 7; const g = c.getContext('2d'); g.fillStyle = color;
  const bits = FONT[ch] || FONT['?'];
  for (let i = 0; i < 35; i++) if (bits[i] === '1') g.fillRect(i % 5, Math.floor(i / 5), 1, 1);
  return (glyphCache[key] = c);
}
/** text(s, x, y, {color, size, align, shadow}) - size is a whole-number scale of the 5x7 font */
function text(s, x, y, o = {}) {
  s = String(s).toUpperCase(); const k = o.size || 1, adv = 6 * k, w = s.length * adv - k;
  let x0 = Math.round(o.align === 'center' ? x - w / 2 : o.align === 'right' ? x - w : x); y = Math.round(y);
  const g = o.g || ctx;
  const draw = (dx, dy, col) => { for (let i = 0; i < s.length; i++) g.drawImage(glyphCanvas(s[i], col), x0 + i * adv + dx, y + dy, 5 * k, 7 * k); };
  if (o.shadow !== false) draw(k, k, o.shadowColor || '#160e1e');
  if (o.outline) for (const [dx, dy] of [[-k, 0], [k, 0], [0, -k], [0, k]]) draw(dx, dy, o.outline);
  draw(0, 0, o.color || '#ffffff');
  return w;
}
const textW = (s, k = 1) => String(s).length * 6 * k - k;
