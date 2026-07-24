import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ApiError, api, type FleetNode, type PlatformConfig, type SlotOccupant } from '../api'
import { useFleet } from '../useFleet'
import { fmtDuration, nodeStale, phaseToState, shortSha } from '../format'
import { StateBadge } from '../components/StateBadge'
import { Skeleton } from '../components/Skeleton'

// Poll cadence for the occupancy snapshot. The top-level cluster view has no
// per-project SSE to ride, so it polls; the running counters tick every second
// regardless (below).
const POLL_MS = 2500

/**
 * The Cluster view (#139): a live graph of the little fleet — an api node, the
 * dispatcher, and a node per worker, wired api ↔ dispatcher ↔ workers. Each
 * worker carries a slot widget: one cell per slot, empty cells reading as free
 * capacity, occupied cells showing what that slot runs (job seq, task kind, job
 * type, a phase badge, a live running counter) and linking to the job. A pending
 * tray feeds the workers when the launch queue is non-empty. Display only.
 */
export function ClusterPage() {
  const navigate = useNavigate()
  const { fleet, unavailable, loaded } = useFleet({ pollMs: POLL_MS })
  const [cfg, setCfg] = useState<PlatformConfig | null>(null)
  const [cfgLoaded, setCfgLoaded] = useState(false)
  const [forbidden, setForbidden] = useState(false)

  useEffect(() => {
    api.platformConfig().then(
      (c) => {
        setCfg(c)
        setCfgLoaded(true)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else if (e instanceof ApiError && e.status === 403) setForbidden(true)
        setCfgLoaded(true)
      },
    )
  }, [navigate])

  // Initial load only: skeleton until both the occupancy snapshot and the
  // config roster have answered once. Poll refreshes never re-skeleton.
  const loading = !loaded || !cfgLoaded

  // A 1s clock so the running-duration counters advance while slots are busy.
  const [now, setNow] = useState(() => Date.now())
  const anyRunning = (fleet?.nodes ?? []).some((n) => n.occupied > 0)
  useEffect(() => {
    if (!anyRunning) return
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [anyRunning])

  // Worker nodes: the live occupancy feed is the source of truth; before the
  // dispatcher has published anything, fall back to the static configured roster
  // rendered idle so the graph still draws.
  const workers: FleetNode[] = useMemo(() => {
    if (fleet && fleet.nodes.length > 0) return fleet.nodes
    const roster = cfg?.dispatcher?.nodes ?? []
    return roster.map((n) => ({
      name: n.name,
      slots: n.slots,
      occupied: 0,
      available: n.available,
      version: n.version ?? null,
      running: [],
    }))
  }, [fleet, cfg])

  // Version drift: if the workers report more than one distinct build, flag the
  // ones off the most common version with a subtle hint.
  const commonVersion = useMemo(() => {
    const counts = new Map<string, number>()
    for (const n of workers) if (n.version) counts.set(n.version, (counts.get(n.version) ?? 0) + 1)
    let best: string | null = null
    let bestN = 0
    for (const [v, c] of counts) if (c > bestN) [best, bestN] = [v, c]
    return counts.size > 1 ? best : null
  }, [workers])

  // Deploy freshness: the deployed platform SHA the fleet is measured against.
  // A node drifts if it's behind that SHA (stale vs main) OR simply disagrees
  // with its peers — a uniformly-stale fleet (every node behind the deployed
  // platform) is invisible to peer comparison alone, which the audit flags.
  const deployedSha = cfg?.dispatcher?.dispatcher_sha ?? null
  const isDrift = (n: FleetNode) =>
    nodeStale(n.version, deployedSha) ||
    (commonVersion != null && n.version != null && n.version !== commonVersion)
  const staleCount = workers.filter((n) => nodeStale(n.version, deployedSha)).length

  const dispatcherOnline = cfg ? cfg.dispatcher != null : true
  const queue = fleet?.queue_depth ?? 0

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>Cluster</h1>
      </header>

      {forbidden && (
        <section className="card">
          <div className="dim">Platform admin required.</div>
        </section>
      )}
      {!forbidden && unavailable && loaded && (
        <section className="card">
          <div className="dim">Fleet feed unavailable — the dispatcher may be offline.</div>
        </section>
      )}

      {!forbidden && loading && (
        <section className="card cluster-card">
          <div className="cluster-graph">
            <div className="cl-tier">
              <Skeleton width="8rem" height="3.2rem" />
            </div>
            <Edge />
            <div className="cl-tier">
              <Skeleton width="8rem" height="3.2rem" />
            </div>
            <Edge />
            <div className="cl-fan">
              <Skeleton width="12rem" height="5rem" />
              <Skeleton width="12rem" height="5rem" />
            </div>
          </div>
        </section>
      )}
      {!forbidden && !loading && (
        <section className="card cluster-card">
          <div className="cluster-graph">
            <div className="cl-tier">
              <Node role="api" name="api" online sub="gateway" />
            </div>
            <Edge />
            <div className="cl-tier cl-tier-dispatcher">
              <Node role="dispatcher" name="dispatcher" online={dispatcherOnline} sub="orchestrator" />
              {queue > 0 && <PendingTray count={queue} />}
            </div>
            <Edge />
            <div className="cl-fan">
              {workers.length === 0 && <div className="dim cl-empty">no worker nodes</div>}
              {workers.map((n) => (
                <WorkerNode key={n.name} node={n} now={now} drift={isDrift(n)} />
              ))}
            </div>
          </div>
          {staleCount > 0 && deployedSha && (
            <div className="cluster-note dim">
              ⚠ {staleCount} node{staleCount === 1 ? '' : 's'} behind the deployed platform (
              <code>{shortSha(deployedSha)}</code>) — marked. <Link to="/deploys">Deploys ↗</Link>
            </div>
          )}
          {commonVersion && (
            <div className="cluster-note dim">
              ⚠ version drift across workers — nodes off <code>{commonVersion}</code> are marked.
            </div>
          )}
        </section>
      )}
    </div>
  )
}

