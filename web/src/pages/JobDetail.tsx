import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type DiffResponse, type Job, type JobCriteria, type Task } from '../api'
import { useProjectEvents, type JobEvent } from '../useEvents'
import { StateBadge, TaskBadge } from '../components/StateBadge'
import { ResolveForm } from '../components/ResolveForm'
import { TaskArtifacts } from '../components/TaskArtifacts'
import { EvaluatorTable } from '../components/EvaluatorTable'

export function JobDetail() {
  const { owner = '', project = '', seq = '' } = useParams()
  const jobSeq = Number(seq)
  const navigate = useNavigate()
  const [job, setJob] = useState<Job | null>(null)
  const [tasks, setTasks] = useState<Task[]>([])
  const [diff, setDiff] = useState<DiffResponse | null>(null)
  const [criteria, setCriteria] = useState<JobCriteria | null>(null)
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
      .then(() => api.criteria(owner, project, jobSeq).then(setCriteria, () => setCriteria(null)))
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
          #{job.id} <StateBadge state={job.state} />
          {job.awaiting_human && (
            <span className="badge badge-orange" title="a human task is pending in the inbox below">
              action needed
            </span>
          )}
        </h1>
      </header>
      {error && <div className="error banner">{error}</div>}

      <section className="card">
        <h2>
          {job.title || job.type} <span className="dim">{job.title ? job.type : ''}</span>
        </h2>
        {job.description && <pre className="prompt">{job.description}</pre>}
        <dl className="meta">
          <dt>branch</dt>
          <dd>{job.branch}</dd>
          <dt>base_ref</dt>
          <dd>{job.base_ref ?? '—'}</dd>
          <dt>depends on</dt>
          <dd>
            {job.deps.length
              ? job.deps.map((d, i) => (
                  <span key={d}>
                    {i > 0 && ', '}
                    <Link to={`/p/${owner}/${project}/jobs/${d}`}>#{d}</Link>
                  </span>
                ))
              : '—'}
          </dd>
          <dt>created</dt>
          <dd>{new Date(job.created_at).toLocaleString()}</dd>
          {criteria?.wrap_up && (
            <>
              <dt>wrap-up</dt>
              <dd>{criteria.wrap_up}</dd>
            </>
          )}
        </dl>
        <div className="actions">
          {job.state === 'Frozen' && (
            <button
              title="hand the job to the dispatcher: work → evaluation → wrap-up"
              onClick={() => api.release(owner, project, job.id).then(refresh, setActionError(setError))}
            >
              ▶ run
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

      {criteria && <CriteriaCard owner={owner} project={project} criteria={criteria} />}

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
                escalation={job.state === 'Escalated' || job.state === 'Stalled'}
                preWork={job.state === 'Stalled'}
                evaluator={t.phase === 'Evaluation'}
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
        <div className="table-scroll">
          <table className="jobs">
          <thead>
            <tr>
              <th>#</th>
              <th>phase</th>
              <th>cycle</th>
              <th>kind</th>
              <th>state</th>
              <th>detail</th>
              <th>artifacts</th>
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
                <td>
                  <TaskArtifacts owner={owner} project={project} seq={jobSeq} task={t} />
                </td>
              </tr>
            ))}
            {tasks.length === 0 && (
              <tr>
                <td colSpan={7} className="dim">
                  no tasks yet
                </td>
              </tr>
            )}
          </tbody>
          </table>
        </div>
      </section>

      <ChannelLog events={events} />

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

/**
 * The criteria the job will be (or was) judged against: the type's evaluators
 * plus any per-job additions, resolved at the same ref execution uses.
 */
function CriteriaCard({ owner, project, criteria }: { owner: string; project: string; criteria: JobCriteria }) {
  return (
    <section className="card">
      <h2>
        Evaluation criteria <span className="dim">at {criteria.ref.slice(0, 10)}</span>
      </h2>
      {criteria.errors.length > 0 && (
        <div className="error banner">
          {criteria.errors.map((e, i) => (
            <div key={i}>{e}</div>
          ))}
        </div>
      )}
      <EvaluatorTable owner={owner} project={project} evaluators={criteria.evaluators} showSource />
    </section>
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


/**
 * The agent's narrative, reconstructed from the event stream.
 *
 * The `channels` KV entry holds only the latest update and reply — it is a
 * status cache with a 7-day TTL, not a record. The durable history is the
 * `job-events` stream, which is also what feeds this page over SSE.
 */
function ChannelLog({ events }: { events: JobEvent[] }) {
  const posts = events.filter(
    (e) => e.event_type === 'channel-update' || e.event_type === 'channel-reply',
  )
  if (posts.length === 0) return null
  return (
    <section className="card">
      <h2>
        Channel <span className="dim">{posts.length}</span>
      </h2>
      <ul className="channel-log">
        {posts.map((e, i) => (
          <li key={i}>
            <span className="dim">{new Date(e.ts).toLocaleTimeString()}</span>{' '}
            {e.event_type === 'channel-update' ? (
              <>
                {typeof e.percent === 'number' && (
                  <span className="pct">{e.percent}%</span>
                )}{' '}
                {String(e.message ?? '')}
              </>
            ) : (
              <em>{String(e.text ?? '')}</em>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}
