import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  ApiError,
  api,
  type FleetNode,
  type PlatformConfig,
  type RefreshOutcome,
} from '../api'
import { useFleet } from '../useFleet'
import { nodeStale, shortSha, versionHasSha } from '../format'
import { Skeleton, SkeletonLines } from '../components/Skeleton'

// Same cadence as Cluster: no per-project SSE to ride at the platform level, so
// the freshness view polls the fleet snapshot.
const POLL_MS = 2500

/**
 * The Deploys view: prod-vs-main drift and fleet freshness as first-class
 * platform state (2026-07-23 deploy audit). The #109 drift fields
 * (`dispatcher_sha` / `main_tip_sha` / `commits_behind` / `auto_deploy`) and the
 * #187 per-node refresh outcomes are computed and served by the dispatcher but
 * were rendered nowhere; this page renders them.
 *
 * The per-deploy *leg report* (from→to SHA, per-leg ok/failed/skipped chips,
 * rollback badge) renders in each deploy job's detail — see JobDetail's
 * DeployLegCard. A cross-project deploy *history* roll-up here needs the api to
 * expose which project holds the platform's deploy jobs (the self-repo slug is
 * not in any served snapshot today); that is a follow-up `code` job.
 */
export function DeploysPage() {
  const navigate = useNavigate()
  const { fleet, loaded } = useFleet({ pollMs: POLL_MS })
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

  const loading = !loaded || !cfgLoaded
  const d = cfg?.dispatcher
  const deployedSha = d?.dispatcher_sha ?? null

  // Fleet nodes: the live occupancy feed is the source of truth; before the
  // dispatcher publishes it, fall back to the static configured roster so the
  // freshness table still draws (mirrors Cluster).
  const nodes: FleetNode[] = useMemo(() => {
    if (fleet && fleet.nodes.length > 0) return fleet.nodes
    const roster = d?.nodes ?? []
    return roster.map((n) => ({
      name: n.name,
      slots: n.slots,
      occupied: 0,
      available: n.available,
      version: n.version ?? null,
      refresh_outcome: n.refresh_outcome ?? null,
      running: [],
    }))
  }, [fleet, d])

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>Deploys</h1>
        <nav className="topbar-nav">
          <Link to="/cluster">Cluster</Link>
          <Link to="/settings">Settings</Link>
        </nav>
      </header>

      {forbidden && (
        <section className="card">
          <div className="dim">Platform admin required.</div>
        </section>
      )}

      {!forbidden && loading && (
        <section className="card">
          <SkeletonLines n={2} />
          <Skeleton width="100%" height="4rem" />
        </section>
      )}

      {!forbidden && !loading && (
        <>
          <DriftBanner d={d} />
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
              . A node behind the deployed platform is stale until its self-refresh
              lands.
            </p>
            {nodes.length === 0 && <div className="dim">no worker nodes</div>}
            {nodes.length > 0 && (
              <ul className="freshness-list">
                {nodes.map((n) => (
                  <FreshnessRow key={n.name} node={n} deployedSha={deployedSha} />
                ))}
              </ul>
            )}
          </section>
          <p className="dim deploy-foot">
            A deploy job&apos;s per-leg checklist (from→to SHA, ok/failed/skipped
            legs, rollback) renders in that job&apos;s detail page.
          </p>
        </>
      )}
    </div>
  )
}

// prod-vs-main drift: deployed SHA, main tip, commits-behind, and the auto-deploy
// posture. Green when in sync, amber when prod trails main, neutral when the
// dispatcher hasn't reported the CD fields (a local/dev build, or an older
// snapshot).
function DriftBanner({
  d,
}: {
  d: PlatformConfig['dispatcher'] | null | undefined
}) {
  const deployed = d?.dispatcher_sha ?? null
  const tip = d?.main_tip_sha ?? null
  const behind = d?.commits_behind ?? null
  const auto = d?.auto_deploy
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
        {auto != null && (
          <span
            className={`badge ${auto ? 'badge-blue' : 'badge-gray'} drift-auto`}
            title={auto ? 'deploys land automatically' : 'deploys are manual'}
          >
            auto-deploy {auto ? 'on' : 'off'}
          </span>
        )}
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

// One worker's freshness: fresh (green) when its build carries the deployed SHA,
// stale (red) when it demonstrably does not, unknown (grey) when either side is
// unreported. Any refresh outcome (a failed or in-flight self-refresh) rides
// alongside so a wedged refresh is visible, not just a stale version.
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
      <code className="freshness-ver dim">{node.version ?? '—'}</code>
      <RefreshChip outcome={node.refresh_outcome} />
      {!node.available && <span className="badge badge-orange">out of service</span>}
    </li>
  )
}

// The node's last self-refresh outcome, when it carries signal: a failed refresh
// (red, naming the stage) or one still in flight. A clean `ok` is left implicit —
// the version chip already shows the node landed the build.
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
