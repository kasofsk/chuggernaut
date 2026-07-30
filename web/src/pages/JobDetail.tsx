import { Fragment, useCallback, useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import {
  ApiError,
  api,
  type CommandResult,
  type DiffResponse,
  type EvalResult,
  type HumanResult,
  type Job,
  type JobFull,
  type JobCriteria,
  type QueueSnapshot,
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
import { TaskLogPane } from '../components/TaskLogs'
import { EvaluatorTable } from '../components/EvaluatorTable'
import { Markdown } from '../components/Markdown'
import { DraftEditor } from '../components/DraftEditor'
import { ConsistEditor } from '../components/ConsistEditor'
import { CoverWidget } from '../components/CoverWidget'
import { JobAttachments } from '../components/Attachments'
import { ProjectHeader } from '../components/ProjectHeader'
import { Skeleton, SkeletonLines } from '../components/Skeleton'
import { DeployLegCard, deployReportOf } from '../components/DeployLegCard'
import { fmtDuration } from '../format'

// A fetch the page can do without: its failure resolves to null instead of
// rejecting, so it can be awaited alongside the required ones without taking
// the page down with it.
function bestEffort<T>(p: Promise<T>): Promise<T | null> {
  return p.then(
    (v) => v,
    () => null,
  )
}

// How many of a batch's members get their detail fetched. Fetching each member
// directly beats pulling the project's whole job list for the handful a batch
// normally holds, but it costs a request per member on *every* refresh and the
// spec puts no ceiling on `members` — so the fan-out is bounded, and a batch
// past the bound renders its remaining members as bare ids with a note saying
// so rather than issuing an unbounded burst per SSE event.
const MEMBER_DETAIL_MAX = 24

export function JobDetail() {
  const { owner = '', project = '', seq = '' } = useParams()
  const jobSeq = Number(seq)
  const navigate = useNavigate()
  const [job, setJob] = useState<JobFull | null>(null)
  // When this job is a batch, the member jobs it absorbs — fetched from the
  // project list so the Members section can show their titles and live states.
  const [members, setMembers] = useState<Job[]>([])
  const [tasks, setTasks] = useState<Task[]>([])
  const [diff, setDiff] = useState<DiffResponse | null>(null)
  // Capacity launch-queue snapshot (spec §3.5): drives the "queued" badge's
  // "position N of M". Best-effort — null when the dispatcher can't answer, in
  // which case the badge still renders, just without a position.
  const [queue, setQueue] = useState<QueueSnapshot | null>(null)
  const [criteria, setCriteria] = useState<JobCriteria | null>(null)
  const [events, setEvents] = useState<JobEvent[]>([])
  // Initial load only: skeleton until the first fetch answers. SSE-driven
  // refreshes never re-skeleton; a seq/project change resets it (effect below).
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  // The one task whose live-log pane is expanded, if any. Kept to a single open
  // pane so at most one tail loop polls at a time.
  const [openLogs, setOpenLogs] = useState<number | null>(null)
  // The platform's triage image (from the config snapshot): a string when triage
  // is available, null when it isn't (dispatcher rejects triage with 422), and
  // undefined while unknown — snapshot not yet loaded, offline, or forbidden.
  // We only disable the button on a confirmed null, so an unknown state keeps
  // the current behavior (the 422 mapping below still catches a live rejection).
  const [triageImage, setTriageImage] = useState<string | null | undefined>(undefined)
  // True while the run (release) request is in flight. Disables the run button
  // and shows a working label so a tap reads as acknowledged — and so a stray
  // second tap can't land on the revoke button that wraps directly below it.
  const [releasing, setReleasing] = useState(false)

  useEffect(() => {
    api.platformConfig().then(
      (c) => setTriageImage(c.dispatcher ? c.dispatcher.triage_image : undefined),
      () => setTriageImage(undefined),
    )
  }, [])

  // Everything the page needs, in one round trip. Nothing here depends on
  // another's result, so nothing may be chained: at operator latency (~240ms
  // RTT over the tailnet) each serialized fetch is another quarter second on
  // every load *and* every SSE-triggered refresh. `criteria` and `queue` stay
  // best-effort — bestEffort() turns their failure into null so they can ride
  // the same Promise.all without ever failing the page — which leaves
  // job/tasks/diff as the only rejectors, so the 401 → /login bounce below is
  // unchanged.
  const refresh = useCallback(() => {
    Promise.all([
      api.job(owner, project, jobSeq),
      api.tasks(owner, project, jobSeq),
      api.diff(owner, project, jobSeq),
      bestEffort(api.criteria(owner, project, jobSeq)),
      bestEffort(api.queue(owner, project)),
    ])
      .then(([j, ts, d, c, q]) => {
        setJob(j)
        setTasks(ts)
        setDiff(d)
        setCriteria(c)
        setQueue(q)
        setError(null)
        // A batch: fetch its member jobs directly — one small request each, in
        // parallel — rather than the project's entire job list to filter a
        // handful of rows out of it. Best-effort: a member that fails to load
        // just renders as its id. Non-batches clear any stale members.
        const memberIds = (j.members ?? []).slice(0, MEMBER_DETAIL_MAX)
        if (memberIds.length === 0) {
          setMembers([])
          return
        }
        Promise.all(memberIds.map((id) => bestEffort(api.job(owner, project, id)))).then((ms) =>
          setMembers(ms.filter((m): m is JobFull => m !== null)),
        )
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
      .finally(() => setLoading(false))
  }, [owner, project, jobSeq, navigate])

  useEffect(() => setLoading(true), [owner, project, jobSeq])
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

  // Live-ticking durations: a single shared clock that re-renders the duration
  // cells once a second while any visible task is Running or capacity-queued,
  // and is torn down otherwise (and on unmount). Finished tasks compute from
  // completed_at, so they don't tick — only the elapsed-since-start (or
  // queued-for) cells move.
  const [now, setNow] = useState(() => Date.now())
  const anyTicking = tasks.some((t) => t.state === 'Running' || isQueued(t))
  useEffect(() => {
    if (!anyTicking) return
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [anyTicking])

  // `loading` also covers a seq change while a stale job is still in state.
  if (!job || loading) {
    return (
      <div className="page">
        {error ? (
          <div className="error banner">{error}</div>
        ) : (
          <>
            <ProjectHeader owner={owner} project={project} />
            <h1 className="job-heading">
              <span className="job-num">#{seq}</span> <Skeleton width="4rem" height="1em" />
            </h1>
            <section className="card">
              <Skeleton width="16rem" height="1.3em" />
              <SkeletonLines n={5} />
            </section>
            <section className="card">
              <Skeleton width="8rem" height="1.2em" />
              <SkeletonLines n={4} />
            </section>
          </>
        )}
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

  // Triage is unavailable when the platform has no TRIAGE_IMAGE configured; the
  // dispatcher enforces this with a 422, so don't offer the action.
  const triageUnavailable = triageImage === null
  const triageMessage = 'triage unavailable — set TRIAGE_IMAGE on the platform'
  // A 422 means the config changed under us (race): show the friendly message
  // and reflect the now-known-unavailable state so the button disables too.
  const onTriageError = (e: unknown) => {
    if (e instanceof ApiError && e.status === 422) {
      setTriageImage(null)
      setError(triageMessage)
    } else setActionError(setError)(e)
  }

  // A Draft renders the live edit form in place of the read-only info card; its
  // task/criteria/diff sections are empty (nothing has run) so they're hidden.
  const isDraft = job.state === 'Draft'
  // A Draft *batch* (§2.1 draft batches): its members are edited as a consist
  // (train of cars) rather than the plain deps picker. A non-empty members list
  // is the batch marker.
  const isDraftBatch = isDraft && (job.members ?? []).length > 0
  // Already in the record's BTreeMap order (spec §1.1) — Object.entries keeps it.
  const inputEntries = Object.entries(job.inputs ?? {})

  // Tasks banded with an escalation resolution: the resolving Human task and the
  // failed attempts in the same cycle above it. They share an amber left edge in
  // the table so the story reads "these failed → a human stepped in".
  const escalationBand = new Set<number>()
  for (const t of tasks) {
    if (!isEscalationResolution(t)) continue
    escalationBand.add(t.id)
    for (const p of tasks) {
      if (p.id < t.id && p.cycle === t.cycle && p.state === 'Failed') escalationBand.add(p.id)
    }
  }

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      <h1 className="job-heading">
        <span className="job-num">#{job.id}</span> <StateBadge state={job.state} />
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
      {error && <div className="error banner">{error}</div>}

      {isDraft ? (
        <>
          <DraftEditor
            owner={owner}
            project={project}
            job={job}
            isBatch={isDraftBatch}
            onRelease={() => api.release(owner, project, job.id).then(refresh, setActionError(setError))}
            onFinalize={() => api.finalize(owner, project, job.id).then(refresh, setActionError(setError))}
            onRevoke={() => api.revoke(owner, project, job.id).then(refresh, setActionError(setError))}
            onLeftDraft={refresh}
          />
          {isDraftBatch && (
            <ConsistEditor
              owner={owner}
              project={project}
              job={job}
              onRelease={() => api.release(owner, project, job.id).then(refresh, setActionError(setError))}
              onFinalize={() => api.finalize(owner, project, job.id).then(refresh, setActionError(setError))}
              onRevoke={() => api.revoke(owner, project, job.id).then(refresh, setActionError(setError))}
              onEdited={refresh}
            />
          )}
        </>
      ) : (
      <section className="card">
        <h2>
          {job.title || job.type} <span className="dim">{job.title ? job.type : ''}</span>
        </h2>
        {job.cover_html && <CoverWidget html={job.cover_html} title="job cover" />}
        {job.description && <Markdown text={job.description} className="job-desc" />}
        <dl className="meta">
          <dt>branch</dt>
          <dd>{job.branch}</dd>
          <dt>base_ref</dt>
          <dd>{job.base_ref ?? '—'}</dd>
          {/* The job's EFFECTIVE inputs (spec §1.1, #311 Decision 6): supplied
              values plus the type's declared defaults, which the Ready
              transition materializes onto the record. A released job can
              therefore show a value its creator never typed — that is the audit
              surface, so defaulted entries are shown, not hidden. Immutable
              after that transition, hence read-only here. */}
          {inputEntries.length > 0 && (
            <>
              <dt>inputs</dt>
              <dd title="the values this run acted on — supplied at creation, completed with the type's declared defaults at the Ready transition, immutable thereafter">
                {inputEntries.map(([name, value]) => (
                  <div key={name}>
                    <span className="dim">{name}=</span>
                    {value}
                  </div>
                ))}
              </dd>
            </>
          )}
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
        {job.state === 'Batched' ? (
          <p className="batch-note dim">
            Absorbed into{' '}
            {job.batch_id != null ? (
              <Link to={`/p/${owner}/${project}/jobs/${job.batch_id}`}>batch #{job.batch_id}</Link>
            ) : (
              'a batch'
            )}
            {' '}— its single branch implements this job, so claim, release, and revoke happen on
            the batch, not here.
          </p>
        ) : (
        <div className="actions">
          {job.state === 'Frozen' && (
            <button
              disabled={releasing}
              title="hand the job to the dispatcher: work → evaluation → wrap-up"
              onClick={() => {
                setReleasing(true)
                api
                  .release(owner, project, job.id)
                  .then(refresh, setActionError(setError))
                  .finally(() => setReleasing(false))
              }}
            >
              {releasing ? '⏳ running…' : '▶ run'}
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
              disabled={triageUnavailable}
              title={
                triageUnavailable
                  ? triageMessage
                  : 'run an advisory triage agent over the job state — assessment + recommendation, no change to the job'
              }
              onClick={() => api.triage(owner, project, job.id).then(refresh, onTriageError)}
            >
              dispatch triage
            </button>
          )}
          {job.state !== 'Done' && job.state !== 'Revoked' && (
            <button
              className="danger"
              onClick={() => {
                if (!confirm(`Revoke job #${job.id}? This cancels the job and can't be undone.`)) return
                api.revoke(owner, project, job.id).then(refresh, setActionError(setError))
              }}
            >
              revoke
            </button>
          )}
        </div>
        )}
      </section>
      )}

      {!isDraft && (job.members ?? []).length > 0 && (
        <section className="card">
          <h2>
            Members <span className="dim">{(job.members ?? []).length}</span>
          </h2>
          {(job.members ?? []).length > MEMBER_DETAIL_MAX && (
            <p className="dim">
              Titles and states are loaded for the first {MEMBER_DETAIL_MAX} members; open a member
              to see the rest.
            </p>
          )}
          <ul className="batch-members">
            {(job.members ?? []).map((id) => {
              const m = members.find((x) => x.id === id)
              return (
                <li key={id}>
                  <Link to={`/p/${owner}/${project}/jobs/${id}`}>#{id}</Link>
                  <span className="batch-member-title">
                    {m ? m.title || <span className="dim">—</span> : <span className="dim">…</span>}
                  </span>
                  {m && <StateBadge state={m.state} />}
                </li>
              )
            })}
          </ul>
        </section>
      )}

      {!isDraft && criteria && <CriteriaCard owner={owner} project={project} criteria={criteria} />}

      {/* Attachments (spec §1.6): screenshots and reference files. The Draft
          editor renders its own copy, so only the read view shows it here. */}
      {!isDraft && <JobAttachments owner={owner} project={project} seq={jobSeq} />}

      {pendingHuman.length > 0 && (
        <section className="card inbox">
          <h2>Awaiting you</h2>
          {pendingHuman.map((t) => (
            <div className="inbox-task" key={t.id}>
              <div className="inbox-head">
                task {t.id} · {t.phase}
                {(t.evaluator ?? t.label) ? ` · ${t.evaluator ?? t.label}` : ''}
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

      {!isDraft && (
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
              <th>logs</th>
            </tr>
          </thead>
          <tbody>
            {tasks.map((t, i) => {
              const esc = isEscalationResolution(t)
              const rowClass =
                [i % 2 ? 'row-stripe' : '', escalationBand.has(t.id) ? 'row-escalation' : '', esc ? 'row-escalation-lead' : '']
                  .filter(Boolean)
                  .join(' ') || undefined
              return (
              <Fragment key={t.id}>
              <tr className={rowClass}>
                <td>{t.id}</td>
                <td><PhaseLabel phase={t.phase} escalation={esc} /></td>
                <td>
                  {t.cycle}
                  {t.attempt > 1 ? <span className="dim"> (attempt {t.attempt})</span> : ''}
                </td>
                <td>
                  {t.kind.kind}
                  {(t.evaluator ?? t.label) ? ` · ${t.evaluator ?? t.label}` : ''}
                  {t.performed_by === 'human' && <span className="dim"> · by human</span>}
                </td>
                <td>
                  {isQueued(t) ? (
                    <span className="badge badge-orange" title={queuedTooltip(t, queue)}>
                      queued
                    </span>
                  ) : (
                    <TaskBadge state={t.state} />
                  )}
                </td>
                <td
                  className="dim task-time"
                  title={(isQueued(t) ? t.queued_at : t.started_at) ?? undefined}
                >
                  {fmtTime(isQueued(t) ? t.queued_at ?? null : t.started_at)}
                </td>
                <td className="dim task-time">{taskDuration(t, now)}</td>
                <td className="dim result-cell">
                  {esc ? (
                    escalationDetail(t)
                  ) : neverLaunched(t) ? (
                    <span title="the attempt was never launched — no container ran">never launched</span>
                  ) : (
                    resultSummary(t)
                  )}
                </td>
                <td>
                  <TaskArtifacts owner={owner} project={project} seq={jobSeq} task={t} />
                </td>
                <td className="logs-cell">
                  {hasLogs(t) ? (
                    <button
                      className="linklike"
                      onClick={() => setOpenLogs((cur) => (cur === t.id ? null : t.id))}
                    >
                      {openLogs === t.id ? 'hide' : t.state === 'Running' ? 'live' : 'logs'}
                    </button>
                  ) : (
                    <span className="dim" title="no container output for this task">
                      —
                    </span>
                  )}
                </td>
              </tr>
              {openLogs === t.id && (
                <tr className="log-row">
                  <td colSpan={10}>
                    <TaskLogPane
                      owner={owner}
                      project={project}
                      seq={jobSeq}
                      task={t}
                      onClose={() => setOpenLogs(null)}
                    />
                  </td>
                </tr>
              )}
              </Fragment>
              )
            })}
            {tasks.length === 0 && (
              <tr>
                <td colSpan={10} className="dim">
                  no tasks yet
                </td>
              </tr>
            )}
          </tbody>
          </table>
        </div>
      </section>
      )}

      <ChannelLog events={events} />

      <TaskReports tasks={tasks} />

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

// Per-phase hue so the task table reads as a sequence of distinct phases at a
// glance rather than near-uniform rows. Hues match the app-wide state badges
// (Work=blue, Evaluation=purple) so the phase language is consistent
// everywhere. Merge gate is the evaluation family — a CI re-run against the
// squash candidate — so it shares evaluation's purple, set apart by its label +
// tooltip. WrapUp/Triage are the muted housekeeping/advisory phases. Escalation
// is amber and handled separately (see PhaseLabel's escalation branch).
const PHASE_HUE: Record<string, string> = {
  Work: 'blue',
  Evaluation: 'purple',
  MergeGate: 'purple',
  WrapUp: 'gray',
  Triage: 'gray',
}

// The task's phase, as a hued pill. An escalation resolution (a human stepping
// in to decide a run of failed attempts) renders as an amber `escalation` pill
// regardless of the phase the record was stamped under — old records carry the
// resolution under Work; see isEscalationResolution. MergeGate keeps its
// "why is a CI task running after evaluation passed?" tooltip. Unknown/future
// phases fall back to a neutral pill so they still read as a phase.
function PhaseLabel({ phase, escalation = false }: { phase: string; escalation?: boolean }) {
  if (escalation) {
    return (
      <span
        className="badge badge-orange"
        title="a human resolved the escalation for the failed attempt(s) above"
      >
        escalation
      </span>
    )
  }
  if (phase === 'MergeGate') {
    return (
      <span
        className="badge badge-purple"
        title="CI re-run against the squash candidate because the default branch moved"
      >
        merge gate
      </span>
    )
  }
  const hue = PHASE_HUE[phase] ?? 'gray'
  return <span className={`badge badge-${hue}`}>{phase}</span>
}

// Whether a task record is a human resolving an escalation, rather than an
// attempt of its stamped phase. The code side stamps these under a dedicated
// `Escalation` phase; older records carry them as a Human-kind result whose
// `action` (Retry/Resolve/Revoke) is the resolving decision — both are treated
// as escalation events so the table tells the story consistently.
function isEscalationResolution(t: Task): boolean {
  if (t.phase === 'Escalation') return true
  return t.result?.kind === 'Human' && !!t.result.action
}

// The escalation-event detail line: the resolving action and the operator who
// made the call, e.g. "escalation resolved: Retry — david@…".
function escalationDetail(t: Task): string {
  const r = t.result?.kind === 'Human' ? t.result : null
  const action = r?.action ?? '—'
  const op = r?.operator
  return `escalation resolved: ${action}${op ? ` — ${op}` : ''}`
}

// A Failed task that never got a container is a launch that never happened (no
// free slot, rejected before spawn) — 0s and no output, otherwise identical to
// a real agent crash (which spawns a container that then dies). Human tasks
// never spawn a container, so exclude them.
function neverLaunched(t: Task): boolean {
  return t.state === 'Failed' && !t.container_id && t.performed_by !== 'human' && !t.result
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
 * The Work→Evaluation→WrapUp thread, newest first: the latest task's closing
 * report sits on top (it's usually why the operator is here), with history
 * below it. Triage runs are advisory and render in their own section above,
 * so they're skipped here.
 */
function TaskReports({ tasks }: { tasks: Task[] }) {
  const thread = tasks.filter((t) => t.phase !== 'Triage').reverse()
  if (thread.length === 0) return null
  return (
    <section className="card">
      <h2>Reports</h2>
      <ol className="reports">
        {thread.map((t) => (
          <li className="report" key={t.id}>
            <div className="report-head">
              <div className="report-id">
                <span className="report-task">task {t.id}</span>
                <PhaseLabel phase={t.phase} escalation={isEscalationResolution(t)} />
                {t.evaluator && <span className="report-eval">{t.evaluator}</span>}
                <span className="dim">
                  cycle {t.cycle}
                  {t.attempt > 1 ? ` · attempt ${t.attempt}` : ''}
                </span>
                <span className="dim report-performer">{performerLabel(t)}</span>
                {t.started_at && (
                  <time className="dim" title={fmtIso(t.started_at)}>
                    {fmtStamp(t.started_at)}
                  </time>
                )}
              </div>
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
        <Markdown text={r.summary} className="report-summary" />
      ) : (
        <div className="dim">no summary</div>
      )}
      {r.cover_html && <CoverWidget html={r.cover_html} title="work cover" />}
      {files && files.length > 0 && (
        <ul className="report-files">
          {files.map((f, i) => (
            <li key={i}>
              <code>{f}</code>
            </li>
          ))}
        </ul>
      )}
      {notes && <Markdown text={notes} className="report-notes" />}
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
      {body && <Markdown text={body} className="report-summary" />}
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
      {r.cover_html && <CoverWidget html={r.cover_html} title="review cover" />}
      <TokenChip usage={r.token_usage} />
    </div>
  )
}

// Command / CI evaluator: pass/fail + exit code, with the (long) output in a
// collapsed details that scrolls inside its own box — never widens the page.
function CommandReport({ r }: { r: CommandResult }) {
  const parsed = parseStructured(r.structured)
  const deploy = deployReportOf(r.structured)
  return (
    <div className="report-body">
      <div className="report-verdict">
        <span className={`badge ${r.pass ? 'badge-green' : 'badge-red'}`}>
          {r.pass ? 'passed' : 'failed'}
        </span>
        <span className="dim">exit {r.exit_code}</span>
      </div>
      {deploy ? <DeployLegCard report={deploy} /> : <StructuredBody parsed={parsed} />}
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
      {r.summary && <Markdown text={r.summary} className="report-summary" />}
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

// A task parked Pending by the capacity launch queue (spec §3.5) — waiting for
// a fleet slot, not idle. It carries no container and no start time; `queued_at`
// anchors the queued-for duration.
export function isQueued(t: Task): boolean {
  return t.state === 'Pending' && t.pending_reason === 'QueuedForCapacity'
}

// The "queued" badge's tooltip: names the wait and, when the queue snapshot is
// available, this launch's position. Degrades gracefully to just the reason
// when the snapshot is missing (dispatcher unreachable) or the entry has
// already drained out of it.
function queuedTooltip(t: Task, queue: QueueSnapshot | null): string {
  const base = 'waiting for a fleet slot'
  const entry = queue?.entries.find((e) => e.seq === t.job_seq && e.task_id === t.id)
  return entry ? `${base} — position ${entry.position} of ${queue!.depth}` : base
}

// Finished tasks show completed_at − started_at; Running shows elapsed since
// started_at; a capacity-queued task shows how long it has waited (now −
// queued_at). All live cells are measured against the caller's shared `now`
// clock so they tick (JobDetail drives a 1s interval while any task is Running
// or queued); an idle Pending is blank.
function taskDuration(t: Task, now: number): string {
  if (isQueued(t)) return t.queued_at ? fmtDuration(now - new Date(t.queued_at).getTime()) : ''
  if (!t.started_at) return ''
  const start = new Date(t.started_at).getTime()
  if (t.state === 'Done' || t.state === 'Failed') {
    return t.completed_at ? fmtDuration(new Date(t.completed_at).getTime() - start) : ''
  }
  if (t.state === 'Running') return fmtDuration(now - start)
  return ''
}

// Whether the logs button shows for a task. Gate on state, not container_id:
// the record carries no container_id while an agent task is Running, so gating
// on it hides the button for the very case the log viewer exists to serve
// (tailing a live agent). Any container-backed run — Running ('live'), or a
// finished Done/Failed ('logs') — offers logs; the pane handles a not-yet-
// spawned container gracefully (404 → muted note, slow re-checks). Human tasks
// (claimed attempts, human-kind) never spawn a container, so they keep the dash.
function hasLogs(t: Task): boolean {
  if (t.performed_by === 'human') return false
  return t.state === 'Running' || t.state === 'Done' || t.state === 'Failed'
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
  // Newest first: the latest post is why the operator opens the section.
  const posts = events
    .filter((e) => e.event_type === 'channel-update' || e.event_type === 'channel-reply')
    .reverse()
  // Collapsed by default; the header toggles. While collapsed, a dot flags
  // posts that arrived after the page was opened — the SSE stream replays the
  // durable history on load, so replayed posts (ts before mount) never count
  // as "new", and opening the section acknowledges everything seen so far.
  const [open, setOpen] = useState(false)
  const [seen, setSeen] = useState(0)
  const mountTs = useRef(Date.now())
  useEffect(() => {
    if (open && seen !== posts.length) setSeen(posts.length)
  }, [open, seen, posts.length])
  if (posts.length === 0) return null
  const unread =
    !open &&
    posts.length > seen &&
    // newest-first: the unacknowledged posts are at the head of the list
    posts.slice(0, posts.length - seen).some((e) => Date.parse(String(e.ts)) > mountTs.current)
  return (
    <section className="card">
      <h2>
        <button
          type="button"
          className="card-fold"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
        >
          <span aria-hidden="true">{open ? '▾' : '▸'}</span> Channel{' '}
          <span className="dim">{posts.length}</span>
          {unread && (
            <span className="notif-dot" title="new channel activity since you opened this page" />
          )}
        </button>
      </h2>
      {open && (
      <ul className="channel-log">
        {posts.map((e, i) => {
          const reply = e.event_type === 'channel-reply'
          // channel-reply is the operator/platform replying to the agent;
          // channel-update is the agent's own progress note.
          const text = String((reply ? e.text : e.message) ?? '')
          const percent = !reply && typeof e.percent === 'number' ? e.percent : null
          const origin = channelOrigin(e)
          return (
            <li className={reply ? 'channel-post channel-reply' : 'channel-post'} key={i}>
              <div className="channel-head">
                <time className="dim channel-time" title={fmtIso(String(e.ts))}>
                  {fmtStamp(String(e.ts))}
                </time>
                {origin && <span className="badge channel-origin">{origin}</span>}
                <span className="channel-who">{reply ? '↩ reply' : 'agent'}</span>
                {percent != null && <span className="badge badge-blue pct">{percent}%</span>}
              </div>
              <Markdown text={text} className="channel-md" />
            </li>
          )
        })}
      </ul>
      )}
    </section>
  )
}

// Attribution for a channel post, straight from the event. Channel frames now
// carry their originating task's identity end to end (spec §6.3): `task_id`, an
// optional `phase`, and the evaluator name when the post came from an evaluator.
// We render a compact chip consistent with the Reports thread headers
// ("task 3 · review"). Legacy posts carry none of this → null (no chip), which
// is why the old timestamp-window guessing is gone.
function channelOrigin(e: JobEvent): string | null {
  const taskId = numField(e, 'task_id')
  if (taskId == null) return null
  const evaluator = strField(e, 'evaluator')
  const phase = strField(e, 'phase')
  const detail = evaluator ?? phase?.toLowerCase()
  return detail ? `task ${taskId} · ${detail}` : `task ${taskId}`
}

// A numeric field off an open-keyed event, or null when absent/non-numeric.
function numField(e: JobEvent, key: string): number | null {
  const v = e[key]
  return typeof v === 'number' ? v : null
}

// A non-empty string field off an open-keyed event, or null.
function strField(e: JobEvent, key: string): string | null {
  const v = e[key]
  return typeof v === 'string' && v.length > 0 ? v : null
}

// Who ran the task: the agent model (kind.model on Agent tasks) or, for a
// human-claimed/human-kind task, the operator's email if the result carries it.
function performerLabel(t: Task): string {
  if (t.performed_by === 'human') {
    const op = t.result?.kind === 'Human' ? t.result.operator : null
    return op ? `human · ${op}` : 'human'
  }
  if (t.kind.kind === 'Agent' && t.kind.model) return t.kind.model
  if (t.kind.kind === 'Command') return 'command'
  return ''
}

// A timestamp as compact local time, prefixed with the date when it isn't
// today (channel history can span days). The full ISO value rides as a tooltip.
function fmtStamp(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const time = d.toLocaleTimeString([], { hour12: false })
  if (d.toDateString() === new Date().toDateString()) return time
  return `${d.toLocaleDateString()} ${time}`
}

// Full ISO-8601 for a tooltip; the raw value back if it doesn't parse.
function fmtIso(iso: string): string {
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? iso : d.toISOString()
}
