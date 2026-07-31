import { useCallback, useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type DeployReport, type Job } from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { StateBadge } from '../components/StateBadge'
import { SkeletonLines } from '../components/Skeleton'
import { DeployLegCard, deployReportOfTasks } from '../components/DeployLegCard'

const PAGE = 20

/**
 * The per-project Deploys view: this project's deploy history. Deploys are
 * per-project state changes, so the page lists the project's jobs of type
 * `deploy`, newest first, each with the structured leg report harvested from its
 * command work task (from→to SHA, per-leg ok/failed/skipped, rollback). Each row
 * links to the deploy job's detail. Platform-wide drift and fleet freshness live
 * on Cluster, not here.
 */
export function DeploysPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const scope = `${owner}/${project}`
  const [loaded, setLoaded] = useState<{ scope: string; deploys: Job[] }>({ scope: '', deploys: [] })
  const deploys = loaded.scope === scope ? loaded.deploys : []
  const [reports, setReports] = useState<Map<number, DeployReport | null>>(new Map())
  const requested = useRef<Set<number>>(new Set())
  const reportScope = useRef(scope)
  const [shown, setShown] = useState(PAGE)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setShown(PAGE)
    setReports(new Map())
    requested.current = new Set()
    reportScope.current = scope
    api.jobs(owner, project).then(
      (jobs) => {
        if (cancelled) return
        setLoaded({ scope, deploys: jobs.filter((j) => j.type === 'deploy').sort((a, b) => b.id - a.id) })
        setError(null)
        setLoading(false)
      },
      (e) => {
        if (cancelled) return
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
        setLoading(false)
      },
    )
    return () => {
      cancelled = true
    }
  }, [owner, project, scope, navigate])

  const settleReport = useCallback(
    (id: number, report: DeployReport | null) => {
      if (reportScope.current !== scope) return
      setReports((prev) => new Map(prev).set(id, report))
    },
    [scope],
  )

  useEffect(() => {
    const todo = deploys.slice(0, shown).filter((j) => !requested.current.has(j.id))
    if (todo.length === 0) return
    for (const j of todo) requested.current.add(j.id)
    for (const j of todo) {
      void (async () => {
        let report: DeployReport | null = null
        try {
          report = deployReportOfTasks(await api.tasks(owner, project, j.id))
        } catch {
          report = null
        }
        settleReport(j.id, report)
      })()
    }
  }, [owner, project, deploys, shown, settleReport])

  const visible = deploys.slice(0, shown)

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      {loading && !error && (
        <section className="card">
          <SkeletonLines n={4} />
        </section>
      )}
      {!loading && !error && visible.length === 0 && (
        <section className="card">
          <div className="dim">No deploy jobs for this project yet.</div>
        </section>
      )}
      {!loading && !error && visible.length > 0 && (
        <>
          <ul className="deploy-history">
            {visible.map((job) => (
              <li key={job.id} className="card deploy-entry">
                <Link className="deploy-entry-head" to={`/p/${owner}/${project}/jobs/${job.id}`}>
                  <span className="deploy-entry-seq">#{job.id}</span>
                  <span className="deploy-entry-title">{job.title || '(untitled)'}</span>
                  <StateBadge state={job.state} />
                  <span className="deploy-entry-when dim">{fmtWhen(job.created_at)}</span>
                </Link>
                <DeployEntryReport
                  loaded={reports.has(job.id)}
                  report={reports.get(job.id) ?? null}
                />
              </li>
            ))}
          </ul>
          {deploys.length > visible.length && (
            <div className="deploy-more">
              <button type="button" onClick={() => setShown((n) => n + PAGE)}>
                show older deploys <span className="dim">({deploys.length - visible.length} more)</span>
              </button>
            </div>
          )}
        </>
      )}
    </div>
  )
}

function DeployEntryReport({ loaded, report }: { loaded: boolean; report: DeployReport | null }) {
  if (!loaded) return <SkeletonLines n={2} />
  if (!report) return <div className="dim deploy-entry-noreport">no leg report</div>
  return <DeployLegCard report={report} />
}

function fmtWhen(iso: string): string {
  const d = new Date(iso)
  const now = new Date()
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate()
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
  return sameDay ? time : `${d.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${time}`
}
