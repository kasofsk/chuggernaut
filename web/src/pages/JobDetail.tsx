import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import {
  ApiError,
  api,
  type CommandResult,
  type DiffResponse,
  type EvalResult,
  type HumanResult,
  type Job,
  type JobCriteria,
  type ReviewFinding,
  type Task,
  type TaskResult,
  type TokenUsage,
  type WorkResult,
} from '../api'
import { useDebouncedCallback, useProjectEvents, type JobEvent } from '../useEvents'
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
  // Keep the per-event log immediate, but debounce the refetch: the SSE stream
  // replays the full history on load, so a naive refresh() per event fires a
  // storm of GETs. Coalesce that (and any live burst) into one refetch.
  const debouncedRefresh = useDebouncedCallback(refresh, 250)
  useProjectEvents(
    owner,
    project,
    (e) => {
      setEvents((prev) => [...prev.slice(-199), e])
      debouncedRefresh()
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

  // Human-kind tasks AND claimed attempts of any kind (§1.2 claims) — both
  // resolve through the inbox.
  const pendingHuman = tasks.filter(
    (t) => (t.kind.kind === 'Human' || t.performed_by === 'human') && t.state === 'Pending',
  )
  // Advisory triage runs (§1.2), newest first.
  const triageTasks = tasks
    .filter((t) => t.phase === 'Triage')
    .sort((a, b) => b.id - a.id)

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <Link to={`/p/${owner}/${project}`}>
          {owner}/{project}
        </Link>
        <h1>
          #{job.id} <StateBadge state={job.state} />
          {job.awaiting_human &&
            (job.awaiting_human.claimed ? (
              <span className="badge badge-purple" title="a claimed attempt is in progress — a human is doing the work">
                human working
              </span>
            ) : (
              <span className="badge badge-orange" title="a human task is pending in the inbox below">
                action needed
              </span>
            ))}
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
          {job.timeout && (
            <>
              <dt>timeout</dt>
              <dd title="per-job work-task timeout override (evaluators keep the type default)">
                <code>{job.timeout}</code> <span className="dim">override</span>
              </dd>
            </>
          )}
          {job.model && (
            <>
              <dt>model</dt>
              <dd title="per-job Work agent model override (evaluators keep the type/project/platform resolution)">
                <code>{job.model}</code> <span className="dim">override</span>
              </dd>
            </>
          )}
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
          {!job.claim_next &&
            (job.state === 'Frozen' || job.state === 'Blocked' || job.state === 'Ready') && (
              <button
                title="claim the next work attempt: it parks for you instead of launching (§1.2 claims)"
                onClick={() => api.claim(owner, project, job.id).then(refresh, setActionError(setError))}
              >
                claim
              </button>
            )}
          {job.claim_next && (
            <button
              title="clear the pending claim; the attempt will launch normally"
              onClick={() => api.unclaim(owner, project, job.id).then(refresh, setActionError(setError))}
            >
              unclaim
            </button>
          )}
          {(job.state === 'Escalated' || job.state === 'Stalled') && (
            <button
              title="run an advisory triage agent over the job state — assessment + recommendation, no change to the job"
              onClick={() => api.triage(owner, project, job.id).then(refresh, setActionError(setError))}
            >
              dispatch triage
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
              {t.performed_by === 'human' && (
                <div className="dim">
                  you claimed this attempt — push to {job.branch}, then pass to submit
                  (or fail to hand it back; the next attempt runs per the declared kind)
                </div>
              )}
              <ResolveForm
                escalation={job.state === 'Escalated' || job.state === 'Stalled'}
                preWork={job.state === 'Stalled'}
                evaluator={t.phase === 'Evaluation'}
                work={t.phase === 'Work' && job.state === 'Work'}
                onResolve={(r) =>
                  api.resolve(owner, project, job.id, t.id, r).then(refresh, setActionError(setError))
                }
              />
            </div>
          ))}
        </section>
      )}

      {triageTasks.length > 0 && (
        <section className="card">
          <h2>Triage <span className="dim">advisory — nothing changed on the job</span></h2>
          {triageTasks.map((t) => {
            const assessment = t.result?.kind === 'Triage' ? t.result.assessment : null
            return (
              <div className="triage-task" key={t.id}>
                <div className="inbox-head">
                  task {t.id} · cycle {t.cycle} · <TaskBadge state={t.state} />
                </div>
                {assessment ? (
                  <pre className="prompt">{assessment}</pre>
                ) : (
                  <div className="dim">{t.state === 'Running' ? 'running…' : 'no assessment produced'}</div>
                )}
              </div>
            )
          })}
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
              <th>started</th>
              <th>dur</th>
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
                  {t.performed_by === 'human' && <span className="dim"> · by human</span>}
                </td>
                <td>
                  <TaskBadge state={t.state} />
                </td>
                <td className="dim task-time" title={t.started_at ?? undefined}>
                  {fmtTime(t.started_at)}
                </td>
                <td className="dim task-time">{taskDuration(t)}</td>
                <td className="dim result-cell">{resultSummary(t)}</td>
                <td>
                  <TaskArtifacts owner={owner} project={project} seq={jobSeq} task={t} />
                </td>
              </tr>
            ))}
            {tasks.length === 0 && (
              <tr>
                <td colSpan={9} className="dim">
                  no tasks yet
                </td>
              </tr>
            )}
          </tbody>
          </table>
        </div>
      </section>

      <TaskReports tasks={tasks} />

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

