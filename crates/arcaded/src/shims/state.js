/* Arcade V1.0: arcade.state for app pages (the original arcade's state.js API): a game's own record on each piece. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.state) return;
  var seq = 0, waiting = {};
  window.addEventListener('message', function (e) {
    var m = e.data;
    if (!m || m.arcade !== 'state' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve({ txids: m.txids, updates: m.updates }); else w.reject(new Error(m.error || 'refused'));
  });
  window.arcade.state = {
    get: function (piece, family) {
      return fetch('/r/state/' + (family ? encodeURIComponent(family) + '/' : '') + encodeURIComponent(piece)).then(function (r) { return r.json(); });
    },
    update: function (items, o) {
      return new Promise(function (resolve, reject) {
        var s = ++seq;
        var t = setTimeout(function () { delete waiting[s]; reject(new Error('no wallet is listening: open this page in a DogecoinArcade viewer')); }, 120000);
        waiting[s] = { resolve: resolve, reject: reject, t: t };
        window.parent.postMessage({ arcade: 'state', seq: s, op: 'update', items: items, replay: (o || {}).replay }, '*');
      });
    }
  };
})();
