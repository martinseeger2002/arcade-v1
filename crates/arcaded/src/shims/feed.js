/* Arcade V1.0: arcade.feed for app pages: the public feed (posts, replies, likes, shares, tips, follows), as the Feed
   tab had it. Reading is open to any app; writing (posts, acts, follows: fees paid from this arcade's wallet) only
   when this app's "Feed" switch is on. Answered by the arcade window that framed the page. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.feed) return;
  var seq = 0, waiting = {};
  function ask(message, wait) {
    return new Promise(function (resolve, reject) {
      message.arcade = 'feed'; message.seq = ++seq;
      var t = setTimeout(function () { delete waiting[message.seq]; reject(new Error('no viewer is listening, or the wallet is still locked')); }, wait || 120000);
      waiting[message.seq] = { resolve: resolve, reject: reject, t: t };
      window.parent.postMessage(message, '*');
    });
  }
  window.addEventListener('message', function (e) {
    if (e.source !== window.parent) return;          // only the arcade window that framed this page
    var m = e.data;
    if (!m || m.arcade !== 'feed' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq]; clearTimeout(w.t);
    if (m.ok) w.resolve(m.result); else w.reject(new Error(m.error || 'refused'));
  });
  window.arcade.feed = {
    // {order: popular|new|friends, channel, tag, limit, offset} -> {posts, total}
    read: function (o) { return ask({ op: 'read', view: o || {} }); },
    channels: function () { return ask({ op: 'channels' }); },
    settings: function (set) { return ask({ op: 'settings', set: set || null }); },
    // A picture attached to a post: {type, data (base64)}.
    file: function (txid) { return ask({ op: 'file', txid: txid }); },
    // Where a post can go (ARCADE, Pepecoin testnet, ...): {chains: [...], default}.
    chains: function () { return ask({ op: 'chains' }); },
    me: function () { return ask({ op: 'me' }); },
    // {text, channel, file: {name, type, data}, chain} -> {txid, txids, fee}
    post: function (p) { return ask({ op: 'post', post: p || {} }, 600000); },
    // kind: like, unlike, dislike, undislike, reply, share, tip, edit, delete; extra: {text, amount}
    act: function (kind, target, extra) { extra = extra || {}; return ask({ op: 'act', kind: kind, target: target, text: extra.text || '', amount: extra.amount || 0, chain: extra.chain }, 600000); },
    follow: function (who, on) { return ask({ op: 'follow', who: who, on: on !== false }, 600000); },
    follows: function (who) { return ask({ op: 'follows', who: who || '' }); },
    mute: function (address, on) { return ask({ op: 'mute', address: address, on: on !== false }); }
  };
})();
