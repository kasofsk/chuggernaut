import { useCallback, useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { ApiError, api, type Job, type JobState, type Task } from '../api'
import { useDebouncedCallback, useProjectEvents, type JobEvent } from '../useEvents'
import { StateBadge } from '../components/StateBadge'
import { ResolveForm } from '../components/ResolveForm'
import { ProjectHeader } from '../components/ProjectHeader'
import { OriginPanel } from '../components/OriginPanel'
import { CapacityWidget } from '../components/CapacityWidget'
import { StatusFooter } from '../components/StatusFooter'
import { IconSearch } from '../components/icons'
import { SkeletonTable } from '../components/Skeleton'
import { useFleet } from '../useFleet'
import { isApprovalTask } from '../approval'
import { fmtDuration } from '../format'
import { GroupChips } from '../components/JobGroups'
import {
  filtersFromParams,
  filtersToParams,
  groupOptions,
  matchesFilters,
  stateSelectFilters,
  stateSelectValue,
  STATE_ALL,
  STATE_MULTI,
  type JobFilters,
} from '../jobFilters'

const FILTER_STATES: JobState[] = [
  'Draft', 'Frozen', 'Batched', 'Blocked', 'Ready', 'Work',
  'Evaluation', 'WrapUp', 'Escalated', 'Stalled', 'Done', 'Revoked',
]

type SortKey = 'id' | 'state' | 'type' | 'completed'

const sortDirDefault = (key: SortKey): 'asc' | 'desc' =>
  key === 'id' || key === 'completed' ? 'desc' : 'asc'

const SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: 'state', label: 'State' },
  { key: 'id', label: 'Number' },
  { key: 'type', label: 'Type' },
  { key: 'completed', label: 'Completed' },
]

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

export function completedTip(j: Job): string | undefined {
  if (!j.completed_at) return undefined
  const wall = fmtDuration(Date.parse(j.completed_at) - Date.parse(j.created_at))
  return `${j.completed_at} · ${wall} from creation to completion`
}

export function taskTimeHint(j: Job): string | null {
  return j.task_time_ms == null ? null : fmtDuration(j.task_time_ms)
}

function fmtAge(iso: string): string {
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return ''
  const s = Math.max(0, Math.round((Date.now() - t) / 1000))
  if (s < 60) return `${s}s ago`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ago`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ago`
  return `${Math.floor(h / 24)}d ago`
}

function oneLine(s: string): string {
  return s.replace(/\s+/g, ' ').trim()
}

const CHANNEL_STATES = new Set(['Work', 'Evaluation'])

const RANK_UNKNOWN = 1000
const STATE_RANK: Record<string, number> = {
  Draft: 0,
  Frozen: 1,
  Batched: 2,
  Revoked: 3,
  Done: 4,
  Blocked: 5,
  Ready: 6,
  Work: 7,
  Evaluation: 8,
  WrapUp: 9,
  Failed: 10,
  Stalled: 11,
  Escalated: 12,
}
const stateRank = (state: string) => STATE_RANK[state] ?? RANK_UNKNOWN

