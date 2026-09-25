'use strict';
/* Landing page logic:
   - Forward the pairing PIN hash (#123456) to the Mic and Speaker cards.
   - Register the service worker (PWA offline shell).
   - Surface the native install prompt when the browser offers it. */

(function () {
  var hash = window.location.hash || '';
  var hasPin = /^#\d{6}$/.test(hash);

  if (hasPin) {
    var mic = document.getElementById('card-mic');
    var spk = document.getElementById('card-speaker');
    if (mic) mic.href = 'mic.html' + hash;
    if (spk) spk.href = 'speaker.html' + hash;
    var line = document.getElementById('pair-line');
    if (line) {
      line.hidden = false;
      line.textContent = '✓ Paired to this PC — pick a screen';
    }
  }

  // Service worker: cache the app shell so the PWA opens instantly and
  // survives brief network hiccups. API and socket traffic bypass it.
  if ('serviceWorker' in navigator && window.location.protocol === 'https:') {
    window.addEventListener('load', function () {
      navigator.serviceWorker.register('sw.js').catch(function () { /* offline-first is best-effort */ });
    });
  }

  // Native install prompt (Android Chrome / desktop). iOS has no
  // beforeinstallprompt — the hint below covers Share → Add to Home Screen.
  var deferred = null;
  var box = document.getElementById('install-box');
  var btn = document.getElementById('btn-install');
  var isiOS = /iphone|ipad|ipod/i.test(navigator.userAgent);
  if (isiOS && !window.navigator.standalone) {
    var hint = document.getElementById('ios-install-hint');
    if (hint) hint.hidden = false;
  }
  window.addEventListener('beforeinstallprompt', function (e) {
    e.preventDefault();
    deferred = e;
    if (box) box.hidden = false;
  });
  if (btn) {
    btn.addEventListener('click', function () {
      if (!deferred) return;
      deferred.prompt();
      deferred.userChoice.then(function () { deferred = null; });
    });
  }
})();
