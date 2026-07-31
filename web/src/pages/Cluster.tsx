import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  ApiError,
  api,
  type FleetNode,
  type HealthStatus,
  type PlatformConfig,
  type RefreshOutcome,
  type SlotOccupant,
} from '../api'
import { useFleet } from '../useFleet'
import { fmtDuration, nodeStale, phaseToState, shortSha, shortVersion, versionHasSha } from '../format'
import { StateBadge } from '../components/StateBadge'
import { Skeleton } from '../components/Skeleton'
import { NodeCapacity } from '../components/NodeCapacity'

const POLL_MS = 2500

/**
 * The Cluster view (#139): a live graph of the little fleet — an api node, the
 * dispatcher, and a node per worker, wired api ↔ dispatcher ↔ workers. Each
 * worker carries a slot widget: one cell per slot, empty cells reading as free
 * capacity, occupied cells showing what that slot runs (job seq, task kind, job
 * type, a phase badge, a live running counter) and linking to the job. A pending
 * tray feeds the workers when the launch queue is non-empty.
 *
 * Read-only apart from one control: each worker card carries the capacity
 * stepper and its display states (design #293 §10, `NodeCapacity`) — the only
 * place an operator can change a node's slot count without ssh and a rebuild.
 */
export function ClusterPage() {
  const navigate = useNavigate()
  const [tick, setTick] = useState(0)
  const { fleet, unavailable, loaded } = useFleet({ tick, pollMs: POLL_MS })
  const [cfg, setCfg] = useState<PlatformConfig | null>(null)
  const [cfgLoaded, setCfgLoaded] = useState(false)
  const [health, setHealth] = useState<HealthStatus | null>(null)
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

  useEffect(() => {
    api.health().then(setHealth, () => setHealth(null))
  }, [])

  const loading = !loaded || !cfgLoaded

  const [now, setNow] = useState(() => Date.now())
  const anyRunning = (fleet?.nodes ?? []).some((n) => n.occupied > 0)
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), anyRunning ? 1000 : 5000)
    return () => clearInterval(id)
  }, [anyRunning])

  const workers: FleetNode[] = useMemo(() => {
    if (fleet && fleet.nodes.length > 0) return fleet.nodes
    const roster = cfg?.dispatcher?.nodes ?? []
    return roster.map((n) => ({
      name: n.name,
      slots: n.slots,
      occupied: 0,
      available: n.available,
      version: n.version ?? null,
      capacity_source: n.capacity_source ?? null,
      capacity_observed_at: n.capacity_observed_at ?? null,
      running: [],
    }))
  }, [fleet, cfg])

  const isWorkerEndpoint = useMemo(() => {
    const endpoints = new Map((cfg?.dispatcher?.nodes ?? []).map((n) => [n.name, n.endpoint]))
    return (name: string) => (endpoints.get(name) ?? 'worker') === 'worker'
  }, [cfg])

  const commonVersion = useMemo(() => {
    const counts = new Map<string, number>()
    for (const n of workers) if (n.version) counts.set(n.version, (counts.get(n.version) ?? 0) + 1)
    let best: string | null = null
    let bestN = 0
    for (const [v, c] of counts) if (c > bestN) [best, bestN] = [v, c]
    return counts.size > 1 ? best : null
  }, [workers])

  const deployedSha = cfg?.dispatcher?.dispatcher_sha ?? null
  const apiSha = health?.api_sha ?? null
  const webSha = __CHUG_WEB_SHA__ || null
  const commitsBehind = cfg?.dispatcher?.commits_behind ?? null
  const shaSkew = (sha: string | null) =>
    !!sha && !!deployedSha && shortSha(sha) !== shortSha(deployedSha)
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
      {!forbidden && !loading && <DriftBanner d={cfg?.dispatcher} />}
      {!forbidden && !loading && (
        <section className="card cluster-card">
          <div className="cluster-graph">
            <div className="cl-tier">
              <Node
                role="api"
                name="api"
                online
                sub="gateway"
                sha={apiSha}
                drift={shaSkew(apiSha)}
                title="build skew — the api is on a different commit than the dispatcher"
              />
              <Node
                role="web"
                name="web"
                online
                sub="ui"
                sha={webSha}
                drift={shaSkew(webSha)}
                title="build skew — the published web bundle is on a different commit than the dispatcher"
              />
            </div>
            <Edge />
            <div className="cl-tier cl-tier-dispatcher">
              <Node
                role="dispatcher"
                name="dispatcher"
                online={dispatcherOnline}
                sub="orchestrator"
                sha={deployedSha}
                drift={(commitsBehind ?? 0) > 0}
                title={
                  commitsBehind
                    ? `${commitsBehind} commit${commitsBehind === 1 ? '' : 's'} behind main`
                    : undefined
                }
              />
              {queue > 0 && <PendingTray count={queue} />}
            </div>
            <Edge />
            <div className="cl-fan">
              {workers.length === 0 && <div className="dim cl-empty">no worker nodes</div>}
              {workers.map((n) => (
                <WorkerNode
                  key={n.name}
                  node={n}
                  nodes={workers}
                  now={now}
                  drift={isDrift(n)}
                  controllable={isWorkerEndpoint(n.name)}
                  onChanged={() => setTick((t) => t + 1)}
                />
              ))}
            </div>
          </div>
          {staleCount > 0 && deployedSha && (
            <div className="cluster-note dim">
              ⚠ {staleCount} node{staleCount === 1 ? '' : 's'} behind the deployed platform (
              <code>{shortSha(deployedSha)}</code>) — marked.
            </div>
          )}
          {commonVersion && (
            <div className="cluster-note dim">
              ⚠ version drift across workers — nodes off{' '}
              <code title={commonVersion}>{shortVersion(commonVersion)}</code> are marked.
            </div>
          )}
        </section>
      )}
      {!forbidden && !loading && (
        <section className="card">
          <h2>Fleet freshness</h2>
          <p className="dim deploy-sub">
            Each worker&apos;s daemon build vs the deployed platform SHA
            {deployedSha ? (
              <>
                {' '}
                (<code>{shortSha(deployedSha)}</code>)
              </>
            ) : null}
            . A node behind the deployed platform is stale until its self-refresh lands.
          </p>
          {workers.length === 0 && <div className="dim">no worker nodes</div>}
          {workers.length > 0 && (
            <ul className="freshness-list">
              {workers.map((n) => (
                <FreshnessRow key={n.name} node={n} deployedSha={deployedSha} />
              ))}
            </ul>
          )}
        </section>
      )}
    </div>
  )
}

