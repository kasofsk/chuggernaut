import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev server proxies API + auth to a locally running `chuggernaut api`
// (default bind 0.0.0.0:8080). Production serves the built dist/ from the
// same axum server (UI_DIST), so no proxy is involved.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/auth': 'http://localhost:8080',
    },
  },
})
