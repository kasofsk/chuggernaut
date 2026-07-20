import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, loginPath, type Job, type Task } from '../api'
import { useProjectEvents } from '../useEvents'
import { InboxList } from '../components/InboxList'
import { ProjectTabs } from '../components/ProjectTabs'

// The Inbox tab (Part 11): every pending Human task in the project, as a
// full page. On phones this is the landing surface — operating the fleet is
// approve/steer work, and this is where that work lives.
export function InboxPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [pending, setPending] = useState<Task[]>([])
  const [error, setError] = useState<string | null>(null)

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
  useProjectEvents(owner, project, refresh)

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

      <section className="card inbox">
        <h2>Inbox{pending.length > 0 ? ` — ${pending.length} pending` : ''}</h2>
        {pending.length === 0 ? (
          <p className="dim">Nothing needs you. Agents are on it.</p>
        ) : (
          <InboxList
            owner={owner}
            project={project}
            jobs={jobs}
            pending={pending}
            onResolve={(t, r) =>
              api
                .resolve(owner, project, t.job_seq, t.id, r)
                .then(refresh, (e) => setError(e instanceof Error ? e.message : 'action failed'))
            }
          />
        )}
      </section>
    </div>
  )
}