function DriftBanner({
  d,
}: {
  d: PlatformConfig['dispatcher'] | null | undefined
}) {
  const deployed = d?.dispatcher_sha ?? null
  const tip = d?.main_tip_sha ?? null
  const behind = d?.commits_behind ?? null
  const known = behind != null
  const current = behind === 0
  const tone = !known ? 'neutral' : current ? 'ok' : 'behind'

  return (
    <section className={`card drift-banner drift-${tone}`}>
      <div className="drift-head">
        <span className="drift-dot" aria-hidden="true" />
        <span className="drift-headline">
          {!known
            ? 'Deploy drift unavailable'
            : current
              ? 'Prod is up to date with main'
              : `Prod is ${behind} commit${behind === 1 ? '' : 's'} behind main`}
        </span>
      </div>
      <div className="drift-shas">
        <span>
          deployed{' '}
          {deployed ? <code>{shortSha(deployed)}</code> : <span className="dim">unknown</span>}
        </span>
        <span aria-hidden="true" className="dim">
          →
        </span>
        <span>
          main tip{' '}
          {tip ? <code>{shortSha(tip)}</code> : <span className="dim">unknown</span>}
        </span>
      </div>
    </section>
  )
}

function FreshnessRow({
  node,
  deployedSha,
}: {
  node: FleetNode
  deployedSha: string | null
}) {
  const stale = nodeStale(node.version, deployedSha)
  const fresh = versionHasSha(node.version, deployedSha)
  const chip = stale
    ? { cls: 'badge-red', text: 'stale' }
    : fresh
      ? { cls: 'badge-green', text: 'current' }
      : { cls: 'badge-gray', text: 'unknown' }
  return (
    <li className="freshness-row">
      <span className="freshness-name">{node.name}</span>
      <span className={`badge ${chip.cls}`}>{chip.text}</span>
      <code className="freshness-ver dim" title={node.version ?? undefined}>
        {node.version ? shortVersion(node.version) : '—'}
      </code>
      <RefreshChip outcome={node.refresh_outcome} />
      {!node.available && <span className="badge badge-orange">out of service</span>}
    </li>
  )
}

