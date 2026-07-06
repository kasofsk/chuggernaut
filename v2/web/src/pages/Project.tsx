import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type Job, type Task } from '../api'
import { useProjectEvents } from '../useEvents'
import { StateBadge } from '../components/StateBadge'
import { ResolveForm } from '../components/ResolveForm'

export function ProjectPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [pending, setPending] = useState<Task[]>([])
  const [error, setError] = useState<string | null>(null)
  const [showCreate, setShowCreate] = useState(false)

  const refresh = useCallback(() => {
    Promise.all([api.jobs(owner, project), api.pendingTasks(owner, project)])
      .then(([js, ts]) => {
        setJobs(js)
        setPending(ts)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, navigate])

  useEffect(refresh, [refresh])
  // The SSE stream is the source of truth (Part 11): any event → refetch.
  useProjectEvents(owner, project, refresh)

  const jobBySeq = new Map(jobs.map((j) => [j.id, j]))

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
      {error && <div className="error banner">{error}</div>}

      {pending.length > 0 && (
        <section className="card inbox">
          <h2>Inbox — {pending.length} pending</h2>
          {pending.map((t) => (
            <div className="inbox-task" key={`${t.job_seq}:${t.id}`}>
              <div className="inbox-head">
                <Link to={`/p/${owner}/${project}/jobs/${t.job_seq}`}>
                  job/{t.job_seq}
                </Link>
                <span className="dim">
                  {' '}
                  · task {t.id} · {t.phase}
                  {t.evaluator ? ` · ${t.evaluator}` : ''}
                </span>
              </div>
              {t.kind.kind === 'Human' && <pre className="prompt">{t.kind.prompt}</pre>}
              <ResolveForm
                escalation={jobBySeq.get(t.job_seq)?.state === 'Escalated'}
                onResolve={(r) => act(() => api.resolve(owner, project, t.job_seq, t.id, r))}
              />
            </div>
          ))}
        </section>
      )}

      <section className="card">
        <div className="row-head">
          <h2>Jobs</h2>
          <button onClick={() => setShowCreate(!showCreate)}>
            {showCreate ? 'cancel' : 'new job'}
          </button>
        </div>
        {showCreate && (
          <CreateJob
            onCreate={(type, inputs) =>
              act(async () => {
                await api.createJob(owner, project, { type, inputs })
                setShowCreate(false)
              })
            }
          />
        )}
        <table className="jobs">
          <thead>
            <tr>
              <th>#</th>
              <th>type</th>
              <th>state</th>
              <th>inputs</th>
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
                <td>{j.type}</td>
                <td>
                  <StateBadge state={j.state} />
                </td>
                <td className="dim">
                  {Object.entries(j.inputs)
                    .map(([k, v]) => `${k}←${v}`)
                    .join(', ')}
                </td>
                <td className="dim">{new Date(j.created_at).toLocaleString()}</td>
                <td className="actions">
                  {j.state === 'Frozen' && (
                    <button onClick={() => act(() => api.release(owner, project, j.id))}>
                      release
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
                <td colSpan={6} className="dim">
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

function CreateJob({
  onCreate,
}: {
  onCreate: (type: string, inputs: Record<string, number>) => void
}) {
  const [type, setType] = useState('')
  const [inputs, setInputs] = useState('{}')
  const [parseError, setParseError] = useState<string | null>(null)

  return (
    <form
      className="create-job"
      onSubmit={(e) => {
        e.preventDefault()
        try {
          const parsed = JSON.parse(inputs || '{}')
          setParseError(null)
          onCreate(type, parsed)
        } catch {
          setParseError('inputs must be JSON like {"spec": 1}')
        }
      }}
    >
      <input
        placeholder="job type (jobs/{type}.yaml)"
        value={type}
        onChange={(e) => setType(e.target.value)}
        required
      />
      <input
        placeholder='inputs, e.g. {"spec": 1}'
        value={inputs}
        onChange={(e) => setInputs(e.target.value)}
      />
      <button type="submit">create</button>
      {parseError && <div className="error">{parseError}</div>}
    </form>
  )
}
