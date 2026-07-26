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
import { fmtDuration } from '../format'
import {
  filtersFromParams,
  filtersToParams,
  matchesFilters,
  type JobFilters,
} from '../jobFilters'

// The states the state-filter dropdown offers, lifecycle-ordered. The default
// ("Active") is no filter at all — the finished-hiding gate in matchesFilters
// is what makes the unfiltered view active-only.
const FILTER_STATES: JobState[] = [
  'Draft', 'Frozen', 'Batched', 'Blocked', 'Ready', 'Work',
  'Evaluation', 'WrapUp', 'Escalated', 'Stalled', 'Done', 'Revoked',
]

type SortKey = 'id' | 'state' | 'type' | 'completed'

// Which direction a freshly picked sort key opens in: the numeric/temporal keys
// read newest-first, the categorical ones A→Z.
const sortDirDefault = (key: SortKey): 'asc' | 'desc' =>
  key === 'id' || key === 'completed' ? 'desc' : 'asc'

// The toolbar's sort-by control. It offers exactly the sortable columns, because
// on phones the table header — and with it the click-to-sort affordance — is not
// on screen: the rows render as two-line item widgets, not columns.
const SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: 'state', label: 'State' },
  { key: 'id', label: 'Number' },
  { key: 'type', label: 'Type' },
  { key: 'completed', label: 'Completed' },
]

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

// Tooltip for the completed cell: the full ISO instant plus the humanized
// created→completed WALL-CLOCK span. Deliberately not what the cell shows — the
// gap between this and the task time below is the queueing and waiting the job
// did while Frozen and Blocked. Undefined for live jobs so the cell has no
// tooltip.
export function completedTip(j: Job): string | undefined {
  if (!j.completed_at) return undefined
  const wall = fmtDuration(Date.parse(j.completed_at) - Date.parse(j.created_at))
  return `${j.completed_at} · ${wall} from creation to completion`
}

// The completed cell's muted hint: how long the job spent *working* — the sum of
// its own tasks' spans, carried on the jobs-list record (`task_time_ms`) so the
// table reads it straight off the row instead of fetching tasks per job. Null
// when no task carried a usable span, which the cell renders as no hint at all
// rather than a misleading '0s'; a real zero total still shows.
export function taskTimeHint(j: Job): string | null {
  return j.task_time_ms == null ? null : fmtDuration(j.task_time_ms)
}

// Compact "N ago" for a channel post's age. Coarse on purpose — the exact
// instant rides in the tooltip; here we only want a cheap glance ('2m ago').
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

// Collapse a channel message to a single line for the muted under-title glance
// (the full multi-line text rides in the tooltip). Runs of whitespace —
// including the newlines in markdown prose — become single spaces.
function oneLine(s: string): string {
  return s.replace(/\s+/g, ' ').trim()
}

// Jobs whose latest channel-update is worth surfacing under the title: the two
// live, agent-driven phases. Evaluation shares Work's channel stream, so it
// costs nothing extra to include (per the brief).
const CHANNEL_STATES = new Set(['Work', 'Evaluation'])

// State-column sort order. Alphabetical scatters related states, so rank them by
// lifecycle instead: inert pre-release first, terminal history next, live activity,
// then attention-needed at the very end — so the default descending click surfaces
// Escalated/Failed on top and sinks Frozen/Revoked to the bottom. Unknown/future
// states rank after everything (RANK_UNKNOWN) and still render their name as-is.
const RANK_UNKNOWN = 1000
const STATE_RANK: Record<string, number> = {
  Draft: 0, // inert pre-release, alongside Frozen
  Frozen: 1,
  Batched: 2, // inert member absorbed into a batch — adjacent to Frozen
  Revoked: 3, // terminal history
  Done: 4,
  Blocked: 5, // queued to run, between Done and Work
  Ready: 6,
  Work: 7, // live activity
  Evaluation: 8,
  WrapUp: 9,
  Failed: 10, // attention-needed, adjacent to Escalated
  Stalled: 11,
  Escalated: 12,
}
const stateRank = (state: string) => STATE_RANK[state] ?? RANK_UNKNOWN

