import type { JobState } from '../api'

const COLORS: Record<JobState, string> = {
  Frozen: 'gray',
  Blocked: 'gray',
  Ready: 'blue',
  Work: 'blue',
  Evaluation: 'purple',
  Escalated: 'orange',
  Done: 'green',
  Revoked: 'red',
}

export function StateBadge({ state }: { state: JobState }) {
  return <span className={`badge badge-${COLORS[state]}`}>{state}</span>
}

export function TaskBadge({ state }: { state: 'Pending' | 'Running' | 'Done' | 'Failed' }) {
  const color =
    state === 'Done' ? 'green' : state === 'Failed' ? 'red' : state === 'Running' ? 'blue' : 'gray'
  return <span className={`badge badge-${color}`}>{state}</span>
}