// One-line gloss for the tasks table's detail column; the full report renders
// in the Reports thread below. Triage prose lives in its own section.
function resultSummary(t: Task): string {
  const r = t.result
  if (!r) return ''
  switch (r.kind) {
    case 'Work':
      return r.summary ? String(r.summary).slice(0, 200) : ''
    case 'Agent':
      return [r.pass ? 'pass' : 'fail', r.abort ? 'abort' : ''].filter(Boolean).join(' · ')
    case 'Command':
      return `${r.pass ? 'pass' : 'fail'} · exit ${r.exit_code}`
    case 'Human':
      return [r.pass ? 'pass' : 'fail', r.action ?? ''].filter(Boolean).join(' · ')
    case 'Triage':
      return 'assessment ↓'
    default:
      return ''
  }
}

/**
 * The Work→Evaluation→WrapUp thread, in task order (chronological): each task's
 * closing report rendered top-to-bottom so an operator can read what every
 * work/review/CI step actually said. Triage runs are advisory and render in
 * their own section above, so they're skipped here.
 */
function TaskReports({ tasks }: { tasks: Task[] }) {
  const thread = tasks.filter((t) => t.phase !== 'Triage')
  if (thread.length === 0) return null
  return (
    <section className="card">
      <h2>Reports</h2>
      <ol className="reports">
        {thread.map((t) => (
          <li className="report" key={t.id}>
            <div className="report-head">
              <span>
                task {t.id} · {t.phase}
                {t.evaluator ? ` · ${t.evaluator}` : ''}
                {t.performed_by === 'human' && <span className="dim"> · by human</span>}
              </span>
              <TaskBadge state={t.state} />
            </div>
            <TaskReportBody task={t} />
          </li>
        ))}
      </ol>
    </section>
  )
}

// Dispatches on the result's discriminant; an absent result is the crashed /
// never-ran case (e.g. a launch that found no free slot) — say so plainly
// rather than rendering blank. Unknown kinds fall through to raw JSON.
function TaskReportBody({ task }: { task: Task }) {
  const r = task.result
  if (!r) {
    if (task.state === 'Running') return <div className="dim">running…</div>
    if (task.state === 'Pending') return <div className="dim">not started</div>
    return <div className="report-empty dim">no report — task did not produce output</div>
  }
  switch (r.kind) {
    case 'Work':
      return <WorkReport r={r} />
    case 'Agent':
      return <EvalReport r={r} />
    case 'Command':
      return <CommandReport r={r} />
    case 'Human':
      return <HumanReport r={r} />
    default:
      return <RawReport r={r} />
  }
}

