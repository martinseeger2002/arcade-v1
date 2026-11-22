// stages.js - four maps. Geometry: one solid main block (with ledges) + soft platforms you can drop through.
'use strict';
function offscreen(w, h, draw) { const c = document.createElement('canvas'); c.width = w; c.height = h; const g = c.getContext('2d'); g.imageSmoothingEnabled = false; draw(g); return c; }
function rect(g, x, y, w, h, col) { g.fillStyle = col; g.fillRect(Math.round(x), Math.round(y), Math.round(w), Math.round(h)); }
function gradSky(g, stops, y0 = 0, y1 = H) {   // banded gradient (pixel-art style, no smooth blend)
  const n = stops.length - 1, bands = 18;
  for (let i = 0; i < bands; i++) { const t = i / (bands - 1) * n, k = Math.min(n - 1, Math.floor(t)); g.fillStyle = mixHex(stops[k], stops[k + 1], t - k); g.fillRect(0, y0 + (y1 - y0) * i / bands, W, (y1 - y0) / bands + 1); }
}
function mixHex(a, b, t) {
  const pa = [1, 3, 5].map((i) => parseInt(a.slice(i, i + 2), 16)), pb = [1, 3, 5].map((i) => parseInt(b.slice(i, i + 2), 16));
  return '#' + pa.map((v, i) => Math.round(v + (pb[i] - v) * t).toString(16).padStart(2, '0')).join('');
}
const seeded = (s) => () => ((s = (s * 16807) % 2147483647) / 2147483647);

// draw the main block: a top surface strip + body, in the stage's colours
function drawBlock(g, b, top, side, edge, deco) {
  rect(g, b.x1, b.y, b.x2 - b.x1, (b.bottom || H) - b.y, side);
  rect(g, b.x1, b.y, b.x2 - b.x1, 5, top); rect(g, b.x1, b.y + 5, b.x2 - b.x1, 2, edge);
  if (deco) deco(g, b);
}
function drawPlat(g, p, col, edge) { rect(g, p.x1, p.y, p.x2 - p.x1, 4, col); rect(g, p.x1, p.y + 4, p.x2 - p.x1, 2, edge); }

