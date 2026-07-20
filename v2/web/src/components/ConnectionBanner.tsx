import { useEffect } from 'react'
import { useConnectionDegraded } from '../connection'

// Part 11 offline UX: never a dead shell, never stale-as-live. While the
// browser is offline or an SSE stream is erroring, show a persistent banner
// and stamp <html data-degraded> so the stylesheet can disable action
// buttons (mutations would fail or land on a state the operator can't see).
export function ConnectionBanner() {
  const degraded = useConnectionDegraded()
  useEffect(() => {
    document.documentElement.toggleAttribute('data-degraded', degraded)
  }, [degraded])
  if (!degraded) return null
  return (
    <div className="conn-banner" role="status">
      reconnecting — live updates paused, actions disabled
    </div>
  )
}