// Work agent: the summary paragraph up front, files_changed as a compact list,
// notes below.
function WorkReport({ r }: { r: WorkResult }) {
  const parsed = parseStructured(r.structured)
  const obj = parsed?.kind === 'object' ? parsed.value : null
  const files = Array.isArray(obj?.files_changed) ? (obj.files_changed as string[]) : null
  const notes = typeof obj?.notes === 'string' ? obj.notes : null
  return (
    <div className="report-body">
      {r.summary ? (
        <p className="report-summary">{r.summary}</p>
      ) : (
        <div className="dim">no summary</div>
      )}
      {files && files.length > 0 && (
        <ul className="report-files">
          {files.map((f, i) => (
            <li key={i}>
              <code>{f}</code>
            </li>
          ))}
        </ul>
      )}
      {notes && <p className="report-notes">{notes}</p>}
      <TokenChip usage={r.token_usage} />
    </div>
  )
}

// A structured payload as it arrives on the wire: an object, a JSON-encoded
// string (agent evaluators emit this), or absent. Parsing is tolerant — a
// string that is valid JSON *object* unwraps; anything else (parse failure, a
// bare string, an array/number) is surfaced verbatim rather than dropped.
type ParsedStructured =
  | { kind: 'object'; value: Record<string, unknown> }
  | { kind: 'raw'; text: string }
  | null

function parseStructured(structured: unknown): ParsedStructured {
  if (structured == null) return null
  if (typeof structured === 'string') {
    try {
      const parsed: unknown = JSON.parse(structured)
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return { kind: 'object', value: parsed as Record<string, unknown> }
      }
    } catch {
      // not JSON — fall through and show the raw string
    }
    return { kind: 'raw', text: structured }
  }
  if (typeof structured === 'object' && !Array.isArray(structured)) {
    return { kind: 'object', value: structured as Record<string, unknown> }
  }
  return { kind: 'raw', text: JSON.stringify(structured, null, 2) }
}

// Findings list: each finding tappable to reveal its issue + suggestion. Every
// field is optional, so an unexpected finding shape degrades to a bare row
// rather than crashing.
function FindingList({ findings }: { findings: ReviewFinding[] }) {
  return (
    <ul className="report-findings">
      {findings.map((f, i) => (
        <li key={i}>
          <details>
            <summary>
              {f?.file ? <code>{f.file}</code> : 'finding'}
              {f?.issue ? ` — ${f.issue}` : ''}
            </summary>
            {f?.issue && <p className="report-finding-issue">{f.issue}</p>}
            {f?.suggestion && <p className="report-finding-suggestion">{f.suggestion}</p>}
          </details>
        </li>
      ))}
    </ul>
  )
}

// Renders a parsed structured payload generically, keys open-ended: a
// `summary`/`notes` string becomes the readable body up front, a `findings`
// array becomes the expandable finding list, and every remaining key (verdict,
// scope_check, how_checked, anything unknown) collapses into a small details
// JSON block. An unparseable payload shows its raw text in the same block.
function StructuredBody({ parsed }: { parsed: ParsedStructured }) {
  if (!parsed) return null
  if (parsed.kind === 'raw') {
    return (
      <details className="report-output">
        <summary>details</summary>
        <pre className="output">{parsed.text}</pre>
      </details>
    )
  }
  const value = parsed.value
  const consumed = new Set<string>()
  let body: string | null = null
  if (typeof value.summary === 'string') {
    body = value.summary
    consumed.add('summary')
  } else if (typeof value.notes === 'string') {
    body = value.notes
    consumed.add('notes')
  }
  const findings = Array.isArray(value.findings) ? (value.findings as ReviewFinding[]) : null
  if (findings) consumed.add('findings')
  const rest = Object.entries(value).filter(([k]) => !consumed.has(k))
  return (
    <>
      {body && <p className="report-summary">{body}</p>}
      {findings && findings.length > 0 && <FindingList findings={findings} />}
      {rest.length > 0 && (
        <details className="report-output">
          <summary>details</summary>
          <pre className="output">{JSON.stringify(Object.fromEntries(rest), null, 2)}</pre>
        </details>
      )}
    </>
  )
}

