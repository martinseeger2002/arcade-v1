# ARCADE Knockout

A light 2D platform fighter for the Arcade App Store, published by @yourfirstname. One self-contained HTML page
(about 425 KB, sprites inline, no outside requests).

- **VS:** 1 vs CPU or 2-4 players on one screen (two keyboard layouts, gamepads, CPU fills empty slots); stock or
  time rules; four stages; each fighter has its own specials; fast KOs sometimes hit the screen.
- **Story:** "The UTXO Chronicles", 11 chapters of mostly true UTXO-chain history told with jokes (talk scenes,
  rhythm stages, drawn bosses). Fan-made; not affiliated with or endorsed by anyone in it; not financial advice.
- **Online:** up to 4 players through `arcade.realtime` with `{fast: true}` (`/r/realtime.js`), browser to browser
  like CUL-DE-SAC. The host (lowest player id) runs the match; the others send buttons and draw the host's snapshots
  (30 a second). Rooms `knockout-1`..`knockout-9`, four players each. Outside an arcade, ONLINE says where to play.

Fighters are rendered from the ARCADE cast's 3D models (sprite sheets in `assets/`, left-facing sheets for the
characters with lettering).

## Build

    python3 tools/build.py      # -> dist/knockout.html

`src/*.js` are concatenated in order (core, data, stages, game, boss, story, net, ui, main) into `src/index.html`.

## Gamepads in the app window

The app window's iframe needs `allow="... gamepad"` for `navigator.getGamepads()` (Chromium-based webviews block it
in a cross-origin frame otherwise). The game still runs without it and says pads are blocked.
