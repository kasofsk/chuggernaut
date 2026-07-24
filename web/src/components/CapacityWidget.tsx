import { useState } from 'react'
import { Link } from 'react-router-dom'
import type { FleetStatus } from '../api'

const COLLAPSE_KEY = 'chug-capacity-collapsed'

// Load band → a data attribute the stylesheet maps onto the app's state hues:
// comfortable (green) → busy (blue) → full/queued (orange). Kept out of the
// component so colours stay in styles.css (no hard-coded hex here).
function loadBand(busy: number, total: number, queue: number): 'ok' | 'high' | 'full' {
  if (queue > 0 || (total > 0 && busy >= total)) return 'full'
  if (total > 0 && busy / total >= 0.75) return 'high'
  return 'ok'
}

/**
 * A sticky, always-visible floating cluster for the jobs page (#148): the fleet
 * busy/total slot fraction fed by the live fleet occupancy feed (spec §3.1), with
 * a per-slot dot row and the launch-queue depth when non-empty. Hover/tap expands
 * a per-node breakdown linking each busy slot back to its job, and through to the
 * cluster view. Collapsible to a dot (remembered in localStorage); the capacity
 * readout hides when the feed is unavailable rather than showing zeros.
 *
 * The "new job" launch button (#245) lives here too so it floats at the bottom of
 * the page where it is easy to thumb-tap; it always renders, even when the
 * capacity readout is hidden, so job creation is never lost with the feed.
 */
export function CapacityWidget({
  fleet,
  unavailable,
  newJobHref,
}: {
  fleet: FleetStatus | null
  unavailable: boolean
  newJobHref: string
}) {
  const [collapsed, setCollapsed] = useState(() => localStorage.getItem(COLLAPSE_KEY) === '1')
  const [expanded, setExpanded] = useState(false)

  const setCollapse = (v: boolean) => {
    setCollapsed(v)
    localStorage.setItem(COLLAPSE_KEY, v ? '1' : '0')
    if (v) setExpanded(false)
  }

  // The launch button is the anchor of the cluster and always renders. The
  // capacity pill is grafted on above it only when the feed has real numbers.
  const newJobButton = (
    <Link to={newJobHref} className="capacity-newjob" title="start a new job">
      + new job
    </Link>
  )

  // Feed down, non-admin, or nothing published yet → readout hidden (never zeros),
  // but the launch button still floats on its own.
  const sized = fleet?.nodes.filter((n) => n.slots != null) ?? []
  const total = sized.reduce((n, x) => n + (x.slots ?? 0), 0)
  if (unavailable || !fleet || total === 0) {
    return <div className="capacity-widget">{newJobButton}</div>
  }
  const busy = fleet.nodes.reduce((n, x) => n + x.occupied, 0)
  const queue = fleet.queue_depth
  const band = loadBand(busy, total, queue)

  if (collapsed) {
    return (
      <div className="capacity-widget" data-band={band}>
        <button
          type="button"
          className="capacity-dot"
          title={`fleet: ${busy} / ${total} slots busy${queue ? ` · ${queue} queued` : ''} — click to expand`}
          aria-label={`fleet capacity ${busy} of ${total} busy`}
          onClick={() => setCollapse(false)}
        />
        {newJobButton}
      </div>
    )
  }

  // One dot per slot (capped so a big fleet can't overflow the card); the first
  // `busy` read as filled.
  const dotCount = Math.min(total, 16)
  const filled = Math.round((busy / total) * dotCount)

  return (
    <div className="capacity-widget" data-band={band}>
      <div
        className="capacity-card"
        onMouseEnter={() => setExpanded(true)}
        onMouseLeave={() => setExpanded(false)}
      >
        <button
          type="button"
          className="capacity-summary"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
          title="fleet slot usage — click for the per-node breakdown"
        >
          <span className="capacity-frac">
            {busy} <span className="capacity-slash">/</span> {total}
          </span>
          <span className="capacity-dots" aria-hidden="true">
            {Array.from({ length: dotCount }, (_, i) => (
              <span key={i} className={`cap-dot${i < filled ? ' on' : ''}`} />
            ))}
          </span>
          {queue > 0 && <span className="capacity-queue">· {queue} queued</span>}
        </button>
        <button
          type="button"
          className="capacity-min"
          title="collapse"
          aria-label="collapse capacity widget"
          onClick={() => setCollapse(true)}
        >
          –
        </button>
        {expanded && (
          <div className="capacity-breakdown">
            {fleet.nodes.map((n) => (
              <div key={n.name} className={`cap-node${n.available ? '' : ' cap-out'}`}>
                <div className="cap-node-head">
                  <span className="cap-node-name">{n.name}</span>
                  <span className="cap-node-frac dim">
                    {n.occupied} / {n.slots ?? '?'}
                    {!n.available && ' · out'}
                  </span>
                </div>
                {n.running.length > 0 && (
                  <div className="cap-node-jobs">
                    {n.running.map((s) => (
                      <Link
                        key={`${s.project}:${s.job_seq}:${s.task_id}`}
                        className="cap-job"
                        to={`/p/${s.project}/jobs/${s.job_seq}`}
                        title={`${s.project} · ${s.job_type} · ${s.task_kind}`}
                      >
                        #{s.job_seq}
                      </Link>
                    ))}
                  </div>
                )}
              </div>
            ))}
            <Link className="capacity-cluster-link" to="/cluster">
              cluster view ↗
            </Link>
          </div>
        )}
      </div>
      {newJobButton}
    </div>
  )
}
