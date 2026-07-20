import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { VitePWA } from 'vite-plugin-pwa'

// Dev server proxies API + auth to a locally running `chuggernaut api`
// (default bind 0.0.0.0:8080). Production serves the built dist/ from the
// same axum server (UI_DIST), so no proxy is involved.
export default defineConfig({
  plugins: [
    react(),
    // Part 11: installable PWA. The service worker precaches the hashed app
    // shell only — /api, /auth, and the SSE streams are never cached (they
    // are not precached, have no runtime caching rules, and navigations to
    // them are denylisted, so those requests hit the network untouched).
    // autoUpdate: a new deploy refreshes the shell on next load, no prompt.
    VitePWA({
      registerType: 'autoUpdate',
      includeAssets: ['icon.svg', 'apple-touch-icon.png'],
      manifest: {
        name: 'Chuggernaut',
        short_name: 'Chuggernaut',
        description: 'Operator console for the Chuggernaut agent platform',
        start_url: '/',
        display: 'standalone',
        background_color: '#12141a',
        theme_color: '#12141a',
        icons: [
          { src: '/pwa-192.png', sizes: '192x192', type: 'image/png' },
          { src: '/pwa-512.png', sizes: '512x512', type: 'image/png' },
          { src: '/pwa-maskable-512.png', sizes: '512x512', type: 'image/png', purpose: 'maskable' },
        ],
      },
      workbox: {
        globPatterns: ['**/*.{js,css,html,svg,png,ico}'],
        navigateFallback: 'index.html',
        navigateFallbackDenylist: [/^\/api\//, /^\/auth\//],
      },
    }),
  ],
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/auth': 'http://localhost:8080',
    },
  },
})