function Edge() {
  return <div className="cl-edge" aria-hidden="true" />
}

function Node({
  role,
  name,
  online,
  sub,
  version,
  drift,
}: {
  role: string
  name: string
  online: boolean
  sub?: string
  version?: string | null
  drift?: boolean
}) {
  return (
    <div className={`cl-node cl-node-${role}${online ? '' : ' cl-dim'}${drift ? ' cl-drift' : ''}`}>
      <div className="cl-node-head">
        <span className={`cl-pulse${online ? '' : ' off'}`} aria-hidden="true" />
        <span className="cl-node-name">{name}</span>
      </div>
      <div className="cl-node-sub dim">
        {version ? <code title={drift ? 'version drift' : undefined}>{version}</code> : sub}
      </div>
    </div>
  )
}

function PendingTray({ count }: { count: number }) {
  return (
    <div className="cl-tray" title={`${count} launch${count === 1 ? '' : 'es'} queued for a free slot`}>
      <span className="cl-tray-label dim">queued</span>
      <span className="cl-tray-count">{count}</span>
    </div>
  )
}

function WorkerNode({ node, now, drift }: { node: FleetNode; now: number; drift: boolean }) {
  const total = node.slots ?? node.running.length
  const cells = Array.from({ length: Math.max(total, node.running.length) }, (_, i) => node.running[i] ?? null)
  return (
    <div className={`cl-node cl-worker${node.available ? '' : ' cl-dim cl-out'}${drift ? ' cl-drift' : ''}`}>
      <div className="cl-node-head">
        <span className={`cl-pulse${node.available ? '' : ' off'}`} aria-hidden="true" />
        <span className="cl-node-name">{node.name}</span>
        <span className="cl-node-frac dim">
          {node.occupied}/{node.slots ?? '?'}
        </span>
      </div>
      <div className="cl-node-sub dim">
        {node.version ? <code title={drift ? 'version drift' : undefined}>{node.version}</code> : 'worker'}
        {!node.available && <span className="cl-out-tag"> out of service</span>}
      </div>
      <div className="cl-slots">
        {cells.map((s, i) => (
          <Slot key={s ? `${s.project}:${s.job_seq}:${s.task_id}` : `empty-${i}`} occ={s} now={now} />
        ))}
      </div>
    </div>
  )
}

function Slot({ occ, now }: { occ: SlotOccupant | null; now: number }) {
  if (!occ) return <div className="cl-slot cl-slot-free" title="free" />
  const started = occ.started_at ? Date.parse(occ.started_at) : null
  const state = phaseToState(occ.phase)
  return (
    <Link
      className="cl-slot cl-slot-busy"
      to={`/p/${occ.project}/jobs/${occ.job_seq}`}
      title={`${occ.project} · #${occ.job_seq} · ${occ.job_type} · ${occ.task_kind}`}
    >
      <div className="cl-slot-fill">
        <div className="cl-slot-top">
          <span className="cl-slot-seq">#{occ.job_seq}</span>
          {state ? (
            <StateBadge state={state} />
          ) : (
            <span className="badge badge-gray">{occ.phase}</span>
          )}
        </div>
        <div className="cl-slot-meta">
          <span className="cl-slot-kind">{occ.task_kind}</span>
          <span className="cl-slot-type dim">{occ.job_type}</span>
        </div>
        {started != null && <span className="cl-slot-dur">{fmtDuration(now - started)}</span>}
      </div>
    </Link>
  )
}
