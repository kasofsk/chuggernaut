import { useState } from 'react'
import type { TaskResolution } from '../api'

export function ResolveForm({
  escalation,
  preWork = false,
  evaluator = false,
  approval = false,
  work = false,
  onResolve,
}: {
  escalation: boolean
  /** pre-work escalation (job Stalled): Resolve is not offered (§1.2) */
  preWork?: boolean
  /** human evaluator task: failing offers the abort verdict (design-lifecycle.md) */
  evaluator?: boolean
  /** the synthesized per-job approval gate (§1.1): the same Pass/Fail verbs,
   *  labelled as the sign-off decision the operator is actually making */
  approval?: boolean
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
        placeholder={approval ? 'notes (optional; required to reject)' : 'notes (optional; required to fail)'}
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
            {approval ? 'approve' : 'pass'}
          </button>
          <button
            className="danger"
            disabled={!structured}
            title={structured ? '' : approval ? 'rejecting requires notes' : 'failing requires notes'}
            onClick={() => structured && onResolve({ kind: 'Fail', structured, abort: abort || undefined })}
          >
            {approval ? 'reject' : 'fail'}
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
