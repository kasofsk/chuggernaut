import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type DeployReport, type Job } from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { StateBadge } from '../components/StateBadge'
import { SkeletonLines } from '../components/Skeleton'
import { DeployLegCard, deployReportOfTasks } from '../components/DeployLegCard'

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
  const [rows, setRows] = useState<{ job: Job; report: DeployReport | null }[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    api.jobs(owner, project).then(
      async (jobs) => {
        // Newest first by seq; the leg report for each rides its tasks (the
        // command work task carries the structured DeployReport).
        const deploys = jobs.filter((j) => j.type === 'deploy').sort((a, b) => b.id - a.id)
        const withReports = await Promise.all(
          deploys.map(async (job) => {
            try {
              const tasks = await api.tasks(owner, project, job.id)
              return { job, report: deployReportOfTasks(tasks) }
            } catch {
              return { job, report: null }
            }
          }),
        )
        if (cancelled) return
        setRows(withReports)
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
  }, [owner, project, navigate])

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      {loading && !error && (
        <section className="card">
          <SkeletonLines n={4} />
        </section>
      )}
      {!loading && !error && rows.length === 0 && (
        <section className="card">
          <div className="dim">No deploy jobs for this project yet.</div>
        </section>
      )}
      {!loading && !error && rows.length > 0 && (
        <ul className="deploy-history">
          {rows.map(({ job, report }) => (
            <li key={job.id} className="card deploy-entry">
              <Link className="deploy-entry-head" to={`/p/${owner}/${project}/jobs/${job.id}`}>
                <span className="deploy-entry-seq">#{job.id}</span>
                <span className="deploy-entry-title">{job.title || '(untitled)'}</span>
                <StateBadge state={job.state} />
                <span className="deploy-entry-when dim">{fmtWhen(job.created_at)}</span>
              </Link>
              {report ? (
                <DeployLegCard report={report} />
              ) : (
                <div className="dim deploy-entry-noreport">no leg report</div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

// Compact local timestamp: time-only when the moment is today, date prepended
// otherwise (mirrors the jobs table's convention).
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