export function ProjectPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [jobs, setJobs] = useState<Job[]>([])
  const [pending, setPending] = useState<Task[]>([])
  // Job seqs with a capacity-deferred launch waiting for a fleet slot (§3.5).
  const [queuedSeqs, setQueuedSeqs] = useState<Set<number>>(new Set())
  // Initial load only: skeleton until the first jobs fetch answers. SSE-driven
  // refreshes never re-skeleton; a project change resets it (effect below).
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  // Jobs table controls: column sort (default state-descending, so live and
  // attention-needed jobs surface on top). The filter model (#162) lives in
  // the URL so a filtered view is shareable.
  const [sort, setSort] = useState<{ key: SortKey; dir: 'asc' | 'desc' }>({ key: 'state', dir: 'desc' })
  // Batches whose member rows are unfolded in the table (collapsed by default).
  const [expandedBatches, setExpandedBatches] = useState<Set<number>>(new Set())
  const [params, setParams] = useSearchParams()
  const filters = filtersFromParams(params)
  const setFilters = useCallback(
    (f: JobFilters) => setParams(filtersToParams(f), { replace: true }),
    [setParams],
  )
  // The URL owns the filters (a filtered view is shareable, #162), but writing
  // `q` there on every keystroke re-runs the filter/sort/batch-grouping pipeline
  // over every job for each letter typed. So type into local state and push to
  // the URL once typing settles. `qPending` also tracks what we last wrote, so
  // the sync below can tell a URL change we caused from one we didn't
  // (back/forward, a shared link) and only adopt the latter.
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
        // Seed/refresh the progress lines from the list. A live SSE event is
        // always newer than the snapshot it raced, so let the existing ts guard
        // decide rather than clobbering: only fill in what we don't already
        // hold at an equal-or-newer stamp.
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
    // Best-effort capacity-queue snapshot (spec §3.5): the seqs with a launch
    // waiting for a fleet slot, for the subtle "queued" chip. Never blocks the
    // list — an unreachable dispatcher just drops the chip.
    api.queue(owner, project).then(
      (q) => setQueuedSeqs(new Set(q.entries.map((e) => e.seq))),
      () => setQueuedSeqs(new Set()),
    )
  }, [owner, project, navigate])

  // Latest channel-update message per job, surfaced muted under the title for
  // live jobs. Seeded from the jobs list itself (each live job carries its
  // latest post), then kept current from live SSE events. It used to be seeded
  // from the SSE history replay, which meant every page load downloaded the
  // project's entire event history — ~900 KB to annotate a handful of rows.
  // Reset on project change so stale rows don't bleed across navigations.
  const [channelMsgs, setChannelMsgs] = useState<Map<number, { message: string; ts: string }>>(
    new Map(),
  )
  useEffect(() => setChannelMsgs(new Map()), [owner, project])
  useEffect(() => setLoading(true), [owner, project])

  useEffect(refresh, [refresh])
  // The SSE stream is the source of truth (Part 11): any event → refetch. On
  // page load the stream replays the full history, so debounce to collapse that
  // burst (and any live burst) into a single refetch.
  const debouncedRefresh = useDebouncedCallback(refresh, 250)
  // Live fleet capacity readout (#148). Every occupancy change rides a task
  // lifecycle event already on this stream, so bump a tick (debounced against the
  // load-time replay burst) to refetch the snapshot. Admin-only feed: the widget
  // hides itself when unavailable. The tick only fires on *this* project's events,
  // but the fleet is shared across projects (#177) — so also poll on an interval,
  // catching a sibling project's occupancy changes and the end-of-burst quiescence
  // after which no further event fires. Same-project activity still updates instantly.
  const [fleetTick, setFleetTick] = useState(0)
  const bumpFleet = useDebouncedCallback(() => setFleetTick((n) => n + 1), 400)
  const fleet = useFleet({ tick: fleetTick, pollMs: 5000 })
  // Every event both feeds the debounced refetch and, when it's a channel-update,
  // updates the latest-message-per-job map. Guard on ts so an out-of-order frame
  // can't overwrite a newer message with an older one.
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
        : { key, dir: sortDirDefault(key) },
    )
  const flipSortDir = () => setSort((s) => ({ ...s, dir: s.dir === 'asc' ? 'desc' : 'asc' }))
  const sortIndicator = (key: SortKey) =>
    sort.key === key ? <span className="sort-ind">{sort.dir === 'asc' ? '▲' : '▼'}</span> : null

  // Filter + stickiness. The default view hides finished jobs, but a job that
  // was on screen must not vanish mid-view when the SSE refresh flips it to
  // Done/Revoked. `pinnedRef` remembers the ids currently shown and unions them
  // with the filter result on the next refresh, so a live transition stays put;
  // a job that is already finished the first time we see it is filtered out as
  // usual. Changing the filter resets the pins (finished rows then disappear).
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

  // Group batch members under their batch so they always travel as one unit:
  // while a batch is on screen its members never float free in the list — they
  // render indented directly beneath it, and only when the batch is expanded
  // (members come from the full jobs list, so expanding shows all of them even
  // if the current filter would hide Batched rows). A member whose batch is
  // filtered out falls back to an ordinary top-level row.
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

  const stateDropdownValue =
    filters.states.length === 1 ? filters.states[0] : filters.states.length ? '__multi' : ''

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
                onChange={(e) => {
                  const v = e.target.value
                  setFilters({ ...filters, states: v && v !== '__multi' ? [v as JobState] : [] })
                }}
                aria-label="Filter by state"
              >
                <option value="">Active</option>
                {stateDropdownValue === '__multi' && <option value="__multi">Filtered…</option>}
                {FILTER_STATES.map((s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
              </select>
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
                    // A live job has no completion moment: the table wants a
                    // placeholder in the column, the mobile item layout drops it.
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
