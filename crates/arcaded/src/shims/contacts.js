/* Arcade V1.0: arcade.contacts for app pages (the original arcade's contacts.js API). */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.contacts) return;
  var seq = 0, waiting = {};
  function ask(message, wait, soft) {
    return new Promise(function (resolve, reject) {
      if (window.parent === window) { if (soft) return resolve(null); return reject(new Error('no viewer: open this page in the arcade to change your address book')); }
      message.arcade = 'contacts'; message.seq = ++seq;
      var t = setTimeout(function () { delete waiting[message.seq]; if (soft) resolve(null); else reject(new Error('no viewer: nothing answered; open this page in the arcade')); }, wait);
      waiting[message.seq] = { resolve: resolve, reject: reject, t: t, soft: soft };
      window.parent.postMessage(message, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data;
    if (!m || m.arcade !== 'contacts' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve(m); else if (w.soft) w.resolve(null); else w.reject(new Error(m.error || 'refused'));
  });
  function addr(who) { return typeof who === 'string' ? who : (who && who.address) || ''; }
  window.arcade.contacts = {
    list: function () {
      return ask({ op: 'list' }, 120000, true).then(function (m) {
        var out = (m && m.people) || [];
        if (!m) out.why = 'viewer'; else if (m.why) out.why = m.why;
        return out;
      });
    },
    add: function (who) {
      var msg = { op: 'add', address: addr(who) };
      if (who && typeof who === 'object' && who.name) msg.name = String(who.name);
      return ask(msg, 300000).then(function (m) { return { ok: true, status: m.status }; });
    },
    remove: function (who) { return ask({ op: 'remove', address: addr(who) }, 300000).then(function (m) { return { ok: true, status: m.status }; }); }
  };
})();
