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

/** First 12 chars of a git SHA for compact display; '' for a nullish sha. */
export function shortSha(sha: string | null | undefined): string {
  return sha ? sha.slice(0, 12) : ''
}

/** Whether a node's reported build `version` (`{pkg}+{sha}`) carries the deployed
 *  `sha` — i.e. the node is running the deployed platform SHA. Mirrors the
 *  dispatcher's own confirm needle (`+{sha[..12]}`, types::worker). Returns false
 *  when either side is unknown, so an un-decidable node is never flagged stale. */
export function versionHasSha(
  version: string | null | undefined,
  sha: string | null | undefined,
): boolean {
  if (!version || !sha) return false
  return version.includes('+' + sha.slice(0, Math.min(12, sha.length)))
}

/** A worker node is *stale* relative to the deployed platform when both its
 *  version and the deployed SHA are known and the version does not carry that
 *  SHA. Unknown either side → not stale (we can't tell), never a false alarm. */
export function nodeStale(
  version: string | null | undefined,
  deployedSha: string | null | undefined,
): boolean {
  return !!version && !!deployedSha && !versionHasSha(version, deployedSha)
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
