/* Arcade V1.0: arcade.node for app pages (the original arcade's node.js API): sealed node-to-node messages. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.node) return;
  var seq = 0, waiting = {};
  function ask(message) {
    return new Promise(function (resolve, reject) {
      message.arcade = 'node'; message.seq = ++seq;
      var t = setTimeout(function () { delete waiting[message.seq]; reject(new Error('no wallet is listening: open this page in a DogecoinArcade viewer')); }, 60000);
      waiting[message.seq] = { resolve: resolve, reject: reject, t: t };
      window.parent.postMessage(message, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data;
    if (!m || m.arcade !== 'node' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  var node = {
    identity: function () { return ask({ op: 'identity' }); },
    send: function (to, body) { return ask({ op: 'send', to: String(to), body: body }); },
    sent: function () { return ask({ op: 'sent' }).then(function (r) { return r.sent; }); },
    replies: function (o) { o = o || {}; return ask({ op: 'replies', after: o.after || 0, limit: o.limit || 100 }).then(function (r) { return r.replies; }); },
    listen: function (handler, o) {
      o = o || {}; var after = o.after, every = o.every || 10000, stopped = false, first = after === undefined;
      function poll() {
        if (stopped) return;
        node.replies({ after: after || 0 }).then(function (rs) {
          rs.forEach(function (r) { if (!first) handler(r); after = Math.max(after || 0, r.id); });
          first = false;
        }).catch(function () {}).then(function () { if (!stopped) setTimeout(poll, every); });
      }
      poll();
      return function stop() { stopped = true; };
    }
  };
  window.arcade.node = node;
})();
