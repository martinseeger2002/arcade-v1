/* Arcade V1.0: arcade.escrow for app pages. Escrow (items held for a game, released by its judge) is not on this
   arcade yet: every call says so instead of waiting. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.escrow) return;
  var no = function () { return Promise.reject(new Error('escrow is not on this arcade yet')); };
  window.arcade.escrow = new Proxy({}, { get: function () { return no; } });
})();
