import { useEffect, useRef, useState } from 'react'
import { api, type FleetStatus } from './api'

export interface FleetState {
  /** last good snapshot, or null before the first successful load */
  fleet: FleetStatus | null
  /** true once a load has failed and we have never had data (403 non-admin, or
   *  the dispatcher/feed is down). Callers hide rather than render zeros. A
   *  transient failure after a good load keeps the last snapshot instead. */
  unavailable: boolean
  /** true once the first load attempt (success or failure) has settled */
  loaded: boolean
}

/**
 * The live fleet occupancy snapshot (spec §3.1, `GET .../fleet`). Refetched
 * whenever `tick` changes — drive it from the project SSE stream, on which every
 * occupancy change coincides with a task lifecycle event — and, when `pollMs` is
 * set, on a visible-tab interval (the top-level cluster view has no SSE to ride).
 */
export function useFleet({ tick = 0, pollMs }: { tick?: number; pollMs?: number }): FleetState {
  const [fleet, setFleet] = useState<FleetStatus | null>(null)
  const [unavailable, setUnavailable] = useState(false)
  const [loaded, setLoaded] = useState(false)
  const gotData = useRef(false)

  useEffect(() => {
    let cancelled = false
    const load = () => {
      if (document.hidden) return
      api.fleet().then(
        (f) => {
          if (cancelled) return
          gotData.current = true
          setFleet(f)
          setUnavailable(false)
          setLoaded(true)
        },
        () => {
          if (cancelled) return
          setLoaded(true)
          // Only hide when we have never had data; a blip after a good load
          // keeps the last snapshot so the widget doesn't flicker out.
          if (!gotData.current) setUnavailable(true)
        },
      )
    }
    load()
    if (!pollMs) return () => { cancelled = true }
    const id = window.setInterval(load, pollMs)
    const onVisible = () => { if (!document.hidden) load() }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      cancelled = true
      window.clearInterval(id)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [tick, pollMs])

  return { fleet, unavailable, loaded }
}
