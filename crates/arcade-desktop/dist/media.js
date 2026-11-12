/* Camera, microphone and screen for app windows (room.media on a room joined with {fast: true}).

   The original arcade's viewer puts the camera's tracks on WebRTC links. An app window here has no
   WebRTC, so it does the same work itself: it opens the camera, microphone or screen, encodes them
   (WebCodecs: VP8 pictures, Opus sound) and hands the pieces to its arcade (realtime.media), which
   carries them to the arcade of each player they are shared with: over a direct link between the
   two arcades, or through an arcade both are linked to when they have none (behind NAT). The
   other window decodes them and draws each picture where its page placed it, in a layer over the
   page, and plays each voice its page chose to hear. The page itself never touches a camera or a
   voice: it only says what goes to whom (share), whose voice it hears (hear, volume) and where a
   picture goes (place), as on the original.

   Nothing is sent until the page names a player (share), and nobody's sound plays until the page
   names them (hear). While anything is open the window shows so, with a Stop the page cannot cover.

   A piece on the wire: 0xA1, kind (1 camera, 2 microphone, 3 screen, 4 screen sound, 9 a note),
   flags (1: key frame), seq (u32), time (u32, ms), part, parts, width (u16), height (u16), bytes. */
(function () {
  "use strict";
  const KIND = { cam: 1, mic: 2, scr: 3, snd: 4, note: 9 };
  const HEAD = 17, PIECE = 48000, LOUDEST = 4, TILES = 16;
  const CAM = { most: 640, fps: 20, bitrate: 600000 }, SCR = { most: 1280, fps: 10, bitrate: 1500000 };
  const KEY_EVERY = 2000, GONE_AFTER = 3000, SPEAK_ON = 0.04, SPEAK_HOLD = 600, OUT_MOST = 80;
  const FRAME_MAX_W = 1920, FRAME_MAX_H = 1080, FRAME_MAX_FPS = 30;

  window.ArcadeMedia = function (env) {
    const { api, push, perms } = env;
    let layer = null, pill = null, ctx = null, ctxReady = null;
    const seats = new Set();

    // ---------------- sound: one context for the window ----------------
    function audio() {
      if (ctxReady) return ctxReady;
      const C = window.AudioContext || window.webkitAudioContext;
      ctx = new C({ sampleRate: 48000 });
      // A window that has not been clicked may start with its sound held: any click lets it go.
      const wake = () => { if (ctx.state === "suspended") ctx.resume().catch(() => {}); };
      window.addEventListener("pointerdown", wake, true);
      window.addEventListener("keydown", wake, true);
      ctxReady = ctx.audioWorklet.addModule("media-worklet.js").then(() => { wake(); return ctx; });
      return ctxReady;
    }

    // ---------------- the layer over the page, and the on-air pill ----------------
    function theLayer() {
      if (layer) return layer;
      layer = document.createElement("div");
      layer.style.cssText = "position:fixed;inset:0;pointer-events:none;overflow:hidden;z-index:5";
      document.body.appendChild(layer);
      return layer;
    }
    function drawPill() {
      let on = null;
      for (const s of seats) if (s.media && (s.media.stream || s.media.xstream || Object.keys(s.media.frames).length || s.media.levels)) on = s;
      if (!on) { if (pill) pill.hidden = true; return; }
      if (!pill) {
        pill = document.createElement("div");
        pill.setAttribute("role", "status");
        pill.style.cssText = "position:absolute;right:8px;top:8px;z-index:2;pointer-events:auto;display:flex;gap:8px;align-items:center;"
          + "padding:4px 6px 4px 10px;border-radius:999px;background:rgba(20,20,20,.82);color:#fff;font:600 13px/1.2 system-ui,sans-serif;"
          + "box-shadow:0 1px 4px rgba(0,0,0,.4)";
        const dot = document.createElement("span"); dot.textContent = "●"; dot.style.color = "#ff4d4d";
        const words = document.createElement("span"); words.className = "words";
        const stop = document.createElement("button"); stop.type = "button"; stop.textContent = "Stop"; stop.className = "stop";
        stop.style.cssText = "border:0;border-radius:999px;padding:3px 10px;background:#fff;color:#111;font:inherit;cursor:pointer";
        stop.onclick = () => { for (const s of seats) if (s.media) stopAll(s); };
        pill.append(dot, words, stop);
        theLayer().appendChild(pill);
      }
      const md = on.media;
      let what = md.xstream ? "Sharing your screen" : "";
      if (md.stream) {
        const cam = md.video && md.audio ? "Camera and microphone on" : md.video ? "Camera on" : md.audio ? "Microphone on" : "";
        what = what && cam ? what + " · " + cam.toLowerCase() : what || cam;
      }
      if (md.muted.video && md.muted.audio) what += " (muted)";
      if (Object.keys(md.frames).length || md.levels) what = what ? what + " · pictures shown in the app" : "Pictures shown in the app";
      pill.querySelector(".words").textContent = what;
      pill.hidden = false;
    }

    // ---------------- per seat ----------------
    function mediaOf(s) {
      if (!s.media) {
        s.media = { stream: null, xstream: null, video: false, audio: false, screen: false, share: {}, hear: {}, volume: {},
                    place: {}, muted: { video: false, audio: false, sound: false }, tiles: {}, enc: {}, rx: {}, out: [], busy: false,
                    frames: {}, levels: null, spatial: null, speaking: {}, kfAsked: {} };
        seats.add(s);
        s.media.watch = setInterval(() => watch(s), 150);
      }
      return s.media;
    }
    const say = (s, ev) => { ev.room = s.name; push(ev); };
    const peerOf = (s, id) => (s.fast && s.fast.peers[id]) || null;
    // Media goes to a player the arcades' fast lane reaches: over a direct link between the two
    // arcades, through a relaying arcade when they have none (routes: "relay"), or on this one.
    function direct(s, id) {
      const peer = peerOf(s, id);
      if (!peer || peer.state !== "direct") return false;
      const route = s.fast.routes && s.fast.routes[id];
      return route === "direct" || route === "relay" || (!route && peer.p.node && s.me && peer.p.node === s.me.node);
    }

    // ---------------- sending ----------------
    function header(kind, key, seq, t, part, parts, w, h) {
      const b = new Uint8Array(HEAD), v = new DataView(b.buffer);
      b[0] = 0xa1; b[1] = kind; b[2] = key ? 1 : 0;
      v.setUint32(3, seq >>> 0); v.setUint32(7, t >>> 0); b[11] = part; b[12] = parts;
      v.setUint16(13, w || 0); v.setUint16(15, h || 0);
      return b;
    }
    function b64(u8) { let t = ""; for (let i = 0; i < u8.length; i += 0x8000) t += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(t); }
    function unb64(text) { const t = atob(text), u = new Uint8Array(t.length); for (let i = 0; i < t.length; i++) u[i] = t.charCodeAt(i); return u; }
    // Whom a kind goes to now: the players it is shared with whom the fast lane reaches.
    function targets(s, kind) {
      const md = s.media, want = kind === KIND.mic || kind === KIND.snd ? "audio" : "video";
      return Object.keys(md.share).filter((id) => md.share[id][want] && direct(s, id));
    }
    function sendPiece(s, kind, key, seq, t, bytes, w, h, to) {
      const md = s.media;
      if (!to.length) return;
      const parts = Math.max(1, Math.ceil(bytes.length / PIECE));
      for (let i = 0; i < parts; i++) {
        const body = bytes.subarray(i * PIECE, (i + 1) * PIECE), msg = new Uint8Array(HEAD + body.length);
        msg.set(header(kind, key, seq, t, i, parts, w, h)); msg.set(body, HEAD);
        md.out.push({ to, b64: b64(msg), kind, key });
      }
      // Behind: the pictures waiting go (a key frame comes next), sound and notes stay.
      if (md.out.length > OUT_MOST) {
        md.out = md.out.filter((x) => x.kind === KIND.mic || x.kind === KIND.snd || x.kind === KIND.note);
        for (const k of ["cam", "scr"]) if (md.enc[k]) md.enc[k].wantKey = true;
      }
      pump(s);
    }
    function pump(s) {
      const md = s.media;
      if (!md || md.busy) return;
      const x = md.out.shift();
      if (!x) return;
      md.busy = true;
      api("realtime.media", { token: s.token, to: x.to, b64: x.b64 }).catch(() => {}).then(() => { md.busy = false; pump(s); });
    }
    function note(s, to, o) {
      if (!to.length) return;
      sendPiece(s, KIND.note, false, 0, 0, new TextEncoder().encode(JSON.stringify(o)), 0, 0, to);
    }

    // Pictures: a stream's video drawn into a canvas at its fps, each frame encoded.
    function startVideo(s, slot, kind, stream, opt) {
      const md = s.media, v = document.createElement("video");
      v.muted = true; v.playsInline = true; v.srcObject = stream; v.play().catch(() => {});
      const c = document.createElement("canvas"), g = c.getContext("2d");
      const e = (md.enc[slot] = { v, c, seq: 0, wantKey: true, lastKey: 0, w: 0, h: 0, enc: null, stream });
      const make = (w, h) => {
        try { if (e.enc) e.enc.close(); } catch (err) {}
        e.enc = new VideoEncoder({
          output: (chunk) => {
            const bytes = new Uint8Array(chunk.byteLength); chunk.copyTo(bytes);
            sendPiece(s, kind, chunk.type === "key", e.seq++, Math.round(chunk.timestamp / 1000), bytes, e.w, e.h, targets(s, kind));
          },
          error: () => { e.enc = null; e.wantKey = true; },
        });
        e.enc.configure({ codec: "vp8", width: w, height: h, bitrate: opt.bitrate, framerate: opt.fps, latencyMode: "realtime" });
        e.w = w; e.h = h; e.wantKey = true;
      };
      e.timer = setInterval(() => {
        if (md.enc[slot] !== e || v.readyState < 2 || !v.videoWidth) return;
        if (slot === "cam" && md.muted.video) return;
        if (!targets(s, kind).length) { e.wantKey = true; return; }          // nobody to send to: nothing encoded
        const scale = Math.min(1, opt.most / Math.max(v.videoWidth, v.videoHeight));
        const w = Math.max(2, Math.round(v.videoWidth * scale / 2) * 2), h = Math.max(2, Math.round(v.videoHeight * scale / 2) * 2);
        if (!e.enc || e.enc.state === "closed" || w !== e.w || h !== e.h) make(w, h);
        if (e.enc.encodeQueueSize > 4) return;                                // the encoder is behind: skip a frame
        if (c.width !== w || c.height !== h) { c.width = w; c.height = h; }
        g.drawImage(v, 0, 0, w, h);
        const now = performance.now(), key = e.wantKey || now - e.lastKey > KEY_EVERY;
        const frame = new VideoFrame(c, { timestamp: Math.round(now * 1000) });
        try { e.enc.encode(frame, { keyFrame: key }); if (key) { e.wantKey = false; e.lastKey = now; } } catch (err) { e.wantKey = true; }
        frame.close();
      }, Math.round(1000 / opt.fps));
    }
    // Sound: the stream's samples from a worklet, 20 ms at a time, encoded.
    async function startAudio(s, slot, kind, stream) {
      const md = s.media, e = (md.enc[slot] = { seq: 0, buf: new Float32Array(960), n: 0, level: 0, enc: null, stream });
      const c = await audio();
      if (s.media !== md || md.enc[slot] !== e) return;          // stopped while the sound was getting ready
      const src = (e.src = c.createMediaStreamSource(stream)), cap = (e.cap = new AudioWorkletNode(c, "arcade-cap"));
      const quiet = (e.quiet = c.createGain()); quiet.gain.value = 0;
      src.connect(cap); cap.connect(quiet); quiet.connect(c.destination);
      e.enc = new AudioEncoder({
        output: (chunk) => {
          const bytes = new Uint8Array(chunk.byteLength); chunk.copyTo(bytes);
          sendPiece(s, kind, true, e.seq++, Math.round(chunk.timestamp / 1000), bytes, 0, 0, targets(s, kind));
        },
        error: () => {},
      });
      e.enc.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 1, bitrate: kind === KIND.snd ? 96000 : 32000 });
      let t0 = performance.now() * 1000, sent = 0;
      cap.port.onmessage = (ev) => {
        const block = ev.data;
        let sum = 0; for (let i = 0; i < block.length; i++) sum += block[i] * block[i];
        e.level = Math.max(e.level * 0.9, Math.sqrt(sum / block.length) * 1.5);
        if (md.enc[slot] !== e) return;
        const muted = slot === "mic" ? md.muted.audio : md.muted.sound;
        for (let i = 0; i < block.length; i++) {
          e.buf[e.n++] = muted ? 0 : block[i];
          if (e.n === 960) {
            e.n = 0;
            if (!targets(s, kind).length || e.enc.state !== "configured") continue;
            const data = new AudioData({ format: "f32-planar", sampleRate: 48000, numberOfFrames: 960, numberOfChannels: 1,
                                         timestamp: t0 + sent * 20000, data: e.buf.slice(0) });
            sent++;
            try { e.enc.encode(data); } catch (err) {}
            data.close();
          }
        }
      };
    }
    function stopSlot(s, slot) {
      const md = s.media, e = md && md.enc[slot];
      if (!e) return;
      delete md.enc[slot];
      clearInterval(e.timer);
      try { if (e.enc && e.enc.state !== "closed") e.enc.close(); } catch (err) {}
      try { if (e.v) { e.v.srcObject = null; } } catch (err) {}
      try { if (e.src) { e.src.disconnect(); e.cap.disconnect(); e.quiet.disconnect(); } } catch (err) {}
    }

    // ---------------- receiving ----------------
    function rxOf(s, id) {
      const md = s.media;
      return md.rx[id] || (md.rx[id] = { parts: {}, dec: {}, last: {}, said: "", level: {} });
    }
    function heard(s, ev) {
      const p = ev.member;
      if (!p || !s.fast || !s.fast.peers[p.id] || typeof ev.b64 !== "string") return;
      const md = mediaOf(s), u = unb64(ev.b64);
      if (u.length < HEAD || u[0] !== 0xa1) return;
      const dv = new DataView(u.buffer), kind = u[1], key = !!(u[2] & 1), seq = dv.getUint32(3), t = dv.getUint32(7);
      const part = u[11], parts = u[12], w = dv.getUint16(13), h = dv.getUint16(15);
      const rx = rxOf(s, p.id);
      let body = u.subarray(HEAD);
      if (parts > 1) {
        const k = kind + "/" + seq, got = rx.parts[k] || (rx.parts[k] = { n: 0, bits: [] });
        if (!got.bits[part]) { got.bits[part] = body.slice(0); got.n++; }
        if (got.n < parts) return;
        delete rx.parts[k];
        const all = new Uint8Array(got.bits.reduce((a, b) => a + b.length, 0));
        let at = 0; for (const b of got.bits) { all.set(b, at); at += b.length; }
        body = all;
        for (const k2 in rx.parts) if (+k2.split("/")[1] < seq - 30) delete rx.parts[k2];
      }
      if (kind === KIND.note) { heardNote(s, p, body); return; }
      if (kind === KIND.cam || kind === KIND.scr) heardVideo(s, p, kind, key, t, body, w, h);
      else if (kind === KIND.mic || kind === KIND.snd) heardAudio(s, p, kind, t, body);
    }
    function heardNote(s, p, body) {
      let o; try { o = JSON.parse(new TextDecoder().decode(body)); } catch (err) { return; }
      const md = s.media, rx = rxOf(s, p.id);
      if (o.kf) for (const slot of ["cam", "scr"]) if (md.enc[slot]) md.enc[slot].wantKey = true;
      if (o.off) { rx.last = {}; said(s, p.id); drawAll(s); }
      if (o.screenOff) { delete rx.last.scr; drawAll(s); }
    }
    function askKey(s, id) {
      const md = s.media, now = Date.now();
      if (now - (md.kfAsked[id] || 0) < 1000) return;
      md.kfAsked[id] = now;
      note(s, [id], { kf: 1 });
    }
    function heardVideo(s, p, kind, key, t, body, w, h) {
      const md = s.media, rx = rxOf(s, p.id), slot = kind === KIND.cam ? "cam" : "scr", tileKey = slot === "cam" ? p.id : p.id + "#screen";
      rx.last[slot] = Date.now();
      let d = rx.dec[slot];
      if (!d || d.dec.state === "closed" || (key && (d.w !== w || d.h !== h))) {
        if (!key) { askKey(s, p.id); return; }
        try { if (d) d.dec.close(); } catch (err) {}
        d = rx.dec[slot] = { w, h, broken: false, dec: null };
        d.dec = new VideoDecoder({
          output: (frame) => { paint(s, tileKey, frame); frame.close(); },
          error: () => { d.broken = true; askKey(s, p.id); },
        });
        d.dec.configure({ codec: "vp8", codedWidth: w, codedHeight: h, optimizeForLatency: true });
      }
      if (d.broken && !key) { askKey(s, p.id); return; }
      if (d.dec.decodeQueueSize > 8 && !key) return;
      try { d.dec.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: t * 1000, data: body })); if (key) d.broken = false; }
      catch (err) { d.broken = true; askKey(s, p.id); }
      said(s, p.id);
    }
    function heardAudio(s, p, kind, t, body) {
      const md = s.media, rx = rxOf(s, p.id), slot = kind === KIND.mic ? "mic" : "snd", key = slot === "mic" ? p.id : p.id + "#screen";
      rx.last[slot] = Date.now();
      let d = rx.dec[slot];
      if (!d) {
        d = rx.dec[slot] = { dec: null, out: null };
        audio().then((c) => {
          if (rx.dec[slot] !== d) return;
          const node = new AudioWorkletNode(c, "arcade-play", { outputChannelCount: [1] });
          const pan = c.createPanner(); pan.panningModel = "HRTF"; pan.distanceModel = "linear"; pan.rolloffFactor = 0;
          const fade = c.createGain(), vol = c.createGain();
          node.connect(pan); pan.connect(fade); fade.connect(vol); vol.connect(c.destination);
          d.out = { node, pan, fade, vol };
          sound(s);
        });
        d.dec = new AudioDecoder({
          output: (a) => {
            const buf = new Float32Array(a.numberOfFrames);
            try { a.copyTo(buf, { planeIndex: 0, format: "f32-planar" }); } catch (err) { a.close(); return; }
            a.close();
            let sum = 0; for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i];
            rx.level[slot] = Math.max((rx.level[slot] || 0) * 0.9, Math.sqrt(sum / buf.length) * 1.5);
            if (d.out) d.out.node.port.postMessage(buf, [buf.buffer]);
          },
          error: () => { try { d.dec.close(); } catch (err) {} delete rx.dec[slot]; if (d.out) d.out.node.disconnect(); },
        });
        d.dec.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 1 });
      }
      if (d.dec.state !== "configured") return;
      try { d.dec.decode(new EncodedAudioChunk({ type: "key", timestamp: t * 1000, data: body })); } catch (err) {}
      said(s, p.id);
    }
    // What the page is told of a player: 'media' {video, audio} while they send, 'media-off'.
    function said(s, id) {
      const md = s.media, rx = md && md.rx[id], peer = peerOf(s, id);
      if (!rx || !peer) return;
      const now = Date.now(), live = (k) => rx.last[k] && now - rx.last[k] < GONE_AFTER;
      const video = live("cam") || live("scr"), audioOn = live("mic");
      const was = rx.said, is = video || audioOn ? (video ? "v" : "") + (audioOn ? "a" : "") : "";
      if (was === is) return;
      // A player who starts sending: their sound often comes a moment before their first key
      // frame. Wait for the other kind a little, so the page hears both at once (as on the original).
      if (!was && is.length === 1) {
        rx.first = rx.first || now;
        if (now - rx.first < 600) return;
      }
      if (!is) rx.first = 0;
      rx.said = is;
      if (is) say(s, { type: "media", member: peer.p, video, audio: audioOn });
      else say(s, { type: "media-off", member: peer.p });
      drawAll(s);
    }

    // ---------------- pictures in the layer ----------------
    function tile(s, key, isVideo) {
      const md = s.media;
      let el = md.tiles[key];
      if (el && (el.tagName === "VIDEO") !== isVideo) { el.remove(); el = null; }
      if (!el) {
        el = md.tiles[key] = document.createElement(isVideo ? "video" : "canvas");
        if (isVideo) { el.muted = true; el.playsInline = true; el.autoplay = true; }
        el.dataset.media = key;
        el.style.cssText = "position:absolute;background:#000;border-radius:6px;display:none";
        theLayer().appendChild(el);
      }
      return el;
    }
    function paint(s, key, frame) {
      const md = s.media;
      if (!md) return;
      const c = tile(s, key, false);
      if (c.width !== frame.displayWidth || c.height !== frame.displayHeight) { c.width = frame.displayWidth; c.height = frame.displayHeight; }
      c.getContext("2d").drawImage(frame, 0, 0);
      c._at = Date.now();
    }
    function drawAll(s) {
      const md = s.media;
      if (!md) return;
      const keep = {};
      if (md.stream && md.video) keep.me = md.stream;
      if (md.xstream) keep["me#screen"] = md.xstream;
      const now = Date.now();
      for (const id in md.rx) {
        const rx = md.rx[id];
        if (rx.last.cam && now - rx.last.cam < GONE_AFTER) keep[id] = true;
        if (rx.last.scr && now - rx.last.scr < GONE_AFTER) keep[id + "#screen"] = true;
      }
      for (const k in md.tiles) if (!keep[k]) { md.tiles[k].remove(); delete md.tiles[k]; }
      for (const k in keep) {
        const el = keep[k] === true ? tile(s, k, false) : tile(s, k, true);
        if (keep[k] !== true && el.srcObject !== keep[k]) { el.srcObject = keep[k]; el.play().catch(() => {}); }
        const at = md.place[k];
        if (at && at.w > 0 && at.h > 0) {
          Object.assign(el.style, { display: "block", left: at.x + "px", top: at.y + "px", width: at.w + "px", height: at.h + "px",
                                    objectFit: at.fit, objectPosition: "center", transform: at.mirror ? "scaleX(-1)" : "" });
        } else el.style.display = "none";
      }
      sound(s);
      drawPill();
    }
    // Every voice: heard only if the page said so, as loud as it asked, placed if the scene says where.
    function sound(s) {
      const md = s.media;
      if (!md || !ctx) return;
      const sc = md.spatial, L = ctx.listener;
      if (sc && L.positionX) {
        const [p, f, u] = [sc.listener.p, sc.listener.f, sc.listener.u];
        L.positionX.value = p[0]; L.positionY.value = p[1]; L.positionZ.value = p[2];
        L.forwardX.value = f[0]; L.forwardY.value = f[1]; L.forwardZ.value = f[2];
        L.upX.value = u[0]; L.upY.value = u[1]; L.upZ.value = u[2];
      }
      for (const id in md.rx) for (const slot of ["mic", "snd"]) {
        const d = md.rx[id].dec[slot], o = d && d.out;
        if (!o) continue;
        const key = slot === "mic" ? id : id + "#screen", src = sc && sc.sources[key];
        const level = key in md.volume ? md.volume[key] : id in md.volume ? md.volume[id] : 1;
        o.vol.gain.value = md.hear[id] ? level : 0;
        const lp = sc ? sc.listener.p : [L.positionX ? L.positionX.value : 0, L.positionY ? L.positionY.value : 0, L.positionZ ? L.positionZ.value : 0];
        const pos = src ? src.p : [lp[0], lp[1], lp[2] - 1];             // no place: straight ahead
        if (o.pan.positionX) { o.pan.positionX.value = pos[0]; o.pan.positionY.value = pos[1]; o.pan.positionZ.value = pos[2]; }
        if (src) {
          const dx = pos[0] - lp[0], dy = pos[1] - lp[1], dz = pos[2] - lp[2], dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
          o.fade.gain.value = dist <= src.full ? 1 : dist >= src.end ? 0 : Math.pow(Math.cos(Math.PI / 2 * (dist - src.full) / (src.end - src.full)), 2);
          if (src.cone) { o.pan.coneInnerAngle = src.cone[0]; o.pan.coneOuterAngle = src.cone[1]; o.pan.coneOuterGain = src.cone[2]; }
        } else o.fade.gain.value = 1;
      }
    }

    // Every 150 ms: who is speaking, who stopped sending, frames and levels the page asked for.
    function watch(s) {
      const md = s.media;
      if (!md) return;
      const now = Date.now();
      for (const id in md.rx) said(s, id);
      const level = (id, lv, p) => {
        const st = md.speaking[id] || (md.speaking[id] = { on: false, at: 0 });
        if (lv > SPEAK_ON) st.at = now;
        const on = lv > SPEAK_ON || (st.on && now - st.at < SPEAK_HOLD);
        if (on !== st.on) { st.on = on; say(s, { type: "media-speaking", member: p, on }); }
      };
      for (const id in md.rx) {
        const peer = peerOf(s, id), rx = md.rx[id];
        if (!peer) continue;
        const lv = md.hear[id] && rx.last.mic && now - rx.last.mic < 500 ? (rx.level.mic || 0) : 0;
        if (rx.level.mic) rx.level.mic *= 0.8;
        level(id, lv, peer.p);
      }
      const mic = md.enc.mic;
      if (s.me) level(s.me.id, mic && !md.muted.audio ? mic.level : 0, s.me);
      if (mic) mic.level *= 0.8;
    }
    function framesStart(s, key, want) {
      const md = s.media;
      framesStop(md, key);
      const f = (md.frames[key] = { busy: false });
      f.timer = setInterval(() => {
        if (s.media !== md || md.frames[key] !== f || f.busy) return;
        const el = md.tiles[key];
        const W = el && (el.tagName === "VIDEO" ? el.videoWidth : el.width), H = el && (el.tagName === "VIDEO" ? el.videoHeight : el.height);
        if (!W || !H) return;
        const scale = Math.min(1, want.w / W, want.h / H), w = Math.max(1, Math.round(W * scale)), h = Math.max(1, Math.round(H * scale));
        const t = Date.now();
        f.busy = true;
        createImageBitmap(el, { resizeWidth: w, resizeHeight: h }).then((bm) => {
          f.busy = false;
          if (s.media !== md || md.frames[key] !== f) { bm.close(); return; }
          const owner = key.replace(/#screen$/, ""), p = owner === "me" ? s.me : (peerOf(s, owner) || {}).p || null;
          say(s, { type: "media-frame", key, member: p, t, w, h, bitmap: bm });
        }, () => { f.busy = false; });
      }, Math.round(1000 / want.fps));
    }
    function framesStop(md, key) { const f = md.frames[key]; if (f) clearInterval(f.timer); delete md.frames[key]; }
    function levelsTick(s) {
      const md = s.media, lv = md && md.levels;
      if (!lv) return;
      const c = lv.canvas || (lv.canvas = document.createElement("canvas")), g = c.getContext("2d", { willReadFrequently: true });
      c.width = 4; c.height = 4;
      const out = {};
      for (const key of lv.keys) {
        const el = md.tiles[key];
        let rgb = null;
        const W = el && (el.tagName === "VIDEO" ? el.videoWidth : el.width);
        if (W) {
          try {
            g.drawImage(el, 0, 0, 4, 4);
            const d = g.getImageData(0, 0, 4, 4).data; let r = 0, gg = 0, b = 0;
            for (let i = 0; i < d.length; i += 4) { r += d[i]; gg += d[i + 1]; b += d[i + 2]; }
            rgb = [Math.round(r / 16), Math.round(gg / 16), Math.round(b / 16)];
          } catch (err) {}
        }
        const owner = key.replace(/#screen$/, ""), rx = md.rx[owner];
        const loud = rx && md.hear[owner] ? (rx.level[key === owner ? "mic" : "snd"] || 0) : 0;
        out[key] = { rgb, loud: Math.round(Math.min(1, loud) * 100) / 100 };
      }
      say(s, { type: "media-levels", levels: out });
    }
    function spatialClean(scene) {
      if (!scene || typeof scene !== "object") return null;
      const v3 = (a, d) => (Array.isArray(a) && a.length === 3 && a.every((x) => isFinite(Number(x))) ? a.map(Number) : d);
      const l = scene.listener || {}, out = { listener: { p: v3(l.p, [0, 0, 0]), f: v3(l.f, [0, 0, -1]), u: v3(l.u, [0, 1, 0]) }, sources: {} };
      const src = scene.sources && typeof scene.sources === "object" ? scene.sources : {};
      Object.keys(src).slice(0, TILES).forEach((k) => {
        const s0 = src[k] || {}, cone = Array.isArray(s0.cone) ? s0.cone.map(Number) : null;
        const full = Math.max(0, Number(s0.full) || 0), end = Math.max(full + 0.01, Number(s0.end) || 30);
        out.sources[String(k)] = { p: v3(s0.p, [0, 0, 0]), d: v3(s0.d, null), full, end, cone: cone && cone.length === 3 && cone.every(isFinite) ? cone : null };
      });
      return out;
    }

    // ---------------- starting and stopping ----------------
    async function allowed() {
      const p = await perms();
      if (!p.media) throw new Error("off: Camera & microphone is off on this app’s card");
    }
    function stopStream(s) {
      const md = s.media;
      stopSlot(s, "cam"); stopSlot(s, "mic");
      if (md.stream) md.stream.getTracks().forEach((t) => t.stop());
      md.stream = null; md.video = md.audio = md.screen = false;
    }
    function stopScreen(s, told) {
      const md = s.media;
      if (!md || !md.xstream) return;
      stopSlot(s, "scr"); stopSlot(s, "snd");
      md.xstream.getTracks().forEach((t) => t.stop());
      md.xstream = null;
      note(s, Object.keys(md.share).filter((id) => direct(s, id)), { screenOff: 1 });
      drawAll(s);
      if (told) say(s, { type: "media-state", state: "screen-off" });
    }
    function stopAll(s, quiet) {
      const md = s.media;
      if (!md) return;
      const had = !!(md.stream || md.xstream);
      note(s, Object.keys(md.share).filter((id) => direct(s, id)), { off: 1 });
      stopStream(s); stopScreen(s, false);
      md.share = {};
      for (const k of Object.keys(md.frames)) framesStop(md, k);
      if (md.levels) { clearInterval(md.levels.timer); md.levels = null; }
      drawAll(s);
      if (had && !quiet) say(s, { type: "media-state", state: "off" });
    }

    async function door(s, m) {
      if (!s.fast) throw new Error("media needs a room joined with {fast: true}");
      const md = mediaOf(s);
      switch (m.do) {
        case "start": {
          const want = { video: !!m.video, audio: !!m.audio, screen: !!m.screen, sound: !!(m.sound && m.screen) };
          if (want.screen) want.video = false;
          if (!want.video && !want.audio && !want.screen) throw new Error("ask for video, audio, screen or a pair");
          const md0 = navigator.mediaDevices;
          if (want.screen && !(md0 && md0.getDisplayMedia)) throw new Error("no screen");
          if ((want.video || want.audio) && !(md0 && md0.getUserMedia)) throw new Error("denied");
          await allowed();
          let stream;
          try {
            const shown = want.screen ? await md0.getDisplayMedia({ video: { width: { max: 1920 }, height: { max: 1080 }, frameRate: { max: 15 } }, audio: want.sound }) : null;
            stream = want.video || want.audio ? await md0.getUserMedia({
              video: want.video ? Object.assign({ width: { max: 1280 }, height: { max: 1280 } }, m.facing ? { facingMode: { ideal: m.facing } } : {}) : false,
              audio: want.audio ? { echoCancellation: true, noiseSuppression: true, autoGainControl: true } : false,
            }) : new MediaStream();
            if (shown) shown.getTracks().forEach((t) => stream.addTrack(t));
          } catch (err) {
            if (err && err.name === "NotAllowedError" && want.screen) throw new Error("cancelled");
            say(s, { type: "media-state", state: "denied" });
            throw new Error("denied");
          }
          stopStream(s);
          md.stream = stream;
          md.video = stream.getVideoTracks().length > 0; md.audio = stream.getAudioTracks().length > 0; md.screen = want.screen && md.video;
          if (md.video) startVideo(s, "cam", KIND.cam, new MediaStream(stream.getVideoTracks()), want.screen ? SCR : CAM);
          const voice = stream.getAudioTracks();
          if (voice.length) startAudio(s, "mic", KIND.mic, new MediaStream(voice));
          if (md.screen) stream.getVideoTracks().forEach((t) => t.addEventListener("ended", () => {
            if (md.stream !== stream) return;
            stopSlot(s, "cam"); md.video = false; md.screen = false; drawAll(s);
            say(s, { type: "media-state", state: "screen-off" });
          }));
          drawAll(s);
          say(s, { type: "media-state", state: "on" });
          return { video: md.video, audio: md.audio, screen: md.screen };
        }
        case "stop": stopAll(s); return {};
        case "screen": {
          if (!m.on) { stopScreen(s, false); return {}; }
          const md0 = navigator.mediaDevices;
          if (!(md0 && md0.getDisplayMedia)) throw new Error("no screen");
          await allowed();
          let x;
          try { x = await md0.getDisplayMedia({ video: { width: { max: 1920 }, height: { max: 1080 }, frameRate: { max: 15 } }, audio: !!m.sound }); }
          catch (err) { throw new Error(err && err.name === "NotAllowedError" ? "cancelled" : String(err && (err.name || err.message) || err)); }
          stopScreen(s, false);
          md.xstream = x; md.muted.sound = false;
          startVideo(s, "scr", KIND.scr, new MediaStream(x.getVideoTracks()), SCR);
          if (x.getAudioTracks().length) startAudio(s, "snd", KIND.snd, new MediaStream(x.getAudioTracks()));
          x.getVideoTracks().forEach((t) => t.addEventListener("ended", () => { if (md.xstream === x) stopScreen(s, true); }));
          drawAll(s);
          return x.getAudioTracks().length ? { sound: true } : {};
        }
        case "share": {
          const id = String(m.peer || ""), peer = peerOf(s, id);
          if (!peer) throw new Error("no such player");
          const kinds = m.on && typeof m.on === "object" ? { video: !!m.on.video, audio: !!m.on.audio } : { video: !!m.on, audio: !!m.on };
          if (!kinds.video && !kinds.audio) { delete md.share[id]; note(s, direct(s, id) ? [id] : [], { off: 1 }); return {}; }
          if (!direct(s, id)) throw new Error("not direct");
          md.share[id] = kinds;
          for (const slot of ["cam", "scr"]) if (md.enc[slot]) md.enc[slot].wantKey = true;      // the new viewer needs a key frame
          return { sending: !!(md.stream || md.xstream) };
        }
        case "hear": {
          const id = String(m.peer || "");
          if (!peerOf(s, id)) throw new Error("no such player");
          if (m.on) md.hear[id] = true; else delete md.hear[id];
          sound(s); return {};
        }
        case "volume": {
          const level = Number(m.level);
          if (!isFinite(level)) throw new Error("a volume is a number from 0 to " + LOUDEST);
          md.volume[String(m.peer || "")] = Math.min(LOUDEST, Math.max(0, level));
          sound(s); return {};
        }
        case "place": {
          const key = m.who === "me" ? "me" : String(m.who || ""), owner = key.replace(/#screen$/, "");
          if (owner !== "me" && !peerOf(s, owner)) throw new Error("no such player");
          const r = m.rect;
          if (r == null) delete md.place[key];
          else {
            if (!md.place[key] && Object.keys(md.place).length >= TILES) throw new Error("too many pictures");
            const n = (v) => (isFinite(Number(v)) ? Number(v) : 0);
            md.place[key] = { x: n(r.x), y: n(r.y), w: Math.max(0, n(r.w)), h: Math.max(0, n(r.h)), fit: m.fit === "contain" ? "contain" : "cover", mirror: !!m.mirror };
          }
          drawAll(s); return {};
        }
        case "mute": {
          if (typeof m.video === "boolean") md.muted.video = m.video;
          if (typeof m.audio === "boolean") md.muted.audio = m.audio;
          if (typeof m.sound === "boolean") md.muted.sound = m.sound;
          drawPill(); return { muted: { ...md.muted } };
        }
        case "frames": {
          const key = m.key === "me" || m.key === "me#screen" ? m.key : String(m.key || ""), owner = key.replace(/#screen$/, "");
          if (!m.size) { framesStop(md, key); drawPill(); return {}; }
          if (owner !== "me" && !peerOf(s, owner)) throw new Error("no such player");
          if (!(await perms()).pictures) throw new Error("off: Pictures in the app is off on this app’s card");
          const n = (v, lo, hi) => { v = Number(v); return isFinite(v) ? Math.min(hi, Math.max(lo, Math.round(v))) : hi; };
          framesStart(s, key, { w: n(m.size.w, 16, FRAME_MAX_W), h: n(m.size.h, 16, FRAME_MAX_H), fps: n(m.size.fps, 1, FRAME_MAX_FPS) });
          drawPill(); return {};
        }
        case "levels": {
          const keys = Array.isArray(m.keys) ? m.keys.slice(0, TILES).map(String) : [];
          if (md.levels) { clearInterval(md.levels.timer); md.levels = null; }
          if (!keys.length) { drawPill(); return {}; }
          if (!(await perms()).pictures) throw new Error("off: Pictures in the app is off on this app’s card");
          md.levels = { keys, timer: setInterval(() => levelsTick(s), 500) };
          drawPill(); return {};
        }
        case "spatial": md.spatial = spatialClean(m.scene); sound(s); return {};
        case "shown": return {};
        default: throw new Error("media ops are start, stop, screen, share, hear, volume, place, mute, frames, spatial and levels");
      }
    }

    return {
      door,
      heard,
      // A player left (or the page stopped linking with them): what was theirs goes.
      gone(s, id) {
        const md = s.media;
        if (!md) return;
        const rx = md.rx[id], peer = peerOf(s, id);
        if (rx) {
          for (const slot in rx.dec) { const d = rx.dec[slot]; try { d.dec.close(); } catch (err) {} if (d.out) try { d.out.node.disconnect(); d.out.vol.disconnect(); } catch (err) {} }
          if (rx.said && peer) say(s, { type: "media-off", member: peer.p });
          delete md.rx[id];
        }
        delete md.share[id]; delete md.hear[id]; delete md.place[id]; delete md.place[id + "#screen"];
        drawAll(s);
      },
      // The room is left: everything off, quietly.
      close(s) {
        if (!s.media) return;
        stopAll(s, true);
        for (const id of Object.keys(s.media.rx)) this.gone(s, id);
        clearInterval(s.media.watch);
        for (const k in s.media.tiles) s.media.tiles[k].remove();
        seats.delete(s);
        s.media = null;
        drawPill();
      },
    };
  };
})();
