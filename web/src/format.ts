import type { JobState } from './api'

/** Humane duration: '42s', '3m 12s', '1h 04m'. Shared by the job tables and the
 *  fleet slot widgets so running counters read identically everywhere. */
export function fmtDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000))
  if (total < 60) return `${total}s`
  const m = Math.floor(total / 60)
  if (m < 60) return `${m}m ${String(total % 60).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`
}

/** Map a lowercased job-phase string (as carried on a fleet slot occupant) back
 *  to the `JobState` the StateBadge understands, so slots reuse the one badge
 *  vocabulary. Returns null for an unrecognized phase (rendered as plain text). */
export function phaseToState(phase: string): JobState | null {
  switch (phase) {
    case 'work':
      return 'Work'
    case 'evaluation':
      return 'Evaluation'
    case 'wrap_up':
      return 'WrapUp'
    case 'ready':
      return 'Ready'
    case 'blocked':
      return 'Blocked'
    case 'escalated':
      return 'Escalated'
    case 'stalled':
      return 'Stalled'
    case 'done':
      return 'Done'
    default:
      return null
  }
}
