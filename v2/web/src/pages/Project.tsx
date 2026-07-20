import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, loginPath, type Job, type Task } from '../api'
import { useProjectEvents } from '../useEvents'
import { StateBadge } from '../components/StateBadge'
import { InboxList } from '../components/InboxList'
import { ProjectTabs } from '../components/ProjectTabs'

export function ProjectPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [pending, setPending] = useState<Task[]>([])
  const [error, setError] = useState<string | null>(null)

  // Phone-first default (Part 11): on small viewports the inbox is the
  // landing surface. Redirect once per project per session so the Jobs tab
  // stays reachable by an explicit tap.
  useEffect(() => {
    const key = `inbox-landed:${owner}/${project}`
    if (
      window.matchMedia('(max-width: 640px)').matches &&
      !sessionStorage.getItem(key)
    ) {
      sessionStorage.setItem(key, '1')
      navigate(`/p/${owner}/${project}/inbox`, { replace: true })
    }
  }, [owner, project, navigate])

  const refresh = useCallback(() => {
    Promise.all([api.jobs(owner, project), api.pendingTasks(owner, project)])
      .then(([js, ts]) => {
        setJobs(js)
        setPending(ts)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate(loginPath())
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, navigate])

  useEffect(refresh, [refresh])
  // The SSE stream is the source of truth (Part 11): any event → refetch.
  useProjectEvents(owner, project, refresh)

  async function act(fn: () => Promise<unknown>) {
    try {
      await fn()
      refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'action failed')
    }
  }

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>
          {owner}/{project}
        </h1>
      </header>
      <ProjectTabs owner={owner} project={project} inboxCount={pending.length} />
      {error && <div className="error banner">{error}</div>}

      {pending.length > 0 && (
        <section className="card inbox">
          <h2>Inbox — {pending.length} pending</h2>
          <InboxList
            owner={owner}
            project={project}
            jobs={jobs}
            pending={pending}
            onResolve={(t, r) => act(() => api.resolve(owner, project, t.job_seq, t.id, r))}
          />
        </section>
      )}

      <section className="card">
        <div className="row-head">
          <h2>Jobs</h2>
          <Link to={`/p/${owner}/${project}/jobs/new`}>
            <button>new job</button>
          </Link>
        </div>
        <table className="jobs">
          <thead>
            <tr>
              <th>#</th>
              <th>title</th>
              <th>type</th>
              <th>state</th>
              <th>deps</th>
              <th>created</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {jobs.map((j) => (
              <tr key={j.id}>
                <td>
                  <Link to={`/p/${owner}/${project}/jobs/${j.id}`}>{j.id}</Link>
                </td>
                <td>
                  <Link to={`/p/${owner}/${project}/jobs/${j.id}`}>
                    {j.title || <span className="dim">—</span>}
                  </Link>
                </td>
                <td>
                  <Link className="dim" to={`/p/${owner}/${project}/job-types/${encodeURIComponent(j.type)}`}>
                    {j.type}
                  </Link>
                </td>
                <td>
                  <StateBadge state={j.state} />
                </td>
                <td className="dim">
                  {j.deps.map((d) => `#${d}`).join(', ')}
                </td>
                <td className="dim">{new Date(j.created_at).toLocaleString()}</td>
                <td className="actions">
                  {j.state === 'Frozen' && (
                    <button
                      title="hand the job to the dispatcher: work → evaluation → wrap-up"
                      onClick={() => act(() => api.release(owner, project, j.id))}
                    >
                      ▶ run
                    </button>
                  )}
                  {j.state !== 'Done' && j.state !== 'Revoked' && (
                    <button
                      className="danger"
                      onClick={() => act(() => api.revoke(owner, project, j.id))}
                    >
                      revoke
                    </button>
                  )}
                </td>
              </tr>
            ))}
            {jobs.length === 0 && (
              <tr>
                <td colSpan={7} className="dim">
                  no jobs yet
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </section>
    </div>
  )
}
