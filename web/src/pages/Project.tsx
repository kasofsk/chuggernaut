import { useCallback, useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type Job, type Task } from '../api'
import { useDebouncedCallback, useProjectEvents } from '../useEvents'
import { StateBadge } from '../components/StateBadge'
import { ResolveForm } from '../components/ResolveForm'
import { ProjectTabs } from '../components/ProjectTabs'
import { OriginPanel } from '../components/OriginPanel'

type SortKey = 'id' | 'state' | 'type' | 'completed'

// Compact local timestamp for the jobs table (reuses JobDetail's #57
// conventions): time-only when the moment is today, date prepended otherwise.
// Callers pass the raw ISO string as the cell's `title` for the full tooltip.
function fmtStamp(iso: string): string {
  const d = new Date(iso)
  const now = new Date()
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate()
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
  return sameDay ? time : `${d.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${time}`
}

// Humane duration, matching JobDetail's #57 task-duration format:
// '42s', '3m 12s', '1h 04m'.
function fmtDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000))
  if (total < 60) return `${total}s`
  const m = Math.floor(total / 60)
  if (m < 60) return `${m}m ${String(total % 60).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`
}

// Tooltip for the completed cell: the full ISO instant plus the humanized
// created→completed duration (the same hint shown muted in the cell). Undefined
// for live jobs so the cell has no tooltip.
function completedTip(j: Job): string | undefined {
  if (!j.completed_at) return undefined
  return `${j.completed_at} · took ${fmtDuration(Date.parse(j.completed_at) - Date.parse(j.created_at))}`
}

// State-column sort order. Alphabetical scatters related states, so rank them by
// lifecycle instead: inert pre-release first, terminal history next, live activity,
// then attention-needed at the very end — so the default descending click surfaces
// Escalated/Failed on top and sinks Frozen/Revoked to the bottom. Unknown/future
// states rank after everything (RANK_UNKNOWN) and still render their name as-is.
const RANK_UNKNOWN = 1000
const STATE_RANK: Record<string, number> = {
  Draft: 0, // inert pre-release, alongside Frozen
  Frozen: 1,
  Revoked: 2, // terminal history
  Done: 3,
  Blocked: 4, // queued to run, between Done and Work
  Ready: 5,
  Work: 6, // live activity
  Evaluation: 7,
  WrapUp: 8,
  Failed: 9, // attention-needed, adjacent to Escalated
  Stalled: 10,
  Escalated: 11,
}
const stateRank = (state: string) => STATE_RANK[state] ?? RANK_UNKNOWN

