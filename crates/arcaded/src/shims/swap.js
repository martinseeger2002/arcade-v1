/* Arcade V1.0: arcade.swap for app pages. Shops and player-to-player trades are not on this arcade yet: every
   call says so instead of waiting; describe() works. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.swap) return;
  var no = function () { return Promise.reject(new Error('shops and trades are not on this arcade yet')); };
  function describe(l) {
    if (!l) return '';
    if (l.kind === 'coins' || l.coins) return (l.amount || l.coins) + ' coins';
    if (l.kind === 'token' || l.token) return (l.amount || '') + ' ' + (l.name || ('token ' + (l.propertyid || l.token)));
    if (l.kind === 'random' || l.pick) return 'a random ' + (l.collection || 'piece');
    if (l.kind === 'inscription' || l.inscription) return 'inscription #' + (l.number || l.inscription);
    return 'something';
  }
  var api = { describe: describe, onTrade: function () { return api; } };
  ['shop', 'offer', 'awaitOffer', 'accept', 'status', 'awaitDecision', 'awaitSwap', 'buy', 'trade', 'answer', 'cancel', 'items'].forEach(function (k) { api[k] = no; });
  window.arcade.swap = api;
})();
