import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type Job } from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { SkeletonLines } from '../components/Skeleton'
import { ActivityChart } from '../components/ActivityChart'
import { fmtDuration } from '../format'
import { activitySeries, countStates, OVERVIEW_GROUPS } from '../jobFilters'

/**
 * Stats page: the project's numbers at a glance. A full-width Jobs overview —
 * total, per-state counts deep-linking into the filtered jobs table, and the
 * daily created/completed activity chart — then a full-width per-type table
 * with completion counts and average wall-clock duration (release → done).
 * Everything derives from the one jobs fetch; per-task metrics (task time,
 * tokens, reworks, escalations) need a server-side aggregate first.
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

  // Per-type rollup: counts plus mean wall-clock duration of Done jobs
  // (ready_at → completed_at; created_at stands in for pre-release records).
  const byType = new Map<string, { total: number; done: number; durSum: number; durN: number }>()
  for (const j of jobs) {
    const t = byType.get(j.type) ?? { total: 0, done: 0, durSum: 0, durN: 0 }
    t.total += 1
    if (j.state === 'Done') {
      t.done += 1
      const start = j.ready_at ?? j.created_at
      if (j.completed_at && start) {
        const ms = Date.parse(j.completed_at) - Date.parse(start)
        if (Number.isFinite(ms) && ms >= 0) {
          t.durSum += ms
          t.durN += 1
        }
      }
    }
    byType.set(j.type, t)
  }
  const series = activitySeries(jobs)

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      {loading && !error ? (
        <>
          <section className="card">
            <h2>Jobs overview</h2>
            <SkeletonLines n={6} />
          </section>
          <section className="card">
            <h2>By type</h2>
            <SkeletonLines n={4} />
          </section>
        </>
      ) : (
        <>
          <section className="card">
            <h2>Jobs overview</h2>
            <div className="ov-layout">
              <div className="ov-side">
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
              </div>
              <div className="ov-chart">
                <ActivityChart
                  starts={series.starts}
                  created={series.created}
                  completed={series.completed}
                />
              </div>
            </div>
          </section>

          <section className="card">
            <h2>By type</h2>
            <div className="table-scroll">
              <table className="type-stats">
                <thead>
                  <tr>
                    <th>Type</th>
                    <th className="num">Done</th>
                    <th className="num">Total</th>
                    <th className="num" title="mean release → done wall-clock time of Done jobs">
                      Avg duration
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {[...byType.entries()]
                    .sort((a, b) => b[1].total - a[1].total)
                    .map(([type, t]) => (
                      <tr key={type}>
                        <td>
                          <Link to={`/p/${owner}/${project}/job-types/${encodeURIComponent(type)}`}>
                            {type}
                          </Link>
                        </td>
                        <td className="num">{t.done}</td>
                        <td className="num">{t.total}</td>
                        <td className="num">{t.durN ? fmtDuration(t.durSum / t.durN) : '—'}</td>
                      </tr>
                    ))}
                </tbody>
              </table>
            </div>
          </section>
        </>
      )}
    </div>
  )
}
