// Minimal service worker to make the operator UI installable as a PWA
// (Android/Chrome require a registered SW with a fetch handler) and to give
// the app shell a basic offline fallback. This is intentionally conservative:
// live data (`/api`, `/auth`, the SSE stream) is NEVER cached — only the
// static, content-hashed build assets and the navigation shell are.
const CACHE = 'chug-shell-v1'
// A separate, activate-preserved cache holding a single shared file handed off
// from the OS share sheet (see the share-target handler below). Kept out of the
// shell cache so the shell can be versioned/wiped without dropping a pending
// share, and vice versa.
const SHARE_CACHE = 'chug-share-v1'
const SHARED_KEY = '/__shared'
const SHELL = ['/', '/favicon.png', '/manifest.webmanifest', '/icon-192.png', '/icon-512.png']

self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()))
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      // Preserve both the shell cache and any pending shared file.
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== CACHE && k !== SHARE_CACHE).map((k) => caches.delete(k))),
      )
      .then(() => self.clients.claim()),
  )
})

// PWA share target (job #174 UI): the OS share sheet POSTs the shared screenshot
// here as multipart/form-data. We can't render inside the POST response, so we
// stash the file in SHARE_CACHE and 303-redirect into the SPA's /share screen,
// which reads it back from `/__shared` and offers "attach to job".
async function handleShareTarget(req) {
  try {
    const form = await req.formData()
    const file = form.get('image') || form.get('file')
    const title = String(form.get('title') || form.get('text') || '')
    if (file && file.size) {
      const cache = await caches.open(SHARE_CACHE)
      await cache.put(
        SHARED_KEY,
        new Response(file, {
          headers: {
            'Content-Type': file.type || 'application/octet-stream',
            // Header-safe: filenames/titles can carry newlines/unicode.
            'X-Filename': encodeURIComponent((file.name || 'shared').slice(0, 255)),
            'X-Share-Title': encodeURIComponent(title.slice(0, 500)),
          },
        }),
      )
    }
  } catch (e) {
    // Swallow — land the operator on /share regardless; it copes with no file.
  }
  return Response.redirect(new URL('/share', self.location.origin).href, 303)
}

// Serve the stashed shared file back to the /share screen, then it's the SPA's
// to consume. Same-origin only (the fetch caller lives in our own scope).
async function serveShared() {
  const cache = await caches.open(SHARE_CACHE)
  const hit = await cache.match(SHARED_KEY)
  return hit || new Response(null, { status: 404 })
}

self.addEventListener('fetch', (event) => {
  const req = event.request
  const url = new URL(req.url)
  const sameOrigin = url.origin === self.location.origin

  // Share-target POST + its stashed-file readback come first: both are our own
  // synthetic routes, handled before the read-through GET logic below.
  if (sameOrigin && req.method === 'POST' && url.pathname === '/share-target') {
    event.respondWith(handleShareTarget(req))
    return
  }
  if (sameOrigin && req.method === 'GET' && url.pathname === SHARED_KEY) {
    event.respondWith(serveShared())
    return
  }

  if (req.method !== 'GET') return
  // Never touch live data or cross-origin requests.
  if (!sameOrigin) return
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
