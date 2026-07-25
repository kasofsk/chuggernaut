import type { Job, JobState } from './api'

/**
 * Quick-filter keys (#162). Composable (AND) with search + state groups.
 *
 * Every key must be decidable from a jobs-*list* row. `waiting` (a job with a
 * pending human task) was not: it read `awaiting_human`, which only the
 * single-job reply carries — see {@link JobFull} — so it silently matched
 * nothing. It is gone rather than left dead; the states that put a job in front
 * of a human (Escalated/Stalled) are what `attention` now means.
 */
export type QuickKey = 'mine' | 'failed' | 'attention' | 'recent'

export interface JobFilters {
  /** free-text search over #id / title / type */
  q: string
  /** explicit JobState names to include; empty = no state constraint */
  states: JobState[]
  /** active quick filters (all must hold) */
  quick: QuickKey[]
  /** fold in the finished jobs the default view hides (Done/Revoked) */
  showFinished: boolean
}

export const EMPTY_FILTERS: JobFilters = { q: '', states: [], quick: [], showFinished: false }

/** JOBS OVERVIEW rows (#162): lifecycle buckets, each a one-click state filter. */
export const OVERVIEW_GROUPS: { key: string; label: string; color: string; states: JobState[] }[] = [
  { key: 'work', label: 'Work', color: 'blue', states: ['Work', 'WrapUp'] },
  { key: 'batched', label: 'Batched', color: 'gray', states: ['Batched'] },
  { key: 'frozen', label: 'Frozen', color: 'gray', states: ['Frozen', 'Draft'] },
  { key: 'eval', label: 'Evaluation', color: 'purple', states: ['Evaluation'] },
  { key: 'waiting', label: 'Waiting', color: 'orange', states: ['Blocked', 'Ready', 'Escalated', 'Stalled'] },
  { key: 'done', label: 'Completed', color: 'green', states: ['Done', 'Revoked'] },
]

const isTerminal = (s: JobState) => s === 'Done' || s === 'Revoked'

export function textMatch(j: Job, q: string): boolean {
  const s = q.trim().toLowerCase()
  if (!s) return true
  return (
    `#${j.id}`.includes(s) ||
    String(j.id).startsWith(s.replace('#', '')) ||
    (j.title ?? '').toLowerCase().includes(s) ||
    j.type.toLowerCase().includes(s)
  )
}

/** How many candidates a dependency picker's dropdown shows. */
const DEP_MATCHES_MAX = 8

/**
 * Dependency-picker candidates: jobs that can still be depended on — not
 * revoked, not already chosen, never the job itself — narrowed by the picker's
 * query (the same matching as the jobs list) and capped at what the dropdown
 * renders. One implementation, so the draft editor's picker and the create
 * form's picker cannot drift apart. `selfId` is omitted when composing a job
 * that does not exist yet.
 */
export function depCandidates(jobs: Job[], deps: number[], query: string, selfId?: number): Job[] {
  return jobs
    .filter((j) => j.id !== selfId && j.state !== 'Revoked' && !deps.includes(j.id))
    .filter((j) => textMatch(j, query))
    .slice(0, DEP_MATCHES_MAX)
}

const DAY = 86_400_000
function within(iso: string | null | undefined, ms: number): boolean {
  if (!iso) return false
  const t = Date.parse(iso)
  return Number.isFinite(t) && Date.now() - t <= ms
}

// A job is "mine/claimed" when a human has taken its next attempt. Without a
// per-job claimed-by field we treat any active claim as the operator's (noted in
// the redesign summary) — claim_next covers the pre-park window, a claimed
// pending Work task the in-flight one.
function quickPred(k: QuickKey, j: Job, claimed: Set<number>): boolean {
  switch (k) {
    case 'mine':
      return !!j.claim_next || claimed.has(j.id)
    case 'failed':
      return j.state === 'Revoked'
    case 'attention':
      return j.state === 'Escalated' || j.state === 'Stalled'
    case 'recent':
      return within(j.created_at, DAY) || within(j.completed_at, DAY)
  }
}

