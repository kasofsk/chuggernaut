import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev server proxies API + auth to a running `chuggernaut api` — a local one
// by default (bind 0.0.0.0:8080), or any reachable deployment via CHUG_API
// (e.g. `CHUG_API=https://gumbo-mini-0.tail20c474.ts.net npm run dev` to
// iterate with HMR against prod data). Production serves the built dist/ from
// the same axum server (UI_DIST), so no proxy is involved.
const apiTarget = process.env.CHUG_API ?? 'http://localhost:8080'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': { target: apiTarget, changeOrigin: true },
      '/auth': { target: apiTarget, changeOrigin: true },
    },
  },
})
