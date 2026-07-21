import { useState } from 'react'
import type { TaskResolution } from '../api'

// Human-task resolution (§1.2). Post-work escalation tasks (job Escalated) take
// Retry/Resolve/Revoke; pre-work escalations (job Stalled) take only
// Retry/Revoke — there is nothing to submit for evaluation. Work/eval Human
// tasks take Pass/Fail. Fail requires structured findings; else it's optional.
export function ResolveForm({
  escalation,
  preWork = false,
  evaluator = false,
  work = false,
  onResolve,
}: {
  escalation: boolean
  /** pre-work escalation (job Stalled): Resolve is not offered (§1.2) */
  preWork?: boolean
  /** human evaluator task: failing offers the abort verdict (design-lifecycle.md) */
  evaluator?: boolean
  /** human-performed work attempt: Pass offers a summary — it becomes the
   *  squash-merge commit body, like an agent's submit_result (§1.2 claims) */
  work?: boolean
  onResolve: (r: TaskResolution) => void
}) {
  const [notes, setNotes] = useState('')
  const [summary, setSummary] = useState('')
  const [abort, setAbort] = useState(false)
  const structured = notes.trim() ? { notes: notes.trim() } : null

  return (
    <div className="resolve">
      <input
        placeholder="notes (optional; required to fail)"
        value={notes}
        onChange={(e) => setNotes(e.target.value)}
      />
      {escalation ? (
        <>
          <button onClick={() => onResolve({ kind: 'Escalation', action: 'Retry', structured })}>
            retry
          </button>
          {!preWork && (
            <button onClick={() => onResolve({ kind: 'Escalation', action: 'Resolve', structured })}>
              resolve
            </button>
          )}
          <button
            className="danger"
            onClick={() => onResolve({ kind: 'Escalation', action: 'Revoke', structured })}
          >
            revoke
          </button>
        </>
      ) : (
        <>
          {work && (
            <input
              placeholder="summary (optional; becomes the merge commit body)"
              value={summary}
              onChange={(e) => setSummary(e.target.value)}
            />
          )}
          <button
            onClick={() =>
              onResolve({ kind: 'Pass', structured, summary: summary.trim() || undefined })
            }
          >
            pass
          </button>
          <button
            className="danger"
            disabled={!structured}
            title={structured ? '' : 'failing requires notes'}
            onClick={() => structured && onResolve({ kind: 'Fail', structured, abort: abort || undefined })}
          >
            fail
          </button>
          {evaluator && (
            <label className="dim" title="not satisfiable by rework — skip retries and escalate to a human">
              <input type="checkbox" checked={abort} onChange={(e) => setAbort(e.target.checked)} />{' '}
              abort (unfixable)
            </label>
          )}
        </>
      )}
    </div>
  )
}
