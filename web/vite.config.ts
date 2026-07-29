import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev server proxies API + auth to a running `chuggernaut api` — a local one
// by default (bind 0.0.0.0:8080), or any reachable deployment via CHUG_API
// (e.g. `CHUG_API=https://gumbo-mini-0.tail20c474.ts.net npm run dev` to
// iterate with HMR against prod data). Production serves the built dist/ from
// the same axum server (UI_DIST), so no proxy is involved.
const apiTarget = process.env.CHUG_API ?? 'http://localhost:8080'

// The bundle's own build SHA, baked in at build time so the cluster view can
// show which commit the published web UI is on (and flag skew against the
// dispatcher/api). Populated by CHUG_GIT_SHA in the deploy web-publish leg
// (deploy/prod/update.sh) and the self-publish flow (.chug/tasks/web-publish.sh); an
// empty string for a local/dev build, which the UI renders as a dash.
const webSha = process.env.CHUG_GIT_SHA ?? ''

export default defineConfig({
  plugins: [react()],
  define: {
    __CHUG_WEB_SHA__: JSON.stringify(webSha),
  },
  server: {
    proxy: {
      '/api': { target: apiTarget, changeOrigin: true },
      '/auth': { target: apiTarget, changeOrigin: true },
    },
  },
})
