// Minimal service worker to make the operator UI installable as a PWA
// (Android/Chrome require a registered SW with a fetch handler) and to give
// the app shell a basic offline fallback. This is intentionally conservative:
// live data (`/api`, `/auth`, the SSE stream) is NEVER cached — only the
// static, content-hashed build assets and the navigation shell are.
const CACHE = 'chug-shell-v1'
const SHELL = ['/', '/favicon.png', '/manifest.webmanifest', '/icon-192.png', '/icon-512.png']

self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()))
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  )
})

self.addEventListener('fetch', (event) => {
  const req = event.request
  if (req.method !== 'GET') return
  const url = new URL(req.url)
  // Never touch live data or cross-origin requests.
  if (url.origin !== self.location.origin) return
  if (url.pathname.startsWith('/api') || url.pathname.startsWith('/auth')) return

  // Navigations: serve the app shell, falling back to cache when offline so
  // the SPA still boots (it then talks to the network for live data).
  if (req.mode === 'navigate') {
    event.respondWith(fetch(req).catch(() => caches.match('/', { ignoreSearch: true })))
    return
  }

  // Static assets (content-hashed by Vite): cache-first, revalidate in the
  // background so new builds are picked up on the next load.
  event.respondWith(
    caches.match(req).then((cached) => {
      const network = fetch(req)
        .then((res) => {
          if (res.ok) {
            const copy = res.clone()
            caches.open(CACHE).then((c) => c.put(req, copy))
          }
          return res
        })
        .catch(() => cached)
      return cached || network
    }),
  )
})