const STAGES = {
  arcade: {
    name: 'ARCADE FLOOR', blurb: 'Where it all gets settled. Three platforms, no excuses.',
    main: { x1: 96, x2: 384, y: 196, bottom: 236 },
    plats: [{ x1: 136, x2: 206, y: 152 }, { x1: 274, x2: 344, y: 152 }, { x1: 205, x2: 275, y: 110 }],
    spawn: [[150, 196], [330, 196], [205, 196], [275, 196]],
    song: { bpm: 132, bass: [45, 0, 45, 0, 48, 0, 45, 0, 43, 0, 43, 0, 47, 0, 43, 0], lead: [69, 0, 72, 0, 76, 0, 72, 74, 0, 72, 0, 69, 67, 0, 69, 0, 69, 0, 72, 0, 76, 0, 79, 0, 77, 0, 76, 74, 72, 0, 0, 0], drum: true },
    paint(g) {
      gradSky(g, ['#1a0f2e', '#2a1446', '#3a1a5a']);
      const r = seeded(7);
      for (let i = 0; i < 60; i++) rect(g, r() * W, r() * 90, 1, 1, '#ffffff55');
      // back wall of cabinets
      for (let x = -10; x < W; x += 44) {
        const h = 74 + (x * 7 % 13);
        rect(g, x, 196 - h, 34, h, '#241338'); rect(g, x + 2, 196 - h + 8, 30, 20, '#120a1e');
        rect(g, x + 4, 196 - h + 10, 26, 16, ['#2ad8ff', '#ff5fd2', '#ffd23a', '#5aff8a'][(x / 44 + 4 | 0) % 4] + '66');
        rect(g, x, 196 - h, 34, 6, '#3a1f58');
      }
      // neon sign
      rect(g, 168, 22, 144, 26, '#160b26'); rect(g, 168, 22, 144, 2, '#ff5fd2'); rect(g, 168, 46, 144, 2, '#ff5fd2');
      drawBlock(g, this.main, '#ff5fd2', '#2a1a40', '#7a3a9a', (g, b) => {
        for (let x = b.x1; x < b.x2; x += 8) for (let y = b.y + 7; y < b.bottom; y += 8) if (((x - b.x1) / 8 + (y - b.y) / 8 | 0) % 2) rect(g, x, y, 8, 8, '#33204e');
        rect(g, b.x1 + 4, b.bottom, b.x2 - b.x1 - 8, 6, '#1a0f2a');
      });
      for (const p of this.plats) drawPlat(g, p, '#2ad8ff', '#1a6a8a');
    },
    live(t) { text('ARCADE', 240, 29, { size: 2, align: 'center', color: (t >> 5) % 8 ? '#ff9ae6' : '#ffffff', shadow: false }); },
  },
  beach: {
    name: 'SUNSET BEACH', blurb: 'Wide sand island. The tide always wins.',
    main: { x1: 84, x2: 396, y: 200, bottom: 230 },
    plats: [{ x1: 116, x2: 186, y: 156 }, { x1: 294, x2: 364, y: 156 }],
    spawn: [[140, 200], [340, 200], [210, 200], [270, 200]],
    song: { bpm: 112, bass: [41, 0, 0, 41, 0, 0, 48, 0, 46, 0, 0, 46, 0, 0, 45, 0], lead: [72, 0, 0, 77, 0, 76, 0, 72, 0, 0, 69, 0, 72, 0, 0, 0, 70, 0, 0, 74, 0, 72, 0, 70, 69, 0, 0, 65, 0, 0, 0, 0] },
    paint(g) {
      gradSky(g, ['#ff9a5a', '#ff6a7a', '#7a3a8a', '#2a1a4a'].reverse(), 0, 170);
      rect(g, 210, 120, 60, 30, '#ffd27a'); rect(g, 216, 114, 48, 6, '#ffd27a'); rect(g, 224, 110, 32, 4, '#ffd27a');
      for (let y = 150; y < H; y += 3) rect(g, 0, y, W, 3, mixHex('#3a2a6a', '#1a2a5a', (y - 150) / 120));
      for (let y = 152; y < 200; y += 6) rect(g, 200 + Math.sin(y) * 20, y, 80 - (y - 150), 1, '#ffd27a88');
      const palm = (x, s) => { for (let i = 0; i < 40 * s; i += 2) rect(g, x + Math.sin(i / 14) * 6, 160 - i, 4, 3, '#1a1028');
        for (const [dx, dy] of [[-18, 4], [18, 4], [-12, -2], [12, -2], [0, -6]]) rect(g, x + dx * s - 8, 160 - 40 * s + dy, 20, 3, '#1a1028'); };
      palm(30, 1.3); palm(450, 1.1);
      drawBlock(g, this.main, '#f2d08a', '#c89a5a', '#a87a4a', (g, b) => { const r = seeded(3); for (let i = 0; i < 90; i++) rect(g, b.x1 + r() * (b.x2 - b.x1), b.y + 8 + r() * 20, 1, 1, '#8a5a3a'); });
      for (const p of this.plats) drawPlat(g, p, '#b07a4a', '#6a4a2a');
    },
    live(t) { for (let i = 0; i < 4; i++) { const x = (t * 0.3 + i * 140) % (W + 40) - 20; rect(ctx, x, 224 + Math.sin(t / 20 + i) * 2, 26, 1, '#ffffff66'); } },
  },
  rooftop: {
    name: 'NIGHT ROOFTOP', blurb: 'A city of nodes below. Mind the moving lift.',
    main: { x1: 110, x2: 370, y: 198, bottom: 270 },
    plats: [{ x1: 150, x2: 220, y: 140, move: { ax: 90, period: 360 } }],
    spawn: [[150, 198], [330, 198], [210, 198], [270, 198]],
    song: { bpm: 124, bass: [38, 38, 0, 38, 0, 38, 41, 0, 36, 36, 0, 36, 0, 36, 43, 0], lead: [62, 0, 65, 0, 69, 0, 0, 67, 65, 0, 62, 0, 60, 0, 0, 0, 62, 0, 65, 0, 72, 0, 0, 70, 69, 0, 67, 0, 65, 0, 0, 0], drum: true },
    paint(g) {
      gradSky(g, ['#05060f', '#0e1530', '#1a2350']);
      const r = seeded(11);
      for (let i = 0; i < 80; i++) rect(g, r() * W, r() * 120, 1, 1, '#ffffff' + (r() > 0.5 ? '99' : '44'));
      for (let x = 0; x < W; x += 22 + (x % 3) * 6) { const h = 40 + r() * 110, w = 18 + r() * 14; rect(g, x, H - h - 40, w, h + 40, '#0a0e22');
        for (let wy = H - h - 34; wy < H; wy += 7) for (let wx = x + 3; wx < x + w - 3; wx += 5) if (r() > 0.55) rect(g, wx, wy, 2, 3, r() > 0.8 ? '#5af0ff' : '#ffd27a88'); }
      drawBlock(g, this.main, '#5a6a8a', '#2a3048', '#3a4462', (g, b) => { rect(g, b.x1 + 20, b.y - 40, 3, 40, '#3a4462'); rect(g, b.x1 + 16, b.y - 42, 11, 3, '#3a4462');
        rect(g, b.x2 - 60, b.y - 18, 40, 18, '#30384f'); rect(g, b.x2 - 56, b.y - 14, 32, 3, '#5a6a8a'); });
      for (const p of this.plats) if (!p.move) drawPlat(g, p, '#8a9ab8', '#4a5470');
    },
    live(t) { if ((t >> 4) % 4 === 0) rect(ctx, 129, 154, 3, 3, '#ff4a5a'); for (const p of this.plats) if (p.move) { drawPlat(ctx, p, '#ffd23a', '#9a7a1a'); rect(ctx, (p.x1 + p.x2) / 2 - 1, p.y + 6, 2, H, '#ffffff10'); } },
  },
  genesis: {
    name: 'GENESIS BLOCK', blurb: 'Block zero, floating in the void. Its neighbours bob up and down.',
    main: { x1: 124, x2: 356, y: 192, bottom: 246 },
    plats: [{ x1: 54, x2: 108, y: 150, move: { ay: 22, period: 300 } }, { x1: 372, x2: 426, y: 150, move: { ay: 22, period: 300, phase: 150 } }, { x1: 206, x2: 274, y: 128 }],
    spawn: [[160, 192], [320, 192], [215, 192], [265, 192]],
    song: { bpm: 100, bass: [40, 0, 0, 0, 47, 0, 0, 0, 45, 0, 0, 0, 43, 0, 0, 0], lead: [64, 0, 67, 71, 0, 0, 69, 0, 67, 0, 64, 0, 62, 0, 0, 0, 64, 0, 67, 71, 0, 74, 0, 72, 71, 0, 0, 67, 0, 0, 0, 0] },
    paint(g) {
      gradSky(g, ['#03020a', '#0a0620', '#140a2a']);
      const r = seeded(5);
      for (let i = 0; i < 120; i++) rect(g, r() * W, r() * H, 1, 1, ['#ffffff', '#f7931a', '#5a8ad8', '#3ad0f0'][i % 4] + '88');
      // a faint chain of blocks behind
      for (let i = 0; i < 9; i++) { const x = 20 + i * 54, y = 40 + Math.sin(i) * 14; rect(g, x, y, 30, 22, '#1a1236'); rect(g, x + 2, y + 2, 26, 18, '#110a26'); text('#' + i, x + 15, y + 8, { align: 'center', color: '#3a2a6a', shadow: false, g }); if (i < 8) rect(g, x + 30, y + 10, 24, 2, '#2a1a4a'); }
      drawBlock(g, this.main, '#f7931a', '#2a1a10', '#b06a10', (g, b) => { for (let y = b.y + 10; y < b.bottom - 4; y += 6) rect(g, b.x1 + 8, y, b.x2 - b.x1 - 16, 1, '#3a2614');
        text('BLOCK 0', (b.x1 + b.x2) / 2, b.y + 20, { align: 'center', color: '#f7931a', shadow: false, g }); });
      drawPlat(g, this.plats[2], '#ffd27a', '#9a6a1a');
    },
    live(t) { for (const p of this.plats) if (p.move) { rect(ctx, p.x1, p.y, p.x2 - p.x1, 14, '#2a1a4a'); drawPlat(ctx, p, '#c8a2ff', '#6a4a9a'); } },
  },
  moon: {   // story only (not in the VS list)
    name: 'THE MOON', blurb: 'Low gravity vibes. Earth is watching.',
    main: { x1: 100, x2: 380, y: 200, bottom: 250 },
    plats: [{ x1: 140, x2: 205, y: 150 }, { x1: 275, x2: 340, y: 150 }],
    spawn: [[150, 200], [330, 200], [210, 200], [270, 200]],
    song: { bpm: 136, bass: [43, 0, 43, 0, 50, 0, 43, 0, 41, 0, 41, 0, 48, 0, 46, 0], lead: [67, 0, 70, 0, 74, 0, 79, 0, 77, 0, 74, 0, 72, 0, 70, 0, 67, 0, 70, 0, 74, 0, 82, 0, 79, 0, 77, 74, 72, 0, 0, 0], drum: true },
    paint(g) {
      gradSky(g, ['#000004', '#05051a', '#0a0a2a']);
      const r = seeded(21);
      for (let i = 0; i < 140; i++) rect(g, r() * W, r() * H, 1, 1, '#ffffff' + (r() > 0.6 ? 'cc' : '55'));
      ob(g, 360, 30, 46, 46, '#2a6ad8'); rect(g, 366, 40, 14, 10, '#3aaa4a'); rect(g, 384, 54, 16, 12, '#3aaa4a'); rect(g, 362, 32, 42, 4, '#5a9aff');
      drawBlock(g, this.main, '#d8d8e0', '#8a8a9a', '#6a6a7a', (g, b) => { const r = seeded(4); for (let i = 0; i < 14; i++) { const x = b.x1 + 10 + r() * (b.x2 - b.x1 - 30), y = b.y + 10 + r() * 30; rect(g, x, y, 10, 4, '#6a6a7a'); rect(g, x + 2, y + 1, 6, 2, '#5a5a6a'); } });
      for (const p of this.plats) drawPlat(g, p, '#b8b8c8', '#6a6a7a');
      rect(g, 120, 168, 2, 32, '#c8c8d4'); rect(g, 122, 168, 14, 9, '#ffd23a'); rect(g, 126, 170, 6, 5, '#f0a040');   // a little flag
    },
    live(t) { const x = (t * 0.4) % (W + 60) - 30; rect(ctx, x, 60 + Math.sin(t / 30) * 6, 6, 2, '#ffffff66'); },
  },
};
const STAGE_ORDER = ['arcade', 'beach', 'rooftop', 'genesis'];
for (const s of Object.values(STAGES)) {
  s.ledges = [{ x: s.main.x1, y: s.main.y, side: -1 }, { x: s.main.x2, y: s.main.y, side: 1 }];
  for (const p of s.plats) { p.bx1 = p.x1; p.bx2 = p.x2; p.by = p.y; p.dx = 0; p.dy = 0; }
}
function stageTick(s, t) {
  for (const p of s.plats) if (p.move) {
    const m = p.move, a = 2 * Math.PI * (t + (m.phase || 0)) / m.period;
    const nx = p.bx1 + (m.ax || 0) * (Math.sin(a) + 1), ny = p.by + (m.ay || 0) * Math.sin(a);
    p.dx = nx - p.x1; p.dy = ny - p.y; p.x2 += p.dx; p.x1 = nx; p.y = ny;
  }
}
function stageReset(s) { for (const p of s.plats) { p.x1 = p.bx1; p.x2 = p.bx2; p.y = p.by; p.dx = p.dy = 0; } }
