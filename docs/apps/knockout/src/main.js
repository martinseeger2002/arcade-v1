// main.js - boot + fixed 60 Hz loop.
'use strict';
let last = 0, acc = 0;
function loop(now) {
  acc += Math.min(100, now - (last || now)); last = now;
  let steps = 0;
  while (acc >= 1000 / 60 && steps < 4) {
    acc -= 1000 / 60; steps++;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    try { scene.update(); } catch (e) { console.error(e); if (scene !== Menu) { try { go(Menu); } catch (e2) {} } }   // one bad frame must never freeze the game
    sceneT++;
    endInputFrame();
  }
  audio.tick();
  requestAnimationFrame(loop);
}
loadSprites().then(() => { go(Title); requestAnimationFrame(loop); });
// test hook: run n frames at once (headless tests simulate whole matches in seconds)
function stepFrames(n) { for (let i = 0; i < n; i++) { ctx.setTransform(1, 0, 0, 1, 0, 0); scene.update(); sceneT++; endInputFrame(); } }
window.KNOCKOUT = { go, STATS, stepFrames, STAGES, get scene() { return scene; }, get F() { return F; }, get MATCH() { return MATCH; }, CharSelect, Fight, Title, Menu, StageSelect, Results, HowTo, RULES, StoryMenu, Talk, Cheer, Card, Continue, ChapterClear, Credits, CHAPTERS, SR, KN_AUTO, storyStart, storyNext, NET, Online, get frameNo() { return frameNo; } };
