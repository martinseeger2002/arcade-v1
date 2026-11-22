// data.js - the roster: stats, normal moves, special names. Sprites come from the 3D cast (see README).
'use strict';
const ROSTER = ['buster', 'lester', 'shiba', 'pep3d', 'digi', 'arcade'];
const CHARS = {
  buster: { name: 'BUSTER', chain: 'BITCOIN', tag: 'THE ORIGINAL', color: '#f7931a', weight: 118, run: 2.0, air: 1.7, jump: 7.4, grav: 0.38, fall: 5.6, jumps: 2, hw: 22, hh: 50, pow: 1.2, reach: 1.0,
            special: 'HALVING SLAM', up: 'BLOCK REWARD', blurb: 'Slow to start, impossible to stop. Hits like a four-year cycle.' },
  lester: { name: 'LESTER', chain: 'LITECOIN', tag: 'SILVER SPEED', color: '#5a8ad8', weight: 82, run: 2.7, air: 2.2, jump: 7.8, grav: 0.36, fall: 5.0, jumps: 2, hw: 20, hh: 50, pow: 0.92, reach: 1.35,
            special: 'COIN TOSS', up: 'SCRYPT SPIN', blurb: 'Long wire limbs, quick blocks. Tests every trick first.' },
  shiba:  { name: 'SHIBA', chain: 'DOGECOIN', tag: 'MUCH FAST', color: '#f0a040', weight: 72, run: 3.2, air: 2.4, jump: 7.4, grav: 0.4, fall: 5.6, jumps: 2, hw: 32, hh: 30, pow: 0.85, reach: 0.9,
            special: 'MOON DASH', up: 'TO THE MOON', blurb: 'Small, fast, flies very far. Wow.' },
  pep3d:  { name: 'PEP', chain: 'PEPECOIN', tag: 'THE LEAPER', color: '#5fbf4a', weight: 95, run: 2.3, air: 2.0, jump: 8.4, grav: 0.34, fall: 4.6, jumps: 3, hw: 22, hh: 46, pow: 1.0, reach: 1.0, kickPow: 1.25,
            special: 'TONGUE LASH', up: 'FROG LEAP', blurb: 'Three jumps and a tongue that pulls you in.' },
  digi:   { name: 'DIGI', chain: 'DIGIBYTE', tag: 'FIVE ALGOS', color: '#3ad0f0', weight: 102, run: 2.2, air: 1.9, jump: 7.6, grav: 0.36, fall: 5.0, jumps: 2, hw: 24, hh: 44, pow: 1.0, reach: 1.05,
            special: 'VISOR BEAM', up: 'DIGISHIELD JET', blurb: 'Keeps you at range with a beam. Hovers home.' },
  arcade: { name: 'ARCADE', chain: 'THE HOST', tag: 'THE HOUSE', color: '#9b6bff', weight: 138, run: 1.7, air: 1.5, jump: 7.2, grav: 0.37, fall: 5.2, jumps: 2, hw: 30, hh: 58, pow: 1.28, reach: 1.15,
            special: 'INSERT COIN', up: 'JOYSTICK SPRING', blurb: 'The big cabinet. Throws tokens, never tilts.' },
};

/** normal moves, built per character from its size / power / reach.
 *  seq = [[sprite frame, frames held], ...]; hit = active windows (t0..t1 in move frames), box relative to the feet, facing right. */
function buildMoves(c) {
  const h = c.hh, r = c.reach, p = c.pow, kp = c.kickPow || 1;
  return {
    jab:   { anim: 'jab', seq: [[0, 3], [1, 5], [2, 6]], hit: [{ t0: 3, t1: 7, x: 6 * r, y: -0.62 * h, w: 18 * r, h: 12, dmg: 3, kb: 2.2, kbg: 1.0, ang: 35 }], swing: 'arc' },
    smash: { anim: 'smash', seq: [[0, 11], [1, 6], [2, 14]], hit: [{ t0: 11, t1: 16, x: 6 * r, y: -0.62 * h, w: 26 * r, h: 18, dmg: Math.round(13 * p), kb: 3.6, kbg: 6.4 * p, ang: 38 }], swing: 'big', heavy: true },
    up:    { anim: 'upair', seq: [[0, 5], [1, 11]], hit: [{ t0: 5, t1: 12, x: -10, y: -h - 16, w: 26, h: 20, dmg: Math.round(8 * p), kb: 3.0, kbg: 4.2 * p, ang: 88 }], swing: 'up' },
    low:   { anim: 'crouch', seq: [[0, 5], [0, 9]], hit: [{ t0: 4, t1: 9, x: 2, y: -0.25 * h, w: 22 * r, h: 10, dmg: 6, kb: 2.6, kbg: 2.8, ang: 22 }], swing: 'low' },
    air:   { anim: 'kick', seq: [[0, 5], [1, 13]], hit: [{ t0: 5, t1: 15, x: 0, y: -0.45 * h, w: 24 * r, h: 16, dmg: Math.round(9 * kp), kb: 3.0, kbg: 4.2 * kp, ang: 40 }], swing: 'arc', air: true },
    uair:  { anim: 'upair', seq: [[0, 4], [1, 12]], hit: [{ t0: 4, t1: 12, x: -12, y: -h - 14, w: 28, h: 18, dmg: Math.round(8 * p), kb: 2.8, kbg: 4.0 * p, ang: 86 }], swing: 'up', air: true },
    dair:  { anim: 'kick', seq: [[1, 6], [1, 12]], hit: [{ t0: 6, t1: 14, x: -8, y: -6, w: 18, h: 16, dmg: Math.round(10 * p), kb: 2.6, kbg: 3.6 * p, ang: -70 }], swing: 'low', air: true },
  };
}
for (const k of ROSTER) CHARS[k].moves = buildMoves(CHARS[k]);
const PCOLORS = ['#ff4a5a', '#3a8cff', '#ffd23a', '#3ad87a'];