// Agent evaluator (e.g. review): the verdict badge, then the structured payload
// rendered generically — the reviewer's summary is readable without expanding,
// findings stay tappable, and scope-check/verdict/unknown keys tuck into a
// collapsed details block. Handles both object and JSON-string wire shapes.
function EvalReport({ r }: { r: EvalResult }) {
  const parsed = parseStructured(r.structured)
  return (
    <div className="report-body">
      <div className="report-verdict">
        <span className={`badge ${r.pass ? 'badge-green' : 'badge-red'}`}>
          {r.pass ? 'passed' : 'failed'}
        </span>
        {r.abort && (
          <span className="badge badge-orange" title="not satisfiable by rework — escalates">
            abort
          </span>
        )}
      </div>
      <StructuredBody parsed={parsed} />
      <TokenChip usage={r.token_usage} />
    </div>
  )
}

// Command / CI evaluator: pass/fail + exit code, with the (long) output in a
// collapsed details that scrolls inside its own box — never widens the page.
function CommandReport({ r }: { r: CommandResult }) {
  const parsed = parseStructured(r.structured)
  return (
    <div className="report-body">
      <div className="report-verdict">
        <span className={`badge ${r.pass ? 'badge-green' : 'badge-red'}`}>
          {r.pass ? 'passed' : 'failed'}
        </span>
        <span className="dim">exit {r.exit_code}</span>
      </div>
      <StructuredBody parsed={parsed} />
      {r.output ? (
        <details className="report-output">
          <summary>output</summary>
          <pre className="output">{r.output}</pre>
        </details>
      ) : (
        <div className="dim">(no output)</div>
      )}
    </div>
  )
}

// A human resolution mirrored back as a result: the verdict, an operator note
// (render-if-present — backend persistence is landing separately), and any
// structured payload rendered the same generic way as an agent report.
function HumanReport({ r }: { r: HumanResult }) {
  const parsed = parseStructured(r.structured)
  return (
    <div className="report-body">
      <div className="report-verdict">
        <span className={`badge ${r.pass ? 'badge-green' : 'badge-red'}`}>
          {r.pass ? 'passed' : 'failed'}
        </span>
        {r.abort && <span className="badge badge-orange">abort</span>}
        {r.action && <span className="badge badge-blue">{r.action}</span>}
        {r.operator && <span className="dim">by {r.operator}</span>}
      </div>
      {r.summary && <p className="report-summary">{r.summary}</p>}
      <StructuredBody parsed={parsed} />
    </div>
  )
}

// Shape drift / kinds this UI doesn't model: show the payload verbatim rather
// than crash.
function RawReport({ r }: { r: TaskResult }) {
  const parsed = parseStructured((r as { structured?: unknown }).structured)
  return (
    <div className="report-body">
      <StructuredBody parsed={parsed} />
      <details className="report-output">
        <summary>result</summary>
        <pre className="output">{JSON.stringify(r, null, 2)}</pre>
      </details>
    </div>
  )
}

// Token accounting, small and muted; absent when the runner didn't measure it.
function TokenChip({ usage }: { usage?: TokenUsage | null }) {
  if (!usage) return null
  return (
    <div className="report-tokens dim">
      {usage.input_tokens.toLocaleString()} in · {usage.output_tokens.toLocaleString()} out
    </div>
  )
}

// A task's start time as a compact local HH:MM:SS; blank until it starts. The
// full ISO timestamp rides along as the cell's title tooltip (set by caller).
function fmtTime(iso: string | null): string {
  if (!iso) return ''
  return new Date(iso).toLocaleTimeString([], { hour12: false })
}

// Humane duration: '42s', '3m 12s', '1h 04m'.
function fmtDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000))
  if (total < 60) return `${total}s`
  const m = Math.floor(total / 60)
  if (m < 60) return `${m}m ${String(total % 60).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`
}

// Finished tasks show completed_at − started_at; Running shows elapsed since
// started_at (recomputed on the existing refresh cadence — no per-second
// timer); Pending is blank.
function taskDuration(t: Task): string {
  if (!t.started_at) return ''
  const start = new Date(t.started_at).getTime()
  if (t.state === 'Done' || t.state === 'Failed') {
    return t.completed_at ? fmtDuration(new Date(t.completed_at).getTime() - start) : ''
  }
  if (t.state === 'Running') return fmtDuration(Date.now() - start)
  return ''
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
