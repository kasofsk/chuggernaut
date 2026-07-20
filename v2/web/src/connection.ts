import { useSyncExternalStore } from 'react'

// Connection health for the reconnecting banner (Part 11). Two inputs feed
// one boolean: browser online/offline, and the health of every live SSE
// subscription (each useProjectEvents instance reports by symbol so one
// erroring stream can't be cleared by another's open).

const downSources = new Set<symbol>()
let offline = typeof navigator !== 'undefined' ? !navigator.onLine : false
const listeners = new Set<() => void>()

function emit() {
  listeners.forEach((l) => l())
}

if (typeof window !== 'undefined') {
  window.addEventListener('online', () => {
    offline = false
    emit()
  })
  window.addEventListener('offline', () => {
    offline = true
    emit()
  })
}

export function markSse(id: symbol, down: boolean) {
  const had = downSources.has(id)
  if (down === had) return
  if (down) downSources.add(id)
  else downSources.delete(id)
  emit()
}

function subscribe(l: () => void) {
  listeners.add(l)
  return () => listeners.delete(l)
}

function snapshot(): boolean {
  return offline || downSources.size > 0
}

/** True when the browser is offline or any live SSE stream is erroring. */
export function useConnectionDegraded(): boolean {
  return useSyncExternalStore(subscribe, snapshot, () => false)
}
