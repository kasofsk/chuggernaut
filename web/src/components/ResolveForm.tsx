import { useState } from 'react'
import type { TaskResolution } from '../api'

// Human-task resolution (§1.2). Escalation tasks (job in Escalated) take
// Retry/Resolve/Revoke; work/eval Human tasks take Pass/Fail. Fail requires
// structured findings; for the rest it's optional context.
export function ResolveForm({
  escalation,
  evaluator = false,
  onResolve,
}: {
  escalation: boolean
  /** human evaluator task: failing offers the abort verdict (design-lifecycle.md) */
  evaluator?: boolean
  onResolve: (r: TaskResolution) => void
}) {
  const [notes, setNotes] = useState('')
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
          <button onClick={() => onResolve({ kind: 'Escalation', action: 'Resolve', structured })}>
            resolve
          </button>
          <button
            className="danger"
            onClick={() => onResolve({ kind: 'Escalation', action: 'Revoke', structured })}
          >
            revoke
          </button>
        </>
      ) : (
        <>
          <button onClick={() => onResolve({ kind: 'Pass', structured })}>pass</button>
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