/**
 * The composed jobs-table predicate. Search AND state-group AND every active
 * quick filter; the finished-hiding gate applies only when nothing explicit is
 * selected, so choosing "Completed" or "Failed" reveals its terminal jobs.
 */
export function matchesFilters(j: Job, f: JobFilters, claimed: Set<number>): boolean {
  if (!textMatch(j, f.q)) return false
  if (f.states.length && !f.states.includes(j.state)) return false
  if (!f.quick.every((k) => quickPred(k, j, claimed))) return false
  const explicit = f.states.length > 0 || f.quick.length > 0
  if (!explicit && !f.showFinished && isTerminal(j.state)) return false
  return true
}

export function countQuick(jobs: Job[], k: QuickKey, claimed: Set<number>): number {
  return jobs.filter((j) => quickPred(k, j, claimed)).length
}

export function countStates(jobs: Job[], states: JobState[]): number {
  const set = new Set(states)
  return jobs.filter((j) => set.has(j.state)).length
}

/**
 * Daily activity buckets over the last `days` for the overview/footer sparkline:
 * how many jobs were created or completed on each day (oldest -> newest). Purely
 * derived from created_at/completed_at — no extra fetch, no chart lib.
 */
/**
 * Two-series daily activity for the stats chart: jobs created and jobs
 * completed per day over the window, oldest → newest, plus each bucket's
 * day-start timestamp for labels/tooltips. Same bucketing as activityBuckets
 * but the series stay separate so the chart can show flow in vs flow out.
 */
export function activitySeries(
  jobs: Job[],
  days = 14,
): { starts: number[]; created: number[]; completed: number[] } {
  const now = new Date()
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() - (days - 1) * DAY
  const created = new Array(days).fill(0)
  const completed = new Array(days).fill(0)
  const bump = (iso: string | null | undefined, series: number[]) => {
    if (!iso) return
    const t = Date.parse(iso)
    if (!Number.isFinite(t)) return
    const d = Math.floor((t - start) / DAY)
    if (d >= 0 && d < days) series[d] += 1
  }
  for (const j of jobs) {
    bump(j.created_at, created)
    bump(j.completed_at, completed)
  }
  return { starts: Array.from({ length: days }, (_, i) => start + i * DAY), created, completed }
}

export function activityBuckets(jobs: Job[], days = 14): number[] {
  const now = new Date()
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() - (days - 1) * DAY
  const buckets = new Array(days).fill(0)
  const bump = (iso: string | null | undefined) => {
    if (!iso) return
    const t = Date.parse(iso)
    if (!Number.isFinite(t)) return
    const d = Math.floor((t - start) / DAY)
    if (d >= 0 && d < days) buckets[d] += 1
  }
  for (const j of jobs) {
    bump(j.created_at)
    bump(j.completed_at)
  }
  return buckets
}

// ── URL <-> filter serialisation, so a filtered view is shareable (#162). ──
export function filtersToParams(f: JobFilters): URLSearchParams {
  const p = new URLSearchParams()
  if (f.q) p.set('q', f.q)
  if (f.states.length) p.set('state', f.states.join(','))
  if (f.quick.length) p.set('quick', f.quick.join(','))
  if (f.showFinished) p.set('finished', '1')
  return p
}

const QUICK_KEYS: QuickKey[] = ['mine', 'failed', 'attention', 'recent']
export function filtersFromParams(p: URLSearchParams): JobFilters {
  return {
    q: p.get('q') ?? '',
    states: (p.get('state') ?? '').split(',').filter(Boolean) as JobState[],
    quick: (p.get('quick') ?? '').split(',').filter((k): k is QuickKey => QUICK_KEYS.includes(k as QuickKey)),
    showFinished: p.get('finished') === '1',
  }
}

export function filtersActive(f: JobFilters): boolean {
  return !!f.q || f.states.length > 0 || f.quick.length > 0 || f.showFinished
}
