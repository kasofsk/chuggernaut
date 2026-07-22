import { useEffect, useState } from 'react'
import { api, type FleetStatus, type Job } from '../api'
import { activityBuckets } from '../jobFilters'
import { Sparkline } from './Sparkline'

const NON_TERMINAL = new Set(['Done', 'Revoked', 'Batched'])

type Health = { level: 'ok' | 'degraded' | 'down'; label: string }

/**
 * System status footer (#163): a slim full-width band on project pages —
 * SYSTEM HEALTH (dispatcher liveness), FLEET CAPACITY (a bar + slot readout
 * from the live feed, hidden when unavailable), and ACTIVE JOBS (non-terminal
 * count + a live activity sparkline). Ambient by design; the floating capacity
 * widget (#148) keeps the interactive per-node detail.
 */
export function StatusFooter({
  jobs,
  fleet,
  fleetUnavailable,
}: {
  jobs: Job[]
  fleet: FleetStatus | null
  fleetUnavailable: boolean
}) {
  const [health, setHealth] = useState<Health | null>(null)
  // Health is a dispatcher round-trip (NATS req/reply), so probe on mount and a
  // slow visible-tab interval rather than on every project event.
  useEffect(() => {
    let ok = true
    const probe = () => {
      if (document.hidden) return
      api.health().then(
        (h) =>
          ok &&
          setHealth({
            level: 'ok',
            label: h.version ? `All systems nominal · ${h.version}` : 'All systems nominal',
          }),
        (e) => {
          if (!ok) return
          const msg = e && typeof e === 'object' && 'body' in e && (e as { body?: { error?: string } }).body?.error
          setHealth({ level: 'down', label: msg ? `dispatcher: ${msg}` : 'dispatcher unreachable' })
        },
      )
    }
    probe()
    const id = window.setInterval(probe, 30_000)
    return () => {
      ok = false
      window.clearInterval(id)
    }
  }, [])

  const active = jobs.filter((j) => !NON_TERMINAL.has(j.state)).length
  const spark = activityBuckets(jobs, 20)

  // Fleet segment: same graceful-degrade rule as the capacity widget — hidden
  // when the feed is unavailable or nothing sized has been published.
  const sized = fleet?.nodes.filter((n) => n.slots != null) ?? []
  const total = sized.reduce((n, x) => n + (x.slots ?? 0), 0)
  const busy = fleet?.nodes.reduce((n, x) => n + x.occupied, 0) ?? 0
  const queue = fleet?.queue_depth ?? 0
  const showFleet = !fleetUnavailable && fleet != null && total > 0
  const pct = total > 0 ? Math.round((busy / total) * 100) : 0
  const band = queue > 0 || busy >= total ? 'full' : busy / Math.max(total, 1) >= 0.75 ? 'high' : 'ok'

  return (
    <footer className="status-footer card" role="contentinfo">
      <div className="sf-seg sf-health">
        <span className="sf-label">System health</span>
        <span className={`sf-health-body sf-h-${health?.level ?? 'ok'}`}>
          <span className="sf-dot" />
          {health?.label ?? 'checking…'}
        </span>
      </div>

      {showFleet && (
        <div className="sf-seg sf-fleet" data-band={band}>
          <span className="sf-label">Fleet capacity</span>
          <span className="sf-fleet-body">
            <span className="sf-bar">
              <span className="sf-bar-fill" style={{ width: `${pct}%` }} />
            </span>
            <span className="sf-fleet-read">
              {pct}% <span className="dim">· {busy}/{total} slots used</span>
              {queue > 0 && <span className="sf-queue"> · {queue} queued</span>}
            </span>
          </span>
        </div>
      )}

      <div className="sf-seg sf-active">
        <span className="sf-label">Active jobs</span>
        <span className="sf-active-body">
          <span className="sf-active-n">{active}</span>
          <Sparkline data={spark} width={120} height={26} className="sf-spark" />
        </span>
      </div>
    </footer>
  )
}
