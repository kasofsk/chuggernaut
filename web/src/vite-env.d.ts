/// <reference types="vite/client" />

// The web bundle's build SHA, injected by vite `define` (vite.config.ts) from
// CHUG_GIT_SHA at build time. Empty string on a local/dev build.
declare const __CHUG_WEB_SHA__: string