export function ProjectPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [pending, setPending] = useState<Task[]>([])
  const [queuedSeqs, setQueuedSeqs] = useState<Set<number>>(new Set())
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [sort, setSort] = useState<{ key: SortKey; dir: 'asc' | 'desc' }>({ key: 'state', dir: 'desc' })
  const [expandedBatches, setExpandedBatches] = useState<Set<number>>(new Set())
  const [params, setParams] = useSearchParams()
  const filters = filtersFromParams(params)
  const setFilters = useCallback(
    (f: JobFilters) => setParams(filtersToParams(f), { replace: true }),
    [setParams],
  )
  const [qDraft, setQDraft] = useState(filters.q)
  const qPending = useRef(filters.q)
  const commitQ = useDebouncedCallback(() => setFilters({ ...filters, q: qPending.current }), 200)
  const urlQ = filters.q
  useEffect(() => {
    if (urlQ === qPending.current) return
    qPending.current = urlQ
    setQDraft(urlQ)
  }, [urlQ])

  const refresh = useCallback(() => {
    Promise.all([api.jobs(owner, project), api.pendingTasks(owner, project)])
      .then(([js, ts]) => {
        setJobs(js)
        setPending(ts)
        setChannelMsgs((prev) => {
          const next = new Map(prev)
          for (const j of js) {
            if (!j.channel?.at) continue
            const cur = next.get(j.id)
            if (cur && Date.parse(cur.ts) >= Date.parse(j.channel.at)) continue
            next.set(j.id, { message: j.channel.message, ts: j.channel.at })
          }
          return next
        })
        setError(null)
        setLoading(false)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
        setLoading(false)
      })
    api.queue(owner, project).then(
      (q) => setQueuedSeqs(new Set(q.entries.map((e) => e.seq))),
      () => setQueuedSeqs(new Set()),
    )
  }, [owner, project, navigate])

  const [channelMsgs, setChannelMsgs] = useState<Map<number, { message: string; ts: string }>>(
    new Map(),
  )
  useEffect(() => setChannelMsgs(new Map()), [owner, project])
  useEffect(() => setLoading(true), [owner, project])

  useEffect(refresh, [refresh])
  const debouncedRefresh = useDebouncedCallback(refresh, 250)
  const [fleetTick, setFleetTick] = useState(0)
  const bumpFleet = useDebouncedCallback(() => setFleetTick((n) => n + 1), 400)
  const fleet = useFleet({ tick: fleetTick, pollMs: 5000 })
  const onEvent = useCallback(
    (e: JobEvent) => {
      if (e.event_type === 'channel-update' && typeof e.message === 'string' && e.message) {
        const message = e.message
        const ts = String(e.ts)
        setChannelMsgs((prev) => {
          const cur = prev.get(e.job_seq)
          if (cur && Date.parse(cur.ts) >= Date.parse(ts)) return prev
          const next = new Map(prev)
          next.set(e.job_seq, { message, ts })
          return next
        })
      }
      debouncedRefresh()
      bumpFleet()
    },
    [debouncedRefresh, bumpFleet],
  )
  useProjectEvents(owner, project, onEvent)

  const jobBySeq = new Map(jobs.map((j) => [j.id, j]))

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

  const claimedInWork = new Set(
    pending.filter((t) => t.phase === 'Work' && t.performed_by === 'human').map((t) => t.job_seq),
  )

  const awaitingApproval = new Set(pending.filter(isApprovalTask).map((t) => t.job_seq))

  const inbox = pending.filter((t) => {
    const state = jobBySeq.get(t.job_seq)?.state
    return state !== 'Revoked' && state !== 'Done'
  })

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
        : { key, dir: sortDirDefault(key) },
    )
  const flipSortDir = () => setSort((s) => ({ ...s, dir: s.dir === 'asc' ? 'desc' : 'asc' }))
  const sortIndicator = (key: SortKey) =>
    sort.key === key ? <span className="sort-ind">{sort.dir === 'asc' ? '▲' : '▼'}</span> : null

  const filterKey = filtersToParams(filters).toString()
  const pinnedRef = useRef<Set<number>>(new Set())
  const filterKeyRef = useRef(filterKey)
  if (filterKeyRef.current !== filterKey) {
    filterKeyRef.current = filterKey
    pinnedRef.current = new Set()
  }
  const passesFilter = (j: Job) => matchesFilters(j, filters, claimedInWork)
  const visible = jobs.filter((j) => passesFilter(j) || pinnedRef.current.has(j.id))
  pinnedRef.current = new Set(visible.map((j) => j.id))

  const sorted = [...visible].sort((a, b) => {
    if (sort.key === 'completed') {
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
    if (r === 0) r = a.id - b.id
    return sort.dir === 'asc' ? r : -r
  })

  const byId = new Map(jobs.map((j) => [j.id, j]))
  const visibleBatchIds = new Set(
    sorted.filter((j) => (j.members ?? []).length > 0).map((j) => j.id),
  )
  const rows: { job: Job; member?: boolean }[] = []
  for (const j of sorted) {
    if (j.batch_id != null && visibleBatchIds.has(j.batch_id)) continue
    rows.push({ job: j })
    if ((j.members ?? []).length > 0 && expandedBatches.has(j.id)) {
      for (const mid of j.members ?? []) {
        const m = byId.get(mid)
        if (m) rows.push({ job: m, member: true })
      }
    }
  }

  const stateDropdownValue = stateSelectValue(filters)

  const groupChoices = groupOptions(jobs)
  const groupSelectable =
    filters.group && !groupChoices.includes(filters.group)
      ? [filters.group, ...groupChoices]
      : groupChoices

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
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
                approval={isApprovalTask(t)}
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

      <div className="jobs-layout jobs-layout-full">
        <section className="card jobs-main">
          <div className="jobs-toolbar">
            <div className="jobs-toolbar-title">
              <h2 className="jobs-h1">Jobs</h2>
            </div>
            <div className="jobs-controls">
              <div className="search-field">
                <IconSearch />
                <input
                  type="search"
                  placeholder="Search jobs…"
                  value={qDraft}
                  onChange={(e) => {
                    qPending.current = e.target.value
                    setQDraft(e.target.value)
                    commitQ()
                  }}
                  aria-label="Search jobs"
                />
              </div>
              <select
                className="state-filter"
                value={stateDropdownValue}
                onChange={(e) => setFilters(stateSelectFilters(filters, e.target.value))}
                aria-label="Filter by state"
              >
                <option value="">Active</option>
                <option value={STATE_ALL}>All</option>
                {stateDropdownValue === STATE_MULTI && (
                  <option value={STATE_MULTI}>Filtered…</option>
                )}
                {FILTER_STATES.map((s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
              </select>
              {groupSelectable.length > 0 && (
                <select
                  className="state-filter group-filter"
                  value={filters.group}
                  onChange={(e) => setFilters({ ...filters, group: e.target.value })}
                  aria-label="Filter by group"
                >
                  <option value="">All groups</option>
                  {groupSelectable.map((g) => (
                    <option key={g} value={g}>
                      {g}
                    </option>
                  ))}
                </select>
              )}
              <div className="sort-field">
                <select
                  className="sort-select"
                  value={sort.key}
                  onChange={(e) => {
                    const key = e.target.value as SortKey
                    setSort({ key, dir: sortDirDefault(key) })
                  }}
                  aria-label="Sort jobs by"
                >
                  {SORT_OPTIONS.map((o) => (
                    <option key={o.key} value={o.key}>
                      sort: {o.label}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  className="sort-dir"
                  onClick={flipSortDir}
                  title={sort.dir === 'asc' ? 'ascending — click to reverse' : 'descending — click to reverse'}
                  aria-label={`sort direction ${sort.dir === 'asc' ? 'ascending' : 'descending'}`}
                >
                  {sort.dir === 'asc' ? '▲' : '▼'}
                </button>
              </div>
            </div>
          </div>
          <div className="table-scroll">
            <table className="jobs jobs-list">
            <thead>
            <tr>
              <th className="spark-col" aria-label="starred"></th>
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
              <th className="sortable" onClick={() => toggleSort('completed')}>
                completed{sortIndicator('completed')}
              </th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {loading && !error && (
              <tr>
                <td colSpan={9}>
                  <SkeletonTable
                    rows={6}
                    widths={['1rem', '2rem', '16rem', '5rem', '5rem', '3rem', '4rem', '6rem', '8rem']}
                  />
                </td>
              </tr>
            )}
            {!loading && rows.map(({ job: j, member }, i) => {
              const gate = j.state === 'Frozen' ? depGate(j) : null
              const taskTime = taskTimeHint(j)
              return (
              <tr
                key={j.id}
                data-state={j.state}
                className={[
                  'row-accent',
                  i % 2 ? 'row-stripe' : '',
                  j.state === 'Draft' ? 'row-draft' : '',
                  member ? 'row-batch-member' : '',
                ]
                  .filter(Boolean)
                  .join(' ')}
              >
                <td className="spark-cell">
                  <span className="row-spark" aria-hidden="true">✦</span>
                </td>
                <td className="col-num">
                  <Link to={`/p/${owner}/${project}/jobs/${j.id}`}>{j.id}</Link>
                </td>
                <td className="col-title">
                  {member && (
                    <span className="batch-indent dim" aria-hidden="true">
                      ↳
                    </span>
                  )}
                  <Link to={`/p/${owner}/${project}/jobs/${j.id}`}>
                    <span className="col-title-num">#{j.id}</span>
                    {j.title || <span className="dim">—</span>}
                  </Link>
                  <GroupChips owner={owner} project={project} groups={j.groups} />
                  {CHANNEL_STATES.has(j.state) && channelMsgs.has(j.id) && (
                    <div className="job-channel dim" title={channelMsgs.get(j.id)!.message}>
                      <span className="job-channel-msg">{oneLine(channelMsgs.get(j.id)!.message)}</span>
                      <span className="job-channel-age">{fmtAge(channelMsgs.get(j.id)!.ts)}</span>
                    </div>
                  )}
                </td>
                <td className="col-type">
                  <Link className="dim" to={`/p/${owner}/${project}/job-types/${encodeURIComponent(j.type)}`}>
                    {j.type}
                  </Link>
                  {(j.members ?? []).length > 0 && (
                    <button
                      type="button"
                      className="chip-batch chip-batch-toggle"
                      title="a batch: implements its member jobs on one branch — click to show/hide them"
                      onClick={() =>
                        setExpandedBatches((prev) => {
                          const next = new Set(prev)
                          if (next.has(j.id)) next.delete(j.id)
                          else next.add(j.id)
                          return next
                        })
                      }
                    >
                      {expandedBatches.has(j.id) ? '▾' : '▸'} batch ({(j.members ?? []).length})
                    </button>
                  )}
                </td>
                <td className="col-state">
                  <StateBadge state={j.state} />
                  {j.state === 'Batched' && j.batch_id != null && !member && (
                    <Link
                      className="in-batch dim"
                      to={`/p/${owner}/${project}/jobs/${j.batch_id}`}
                      title="this job is absorbed into a batch — its work happens there"
                    >
                      in batch #{j.batch_id}
                    </Link>
                  )}
                  {(j.claim_next || (j.state === 'Work' && claimedInWork.has(j.id))) && (
                    <span
                      className="badge badge-purple"
                      title="work attempt claimed by a human — no agent will be launched"
                    >
                      claimed
                    </span>
                  )}
                  {queuedSeqs.has(j.id) && (
                    <span
                      className="badge badge-orange"
                      title="a container launch is waiting for a free fleet slot (§3.5)"
                    >
                      queued
                    </span>
                  )}
                  {awaitingApproval.has(j.id) ? (
                    <span
                      className="badge badge-orange"
                      title="every other criterion passed — this job is waiting on your sign-off"
                    >
                      your approval
                    </span>
                  ) : (
                    j.require_approval &&
                    !j.completed_at && (
                      <span
                        className="badge"
                        title="this job needs your sign-off after its other criteria pass (§1.1)"
                      >
                        approval gated
                      </span>
                    )
                  )}
                </td>
                <td className="dim col-deps">
                  {j.deps.map((d) => `#${d}`).join(', ')}
                </td>
                <td className="dim col-done" title={completedTip(j)}>
                  {j.completed_at ? (
                    <>
                      {fmtStamp(j.completed_at)}
                      {taskTime !== null && <span className="job-dur">{taskTime}</span>}
                    </>
                  ) : (
                    <span className="col-done-none">—</span>
                  )}
                </td>
                <td className="actions">
                  {j.state === 'Draft' && (
                    <Link to={`/p/${owner}/${project}/jobs/${j.id}`}>
                      <button title="open the live draft editor">edit</button>
                    </Link>
                  )}
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
                  {j.state !== 'Done' && j.state !== 'Revoked' && j.state !== 'Batched' && (
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
            {!loading && rows.length === 0 && (
              <tr>
                <td colSpan={9} className="dim">
                  {jobs.length === 0 ? 'no jobs yet' : 'no jobs match — adjust filters'}
                </td>
              </tr>
            )}
          </tbody>
            </table>
          </div>
        </section>
      </div>
      <StatusFooter jobs={jobs} fleet={fleet.fleet} fleetUnavailable={fleet.unavailable} />
      <CapacityWidget
        fleet={fleet.fleet}
        unavailable={fleet.unavailable}
        newJobHref={`/p/${owner}/${project}/jobs/new`}
      />
    </div>
  )
}