export function ProjectPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [pending, setPending] = useState<Task[]>([])
  const [error, setError] = useState<string | null>(null)
  // Jobs table controls: column sort (default newest-first) and a filter that
  // hides finished jobs by default.
  const [sort, setSort] = useState<{ key: SortKey; dir: 'asc' | 'desc' }>({ key: 'id', dir: 'desc' })
  const [showFinished, setShowFinished] = useState(false)

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
  // The SSE stream is the source of truth (Part 11): any event → refetch. On
  // page load the stream replays the full history, so debounce to collapse that
  // burst (and any live burst) into a single refetch.
  const debouncedRefresh = useDebouncedCallback(refresh, 250)
  useProjectEvents(owner, project, debouncedRefresh)

  const jobBySeq = new Map(jobs.map((j) => [j.id, j]))

  // Release readiness of a Frozen job's deps, resolved client-side from the same
  // jobs fetch (the list already holds every job). Releasing before every dep is
  // Done is at best a Blocked transition; a dep that is Revoked/Failed can never
  // satisfy it, so the job is un-runnable. Gate the run button accordingly and
  // surface which deps are the holdup instead of silently offering a bad action.
  const depGate = (j: Job) => {
    const unmet = j.deps.filter((d) => jobBySeq.get(d)?.state !== 'Done')
    if (unmet.length === 0) return { runnable: true as const }
    const dead = unmet.filter((d) => {
      const s = jobBySeq.get(d)?.state as string | undefined
      return s === 'Revoked' || s === 'Failed'
    })
    const refs = (ds: number[]) => ds.map((d) => `#${d}`).join(', ')
    return {
      runnable: false as const,
      dead: dead.length > 0,
      tip: dead.length
        ? `un-runnable — ${refs(dead)} ${dead.length > 1 ? 'are' : 'is'} revoked/failed`
        : `waiting on ${refs(unmet)}`,
    }
  }

  // Jobs whose Work attempt is parked for a human (a claim materialized: a human
  // is doing the work locally, no agent launched). The jobs LIST reply carries
  // `claim_next` but not the derived `awaiting_human`, so we read this off the
  // pending tasks the page already loads for the Inbox — a parked claimed attempt
  // is a Pending Work task with performed_by='human'. No extra fetch, no API
  // change: `claim_next` covers the pre-park window, this covers the in-Work
  // window, so the claimed badge persists for the whole life of the claim.
  const claimedInWork = new Set(
    pending.filter((t) => t.phase === 'Work' && t.performed_by === 'human').map((t) => t.job_seq),
  )

  // A terminal job's pending tasks are zombies — revoke doesn't (yet) close
  // them out server-side, and there is nothing valid to resolve. Hide them so
  // the inbox never offers actions the dispatcher will reject.
  const inbox = pending.filter((t) => {
    const state = jobBySeq.get(t.job_seq)?.state
    return state !== 'Revoked' && state !== 'Done'
  })

  // What the card is asking of the operator — the resolution vocabulary
  // differs (escalations take retry/resolve/revoke, the rest pass/fail).
  const askKind = (t: Task): string => {
    const state = jobBySeq.get(t.job_seq)?.state
    if (state === 'Escalated' || state === 'Stalled') return 'escalation'
    if (t.phase === 'Evaluation') return 'review'
    if (t.performed_by === 'human') return `claimed ${t.kind.kind.toLowerCase()} work`
    return 'human work'
  }

  async function act(fn: () => Promise<unknown>) {
    try {
      await fn()
      refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'action failed')
    }
  }

  const toggleSort = (key: SortKey) =>
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'id' || key === 'completed' ? 'desc' : 'asc' },
    )
  const sortIndicator = (key: SortKey) =>
    sort.key === key ? <span className="sort-ind">{sort.dir === 'asc' ? '▲' : '▼'}</span> : null

  // Filter + stickiness. The default view hides finished jobs, but a job that
  // was on screen must not vanish mid-view when the SSE refresh flips it to
  // Done/Revoked. `pinnedRef` remembers the ids currently shown and unions them
  // with the filter result on the next refresh, so a live transition stays put;
  // a job that is already finished the first time we see it is filtered out as
  // usual. Changing the filter resets the pins (finished rows then disappear).
  const pinnedRef = useRef<Set<number>>(new Set())
  const filterKeyRef = useRef(showFinished)
  if (filterKeyRef.current !== showFinished) {
    filterKeyRef.current = showFinished
    pinnedRef.current = new Set()
  }
  const passesFilter = (j: Job) => showFinished || (j.state !== 'Done' && j.state !== 'Revoked')
  const visible = jobs.filter((j) => passesFilter(j) || pinnedRef.current.has(j.id))
  pinnedRef.current = new Set(visible.map((j) => j.id))

  const sorted = [...visible].sort((a, b) => {
    if (sort.key === 'completed') {
      // Only terminal jobs have a completion moment; non-terminal jobs sink to
      // the bottom regardless of direction (nothing to order them by), so the
      // default descending click surfaces the most-recently-finished job first.
      const ta = a.completed_at ? Date.parse(a.completed_at) : null
      const tb = b.completed_at ? Date.parse(b.completed_at) : null
      if (ta === null && tb === null) return b.id - a.id
      if (ta === null) return 1
      if (tb === null) return -1
      const r = ta - tb || a.id - b.id
      return sort.dir === 'asc' ? r : -r
    }
    let r =
      sort.key === 'id'
        ? a.id - b.id
        : sort.key === 'state'
          ? stateRank(a.state) - stateRank(b.state)
          : a.type.localeCompare(b.type)
    if (r === 0) r = a.id - b.id // stable tiebreak
    return sort.dir === 'asc' ? r : -r
  })

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>
          {owner}/{project}
        </h1>
      </header>
      <ProjectTabs owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      <OriginPanel owner={owner} project={project} />

      {inbox.length > 0 && (
        <section className="card inbox">
          <h2>Inbox — {inbox.length} pending</h2>
          {inbox.map((t) => (
            <div className="inbox-task" key={`${t.job_seq}:${t.id}`}>
              <div className="inbox-head">
                <Link to={`/p/${owner}/${project}/jobs/${t.job_seq}`}>
                  #{t.job_seq}
                </Link>
                <span className="dim">
                  {' '}
                  · task {t.id} · {t.phase}
                  {t.evaluator ? ` · ${t.evaluator}` : ''} · {askKind(t)}
                </span>
              </div>
              {t.kind.kind === 'Human' && <pre className="prompt">{t.kind.prompt}</pre>}
              {t.performed_by === 'human' && (
                <div className="dim">
                  you claimed this attempt — push to job/{t.job_seq}, then pass to submit
                  (or fail to hand it back; the next attempt runs per the declared kind)
                </div>
              )}
              <ResolveForm
                escalation={
                  jobBySeq.get(t.job_seq)?.state === 'Escalated' ||
                  jobBySeq.get(t.job_seq)?.state === 'Stalled'
                }
                preWork={jobBySeq.get(t.job_seq)?.state === 'Stalled'}
                evaluator={t.phase === 'Evaluation'}
                work={
                  t.phase === 'Work' &&
                  jobBySeq.get(t.job_seq)?.state === 'Work'
                }
                onResolve={(r) => act(() => api.resolve(owner, project, t.job_seq, t.id, r))}
              />
            </div>
          ))}
        </section>
      )}

      <section className="card">
        <div className="row-head">
          <h2>Jobs</h2>
          <Link to={`/p/${owner}/${project}/jobs/new`}>
            <button>new job</button>
          </Link>
        </div>
        <div className="tag-row">
          <button
            type="button"
            className={`tag ${showFinished ? 'tag-on' : ''}`}
            onClick={() => setShowFinished((v) => !v)}
            title="include Done and Revoked jobs in the list"
          >
            show finished
          </button>
        </div>
        <div className="table-scroll">
          <table className="jobs">
          <thead>
            <tr>
              <th className="sortable" onClick={() => toggleSort('id')}>
                #{sortIndicator('id')}
              </th>
              <th>title</th>
              <th className="sortable" onClick={() => toggleSort('type')}>
                type{sortIndicator('type')}
              </th>
              <th className="sortable" onClick={() => toggleSort('state')}>
                state{sortIndicator('state')}
              </th>
              <th>deps</th>
              <th>created</th>
              <th className="sortable" onClick={() => toggleSort('completed')}>
                completed{sortIndicator('completed')}
              </th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((j, i) => {
              const gate = j.state === 'Frozen' ? depGate(j) : null
              return (
              <tr key={j.id} className={i % 2 ? 'row-stripe' : undefined}>
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
                  {(j.claim_next || (j.state === 'Work' && claimedInWork.has(j.id))) && (
                    <span
                      className="badge badge-purple"
                      title="work attempt claimed by a human — no agent will be launched"
                    >
                      claimed
                    </span>
                  )}
                </td>
                <td className="dim">
                  {j.deps.map((d) => `#${d}`).join(', ')}
                </td>
                <td className="dim" title={j.created_at}>
                  {fmtStamp(j.created_at)}
                </td>
                <td className="dim" title={completedTip(j)}>
                  {j.completed_at ? (
                    <>
                      {fmtStamp(j.completed_at)}
                      <span className="job-dur">
                        {fmtDuration(Date.parse(j.completed_at) - Date.parse(j.created_at))}
                      </span>
                    </>
                  ) : (
                    '—'
                  )}
                </td>
                <td className="actions">
                  {gate &&
                    (gate.runnable ? (
                      <button
                        title="hand the job to the dispatcher: work → evaluation → wrap-up"
                        onClick={() => act(() => api.release(owner, project, j.id))}
                      >
                        ▶ run
                      </button>
                    ) : (
                      <span
                        className={`run-gate${gate.dead ? ' run-gate-dead' : ''}`}
                        title={gate.tip}
                      >
                        {gate.dead ? '⨯ blocked' : '⏳ waiting'}
                      </span>
                    ))}
                  {!j.claim_next &&
                    (j.state === 'Frozen' || j.state === 'Blocked' || j.state === 'Ready') && (
                      <button
                        title="claim the next work attempt: it parks for you instead of launching (§1.2 claims)"
                        onClick={() => act(() => api.claim(owner, project, j.id))}
                      >
                        claim
                      </button>
                    )}
                  {j.claim_next && (
                    <button
                      title="clear the pending claim; the attempt will launch normally"
                      onClick={() => act(() => api.unclaim(owner, project, j.id))}
                    >
                      unclaim
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
              )
            })}
            {sorted.length === 0 && (
              <tr>
                <td colSpan={7} className="dim">
                  {jobs.length === 0 ? 'no jobs yet' : 'no jobs match — try “show finished”'}
                </td>
              </tr>
            )}
          </tbody>
          </table>
        </div>
      </section>
    </div>
  )
}