function RefreshChip({ outcome }: { outcome?: RefreshOutcome | null }) {
  if (!outcome) return null
  const r = outcome.result
  if (r.result === 'failed') {
    return (
      <span className="badge badge-red" title={r.error_tail}>
        refresh failed · {r.stage}
      </span>
    )
  }
  if (r.result === 'in_progress') {
    return <span className="badge badge-blue">refreshing…</span>
  }
  return null
}

function Edge() {
  return <div className="cl-edge" aria-hidden="true" />
}

function Node({
  role,
  name,
  online,
  sub,
  sha,
  drift,
  title,
}: {
  role: string
  name: string
  online: boolean
  sub?: string
  sha?: string | null
  drift?: boolean
  title?: string
}) {
  return (
    <div className={`cl-node cl-node-${role}${online ? '' : ' cl-dim'}${drift ? ' cl-drift' : ''}`}>
      <div className="cl-node-head">
        <span className={`cl-pulse${online ? '' : ' off'}`} aria-hidden="true" />
        <span className="cl-node-name">{name}</span>
      </div>
      <div className="cl-node-sub dim">
        {sha ? <code title={drift ? title : undefined}>{shortSha(sha)}</code> : sub}
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

function WorkerNode({
  node,
  nodes,
  now,
  drift,
  controllable,
  onChanged,
}: {
  node: FleetNode
  nodes: FleetNode[]
  now: number
  drift: boolean
  controllable: boolean
  onChanged: () => void
}) {
  const total = node.slots ?? node.running.length
  const cells = Array.from({ length: Math.max(total, node.running.length) }, (_, i) => node.running[i] ?? null)
  const overCap = node.slots != null && node.occupied > node.slots
  return (
    <div className={`cl-node cl-worker${node.available ? '' : ' cl-dim cl-out'}${drift ? ' cl-drift' : ''}`}>
      <div className="cl-node-head">
        <span className={`cl-pulse${node.available ? '' : ' off'}`} aria-hidden="true" />
        <span className="cl-node-name">{node.name}</span>
        <span
          className={`cl-node-frac dim${overCap ? ' cl-over' : ''}`}
          title={overCap ? 'over cap — draining: finishing what it holds, taking nothing new' : undefined}
        >
          {node.occupied}/{node.slots ?? '?'}
        </span>
      </div>
      <div className="cl-node-sub dim">
        {node.version ? (
          <code title={drift ? `${node.version} — version drift` : node.version}>{shortVersion(node.version)}</code>
        ) : (
          'worker'
        )}
        {!node.available && <span className="cl-out-tag"> out of service</span>}
      </div>
      <div className="cl-slots">
        {cells.map((s, i) => (
          <Slot
            key={s ? `${s.project}:${s.job_seq}:${s.task_id}` : `empty-${i}`}
            occ={s}
            now={now}
            over={node.slots != null && i >= node.slots}
          />
        ))}
      </div>
      <NodeCapacity
        node={node}
        nodes={nodes}
        now={now}
        controllable={controllable}
        onChanged={onChanged}
      />
    </div>
  )
}

function Slot({ occ, now, over }: { occ: SlotOccupant | null; now: number; over?: boolean }) {
  if (!occ) return <div className="cl-slot cl-slot-free" title="free" />
  const started = occ.started_at ? Date.parse(occ.started_at) : null
  const state = phaseToState(occ.phase)
  return (
    <Link
      className={`cl-slot cl-slot-busy${over ? ' cl-slot-over' : ''}`}
      to={`/p/${occ.project}/jobs/${occ.job_seq}`}
      title={`${occ.project} · #${occ.job_seq} · ${occ.job_type} · ${occ.task_kind}${
        over ? ' · over cap — draining' : ''
      }`}
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
