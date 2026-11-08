/* Arcade V1.0: arcade.llm for app pages (the original arcade's llm.js API). This arcade does not reach model
   sellers yet: offers() is empty and chat/image say so. */
(function () {
  'use strict';
  window.arcade = window.arcade || {};
  if (window.arcade.llm) return;
  var why = 'no model seller is reachable from this arcade yet';
  window.arcade.llm = {
    offers: function () { return Promise.resolve([]); },
    chat: function () { return Promise.reject(new Error(why)); },
    image: function () { return Promise.reject(new Error(why)); }
  };
})();
