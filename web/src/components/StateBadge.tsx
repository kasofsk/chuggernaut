import type { JobState } from '../api'

const COLORS: Record<JobState, string> = {
  Draft: 'gray',
  Frozen: 'gray',
  Batched: 'gray', // never read — special-cased below like Draft; kept for exhaustiveness
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

export function StateBadge({ state }: { state: JobState }) {
  // Draft is pre-release and editable: a dashed pill sets it apart from the
  // solid Frozen/terminal badges so a work-in-progress ticket reads at a glance.
  if (state === 'Draft')
    return (
      <span className="badge badge-draft">
        <span className="badge-dot" />
        Draft
      </span>
    )
  // Batched: inert member absorbed into a batch — a dashed gray pill sets it
  // apart from a solid Frozen, mirroring the Draft treatment for pre-scheduling.
  if (state === 'Batched')
    return (
      <span className="badge badge-batched">
        <span className="badge-dot" />
        Batched
      </span>
    )
  // Redesign (#161): a colored status dot leads the pill, keeping the existing
  // per-state hues.
  return (
    <span className={`badge badge-${COLORS[state]}`}>
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
