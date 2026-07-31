import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const apiTarget = process.env.CHUG_API ?? 'http://localhost:8080'

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
