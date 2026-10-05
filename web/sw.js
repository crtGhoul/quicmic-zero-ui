'use strict';
/* QuicMic service worker: offline app shell for the installed PWA.
   Only same-origin GETs for static assets are cached. The API, the
   pairing flow and both audio sockets always go to the network — caching
   any of those would break liveness detection and streaming. */

var CACHE = 'quicmic-shell-v3';
var SHELL = [
  '/',
  '/mic.html',
  '/drop.html',
  '/speaker.html',
  '/guide.html',
  '/style.css',
  '/drop.css',
  '/landing.css',
  '/guide.css',
  '/app.js',
  '/drop.js',
  '/zip.js',
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
      // Stale-while-revalidate: serve the cached shell instantly, but always
      // refresh it in the background so the next load converges to the newest
      // assets. A pure cache-first handler would serve a stale app.js forever —
      // the server's no-cache + ETag revalidation would never get a chance —
      // stranding the phone on outdated client code after a server update.
      var refresh = fetch(e.request).then(function (res) {
        // Cache a copy of fresh shell assets for next time.
        if (res && res.ok) {
          var copy = res.clone();
          caches.open(CACHE).then(function (c) { c.put(e.request, copy); });
        }
        return res;
      });
      if (hit) {
        // The cached copy already served; a failed refresh (offline) is ignored.
        refresh.catch(function () {});
        return hit;
      }
      return refresh;
    })
  );
});
