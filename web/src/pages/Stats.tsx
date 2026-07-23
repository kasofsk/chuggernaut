import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type Job } from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { SkeletonLines } from '../components/Skeleton'
import { Sparkline } from '../components/Sparkline'
import { activityBuckets, countStates, OVERVIEW_GROUPS } from '../jobFilters'

/**
 * Stats page: the project's numbers at a glance. Holds the Jobs overview
 * (moved off the jobs page so the table keeps the full width) — total,
 * activity sparkline, and per-state counts deep-linking into the filtered
 * jobs table — plus a per-type breakdown from the same fetch.
 */
export function StatsPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setLoading(true) // param change: back to skeletons until the new fetch lands
    api.jobs(owner, project).then(
      (js) => {
        setJobs(js)
        setError(null)
        setLoading(false)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
        setLoading(false)
      },
    )
  }, [owner, project, navigate])

  const byType = new Map<string, { total: number; done: number }>()
  for (const j of jobs) {
    const t = byType.get(j.type) ?? { total: 0, done: 0 }
    t.total += 1
    if (j.state === 'Done') t.done += 1
    byType.set(j.type, t)
  }

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      {loading && !error ? (
        <div className="stats-grid">
          <section className="card side-card">
            <h2>Jobs overview</h2>
            <SkeletonLines n={6} />
          </section>
          <section className="card side-card">
            <h2>By type</h2>
            <SkeletonLines n={4} />
          </section>
        </div>
      ) : (
      <div className="stats-grid">
        <section className="card side-card">
          <h2>Jobs overview</h2>
          <div className="ov-spark">
            <Sparkline data={activityBuckets(jobs)} width={280} height={56} />
          </div>
          <div className="ov-total">
            <span className="ov-total-n">{jobs.length}</span>
            <span className="ov-total-label">Total jobs</span>
          </div>
          <ul className="ov-states">
            {OVERVIEW_GROUPS.map((g) => (
              <li key={g.key}>
                <Link
                  className="ov-state"
                  to={`/p/${owner}/${project}?state=${g.states.join(',')}${
                    g.states.every((s) => s === 'Done' || s === 'Revoked') ? '&finished=1' : ''
                  }`}
                  title={`open jobs filtered to ${g.label}`}
                >
                  <span className={`state-dot dot-${g.color}`} />
                  <span className="ov-state-label">{g.label}</span>
                  <span className="ov-state-n">{countStates(jobs, g.states)}</span>
                </Link>
              </li>
            ))}
          </ul>
        </section>

        <section className="card side-card">
          <h2>By type</h2>
          <ul className="ov-states">
            {[...byType.entries()]
              .sort((a, b) => b[1].total - a[1].total)
              .map(([type, t]) => (
                <li key={type}>
                  <Link
                    className="ov-state"
                    to={`/p/${owner}/${project}/job-types/${encodeURIComponent(type)}`}
                    title={`job type ${type}`}
                  >
                    <span className="ov-state-label">{type}</span>
                    <span className="ov-state-n">
                      {t.done}/{t.total}
                    </span>
                  </Link>
                </li>
              ))}
          </ul>
        </section>
      </div>
      )}
    </div>
  )
}
