'use strict';
/* QuicMic service worker: offline app shell for the installed PWA.
   Only same-origin GETs for static assets are cached. The API, the
   pairing flow and both audio sockets always go to the network — caching
   any of those would break liveness detection and streaming. */

var CACHE = 'quicmic-shell-v1';
var SHELL = [
  '/',
  '/mic.html',
  '/drop.html',
  '/speaker.html',
  '/style.css',
  '/drop.css',
  '/landing.css',
  '/app.js',
  '/drop.js',
  '/speaker.js',
  '/landing.js',
  '/worklet.js',
  '/speaker-worklet.js',
  '/qrcode.min.js',
  '/manifest.webmanifest',
  '/icons/icon-192.png',
  '/icons/icon-512.png',
];

self.addEventListener('install', function (e) {
  e.waitUntil(
    caches.open(CACHE).then(function (c) { return c.addAll(SHELL); })
      .then(function () { return self.skipWaiting(); })
      .catch(function () { /* best-effort: stay functional without the cache */ })
  );
});

self.addEventListener('activate', function (e) {
  e.waitUntil(
    caches.keys().then(function (keys) {
      return Promise.all(keys.map(function (k) {
        return k === CACHE ? null : caches.delete(k);
      }));
    }).then(function () { return self.clients.claim(); })
  );
});

function isCacheable(req) {
  if (req.method !== 'GET') return false;
  var u = new URL(req.url);
  if (u.origin !== self.location.origin) return false;
  if (u.pathname.indexOf('/api/') === 0) return false;
  if (u.pathname === '/ca') return false;
  return true;
}

self.addEventListener('fetch', function (e) {
  if (!isCacheable(e.request)) return;
  e.respondWith(
    caches.match(e.request).then(function (hit) {
      if (hit) return hit;
      return fetch(e.request).then(function (res) {
        // Cache a copy of fresh shell assets for next time.
        if (res && res.ok) {
          var copy = res.clone();
          caches.open(CACHE).then(function (c) { c.put(e.request, copy); });
        }
        return res;
      });
    })
  );
});
