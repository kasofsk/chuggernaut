import type { JobState } from '../api'

const COLORS: Record<JobState, string> = {
  Draft: 'gray',
  Frozen: 'gray',
  Batched: 'gray',
  Blocked: 'gray',
  Ready: 'blue',
  Work: 'blue',
  Evaluation: 'purple',
  WrapUp: 'blue',
  Escalated: 'orange',
  Stalled: 'orange',
  Done: 'green',
  Revoked: 'red',
}

/**
 * The badge hue a state reads as. Exported so anything that summarizes states
 * without drawing a full pill — the designs view's job histogram — speaks the
 * same color vocabulary, and a state means the same thing everywhere. Takes a
 * plain string because a derived read's histogram is keyed by the state name
 * the wire carries; an unknown state falls back to gray rather than blank.
 */
export function stateColor(state: string): string {
  return COLORS[state as JobState] ?? 'gray'
}

const ORDER = Object.keys(COLORS)

/**
 * A state's place in the lifecycle, for anything that lists several at once —
 * the designs view's job histogram, which is keyed by a `BTreeMap` and so
 * arrives alphabetized (`Done` before `Work`). Sorting by this reads left to
 * right as progress. An unknown state sorts last rather than first.
 */
export function stateRank(state: string): number {
  const i = ORDER.indexOf(state)
  return i < 0 ? ORDER.length : i
}

export function StateBadge({ state }: { state: JobState }) {
  if (state === 'Draft')
    return (
      <span className="badge badge-draft">
        <span className="badge-dot" />
        Draft
      </span>
    )
  if (state === 'Batched')
    return (
      <span className="badge badge-batched">
        <span className="badge-dot" />
        Batched
      </span>
    )
  return (
    <span className={`badge badge-${stateColor(state)}`}>
      <span className="badge-dot" />
      {state}
    </span>
  )
}

export function TaskBadge({ state }: { state: 'Pending' | 'Running' | 'Done' | 'Failed' }) {
  const color =
    state === 'Done' ? 'green' : state === 'Failed' ? 'red' : state === 'Running' ? 'blue' : 'gray'
  return <span className={`badge badge-${color}`}>{state}</span>
}
