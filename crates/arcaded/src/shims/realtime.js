/* Arcade V1.0: arcade.realtime for app pages (the original arcade's realtime.js API). Rooms travel the
   arcade mesh. join() never rejects: a room it can't join comes back offline, with why, so apps keep
   working alone. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.realtime) return;
  var seq = 0, waiting = {}, rooms = {};
  function ask(message, wait) {
    return new Promise(function (resolve, reject) {
      message.arcade = 'realtime'; message.seq = ++seq;
      var t = setTimeout(function () { delete waiting[message.seq]; reject(new Error('no viewer is listening: open this page in a DogecoinArcade viewer')); }, wait || 20000);
      waiting[message.seq] = { resolve: resolve, reject: reject, t: t };
      window.parent.postMessage(message, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data;
    if (!m || m.arcade !== 'realtime') return;
    if (m.event) { var r = rooms[m.event.room]; if (r) r._hear(m.event); return; }
    var w = waiting[m.seq]; if (!w) return;
    if (m.pending === 'unlock') return;
    delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  function Room(name, said) {
    this.name = name; this.online = !!said.online; this.me = said.me || null; this.why = said.why || '';
    this.fast = null; this.media = null; this._members = {}; this._on = {};
    var self = this; (said.members || []).forEach(function (p) { self._members[p.id] = p; });
  }
  var TYPES = ['message', 'join', 'leave', 'closed', 'rejoined', 'reconnecting'];
  Room.prototype.on = function (type, fn) {
    if (TYPES.indexOf(type) < 0) throw new Error('room events are ' + TYPES.join(', '));
    (this._on[type] = this._on[type] || []).push(fn); return this;
  };
  Room.prototype._emit = function (type) { var a = [].slice.call(arguments, 1); (this._on[type] || []).forEach(function (f) { try { f.apply(null, a); } catch (e) { console.error(e); } }); };
  Room.prototype.members = function () { var o = []; for (var k in this._members) o.push(this._members[k]); return o; };
  Room.prototype.send = function (value) {
    if (!this.online) return Promise.reject(new Error(this.why || 'this room is offline'));
    return ask({ op: 'send', room: this.name, data: typeof value === 'string' ? value : JSON.stringify(value) });
  };
  Room.prototype.leave = function () { delete rooms[this.name]; this.online = false; return ask({ op: 'leave', room: this.name }).catch(function () {}); };
  Room.prototype._hear = function (ev) {
    var p = ev.member;
    if (ev.type === 'join') { this._members[p.id] = p; this._emit('join', p); }
    else if (ev.type === 'leave') { delete this._members[p.id]; this._emit('leave', p); }
    else if (ev.type === 'closed') { this.online = false; this._emit('closed', ev.why); }
    else if (ev.type === 'me') { this.me = p; }
    else if (ev.type === 'rejoined') { this.me = p || this.me; this.online = true; this._emit('rejoined', this.me, ev.media || null); }
    else if (ev.type === 'reconnecting') { this.online = false; this._emit('reconnecting'); }
    else if (ev.type === 'message') {
      var d = ev.data; if (typeof d === 'string') { try { d = JSON.parse(d); } catch (e) { /* text */ } } else if (ev.b64) d = ev.b64;
      this._emit('message', d, p);
    }
  };
  function joinOne(name, o) {
    if (rooms[name]) return Promise.resolve(rooms[name]);
    var msg = { op: 'join', room: name, game: o.game || '', fast: !!o.fast, waits: true };
    if (o.link) msg.link = o.link; if (o.max) msg.max = o.max;
    return ask(msg, 600000).then(function (said) {
      var r = new Room(name, said); if (r.online) rooms[name] = r; return r;
    }, function (e) { return new Room(name, { online: false, why: String(e && e.message || e) }); });
  }
  function join(name, options) {
    var o = options || {};
    if (!o.cap) return joinOne(name, o);
    var i = 1;
    function next() {
      var n = i === 1 ? name : name + '#' + i;
      return joinOne(n, o).then(function (r) {
        var others = r.members().filter(function (p) { return !r.me || p.id !== r.me.id; }).length;
        if (r.online && others >= o.cap && i < 50) { r.leave(); i++; return next(); }
        return r;
      });
    }
    return next();
  }
  function area(base, o) {
    o = o || {};
    var cell = o.cell || 64;
    return join(base + '@' + Math.floor((o.x || 0) / cell) + ',' + Math.floor((o.y || 0) / cell), o);
  }
  function ring(address, o) { o = o || {}; return ask({ op: 'ring', room: '', to: address, line: o.line || '', link: o.link || '' }, 60000); }
  function ringed() { return ask({ op: 'ringed', room: '' }).then(function (m) { return m.ring || null; }); }
  window.arcade.realtime = { join: join, area: area, ring: ring, ringed: ringed, MAX_BYTES: 512 };
})();
