import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type DiffResponse, type Job, type Task } from '../api'
import { useProjectEvents, type JobEvent } from '../useEvents'
import { StateBadge, TaskBadge } from '../components/StateBadge'
import { ResolveForm } from '../components/ResolveForm'

export function JobDetail() {
  const { owner = '', project = '', seq = '' } = useParams()
  const jobSeq = Number(seq)
  const navigate = useNavigate()
  const [job, setJob] = useState<Job | null>(null)
  const [tasks, setTasks] = useState<Task[]>([])
  const [diff, setDiff] = useState<DiffResponse | null>(null)
  const [events, setEvents] = useState<JobEvent[]>([])
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(() => {
    Promise.all([
      api.job(owner, project, jobSeq),
      api.tasks(owner, project, jobSeq),
      api.diff(owner, project, jobSeq),
    ])
      .then(([j, ts, d]) => {
        setJob(j)
        setTasks(ts)
        setDiff(d)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, jobSeq, navigate])

  useEffect(refresh, [refresh])
  useProjectEvents(
    owner,
    project,
    (e) => {
      setEvents((prev) => [...prev.slice(-199), e])
      refresh()
    },
    jobSeq,
  )

  if (!job) {
    return (
      <div className="page">
        {error ? <div className="error banner">{error}</div> : 'loading…'}
      </div>
    )
  }

  const pendingHuman = tasks.filter(
    (t) => t.kind.kind === 'Human' && t.state === 'Pending',
  )

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <Link to={`/p/${owner}/${project}`}>
          {owner}/{project}
        </Link>
        <h1>
          job/{job.id} <StateBadge state={job.state} />
        </h1>
      </header>
      {error && <div className="error banner">{error}</div>}

      <section className="card">
        <h2>{job.type}</h2>
        <dl className="meta">
          <dt>branch</dt>
          <dd>{job.branch}</dd>
          <dt>base_ref</dt>
          <dd>{job.base_ref ?? '—'}</dd>
          <dt>inputs</dt>
          <dd>
            {Object.entries(job.inputs)
              .map(([k, v]) => `${k}←${v}`)
              .join(', ') || '—'}
          </dd>
          <dt>created</dt>
          <dd>{new Date(job.created_at).toLocaleString()}</dd>
        </dl>
        <div className="actions">
          {job.state === 'Frozen' && (
            <button
              onClick={() => api.release(owner, project, job.id).then(refresh, setActionError(setError))}
            >
              release
            </button>
          )}
          {job.state !== 'Done' && job.state !== 'Revoked' && (
            <button
              className="danger"
              onClick={() => api.revoke(owner, project, job.id).then(refresh, setActionError(setError))}
            >
              revoke
            </button>
          )}
        </div>
      </section>

      {pendingHuman.length > 0 && (
        <section className="card inbox">
          <h2>Awaiting you</h2>
          {pendingHuman.map((t) => (
            <div className="inbox-task" key={t.id}>
              <div className="inbox-head">
                task {t.id} · {t.phase}
                {t.evaluator ? ` · ${t.evaluator}` : ''}
              </div>
              {t.kind.kind === 'Human' && <pre className="prompt">{t.kind.prompt}</pre>}
              <ResolveForm
                escalation={job.state === 'Escalated'}
                onResolve={(r) =>
                  api.resolve(owner, project, job.id, t.id, r).then(refresh, setActionError(setError))
                }
              />
            </div>
          ))}
        </section>
      )}

      <section className="card">
        <h2>Tasks</h2>
        <table className="jobs">
          <thead>
            <tr>
              <th>#</th>
              <th>phase</th>
              <th>cycle</th>
              <th>kind</th>
              <th>state</th>
              <th>detail</th>
            </tr>
          </thead>
          <tbody>
            {tasks.map((t) => (
              <tr key={t.id}>
                <td>{t.id}</td>
                <td>{t.phase}</td>
                <td>
                  {t.cycle}
                  {t.attempt > 1 ? ` (attempt ${t.attempt})` : ''}
                </td>
                <td>
                  {t.kind.kind}
                  {t.evaluator ? ` · ${t.evaluator}` : ''}
                </td>
                <td>
                  <TaskBadge state={t.state} />
                </td>
                <td className="dim result-cell">{resultSummary(t)}</td>
              </tr>
            ))}
            {tasks.length === 0 && (
              <tr>
                <td colSpan={6} className="dim">
                  no tasks yet
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </section>

      {diff && diff.diff && (
        <section className="card">
          <h2>
            Diff{' '}
            <span className="dim">
              {diff.files.length} file{diff.files.length === 1 ? '' : 's'}, +
              {diff.files.reduce((n, f) => n + f.additions, 0)} −
              {diff.files.reduce((n, f) => n + f.deletions, 0)}
            </span>
          </h2>
          <DiffView diff={diff.diff} />
        </section>
      )}

      {events.length > 0 && (
        <section className="card">
          <h2>Live events</h2>
          <ul className="events">
            {[...events].reverse().map((e, i) => (
              <li key={events.length - i}>
                <span className="dim">{new Date(e.ts).toLocaleTimeString()}</span>{' '}
                <b>{e.event_type}</b>
                {'reason' in e && e.reason ? <span className="dim"> — {String(e.reason)}</span> : null}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}

function setActionError(setError: (s: string) => void) {
  return (e: unknown) => setError(e instanceof Error ? e.message : 'action failed')
}

function resultSummary(t: Task): string {
  const r = t.result as Record<string, unknown> | null
  if (!r) return ''
  const parts: string[] = []
  if ('pass' in r) parts.push(r.pass ? 'pass' : 'fail')
  if ('exit_code' in r && typeof r.exit_code === 'number') parts.push(`exit ${r.exit_code}`)
  if ('summary' in r && r.summary) parts.push(String(r.summary))
  if ('action' in r && r.action) parts.push(String(r.action))
  if ('structured' in r && r.structured) parts.push(JSON.stringify(r.structured))
  return parts.join(' · ').slice(0, 200)
}

// Plain unified-diff render with line coloring; react-diff-view lands with
// the full PWA pass (Part 11).
function DiffView({ diff }: { diff: string }) {
  return (
    <pre className="diff">
      {diff.split('\n').map((line, i) => (
        <div
          key={i}
          className={
            line.startsWith('+') && !line.startsWith('+++')
              ? 'diff-add'
              : line.startsWith('-') && !line.startsWith('---')
                ? 'diff-del'
                : line.startsWith('@@')
                  ? 'diff-hunk'
                  : line.startsWith('diff ') || line.startsWith('+++') || line.startsWith('---')
                    ? 'diff-file'
                    : undefined
          }
        >
          {line || ' '}
        </div>
      ))}
    </pre>
  )
}
