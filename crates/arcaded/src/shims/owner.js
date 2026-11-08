/* Arcade V1.0: arcade.owner for app pages (the original arcade's owner.js API): a page this wallet created and
   still holds sends without asking (testnet only). */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.owner) return;
  var seq = 0, waiting = {};
  function ask(message) {
    return new Promise(function (resolve, reject) {
      message.arcade = 'owner'; message.seq = ++seq;
      var t = setTimeout(function () { delete waiting[message.seq]; reject(new Error('no wallet is listening: open this page in a DogecoinArcade viewer')); }, 60000);
      waiting[message.seq] = { resolve: resolve, reject: reject, t: t };
      window.parent.postMessage(message, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data;
    if (!m || m.arcade !== 'owner' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  window.arcade.owner = {
    identity: function () { return ask({ op: 'identity' }); },
    send: function (request) { var m = { op: 'send' }; for (var k in (request || {})) m[k] = request[k]; return ask(m); }
  };
})();
