/* Arcade V1.0: arcade.access for app pages (the original arcade's access.js API). */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.access) return;
  var seq = 0, waiting = {};
  function strip(m) { var o = {}; for (var k in m) if (k !== 'arcade' && k !== 'seq') o[k] = m[k]; return o; }
  function ask(message, wait) {
    if (window.parent === window) return Promise.reject(new Error('no viewer: open this page in the arcade'));
    return new Promise(function (resolve, reject) {
      message.arcade = 'access'; message.seq = ++seq;
      var t = setTimeout(function () { delete waiting[message.seq]; reject(new Error('no viewer: nothing answered; open this page in the arcade')); }, wait);
      waiting[message.seq] = { resolve: resolve, reject: reject, t: t };
      window.parent.postMessage(message, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data;
    if (!m || m.arcade !== 'access' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve(strip(m)); else w.reject(new Error(m.error || 'refused'));
  });
  window.arcade.access = {
    status: function () { return ask({ op: 'status' }, 15000); },
    wallet: function () { return ask({ op: 'wallet' }, 300000); },
    nodes: function () { return ask({ op: 'nodes' }, 30000).then(function (m) { return m.nodes || []; }); },
    move: function (base) { return ask({ op: 'move', to: String(base && base.base || base) }, 300000); }
  };
})();
