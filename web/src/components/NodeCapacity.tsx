import { useEffect, useState } from 'react'
import { ApiError, api, type FleetNode } from '../api'
import {
  CAPACITY_SLOTS_UI_MAX,
  type CapacityStateKind,
  type CapacityView,
  capacityView,
  drainsFleetToZero,
} from '../capacity'
import { fmtDuration } from '../format'

/**
 * The operator's capacity control on a cluster-view node card (design #293 §10):
 * a stepper for the node's **desired** slot count, wired to
 * `PUT /api/v1/platform/fleet/{node}/capacity`, sat under the six display states
 * of {@link capacityView}.
 *
 * Two things the presentation is deliberately careful about:
 *
 * - **The number the scheduler uses stays visually primary.** Intent is stored
 *   so it can be re-asserted; it never places work (design #293 §2), so a
 *   pending `2 → 4` must not read as though the fleet already had 4 slots.
 * - **A capacity path that fails must not look like health.** `seed`, `stale`,
 *   `unacknowledged` and `rejected` each get their own strip and wording rather
 *   than collapsing into a spinner — the absence of exactly that representation
 *   is what hid the 2026-07-26 incident for weeks.
 */
export function NodeCapacity({
  node,
  nodes,
  now,
  controllable,
  onChanged,
}: {
  node: FleetNode
  /** the whole fleet — the drain guard needs peers, not just this node */
  nodes: FleetNode[]
  now: number
  /** false for a docker-endpoint node, whose capacity `DOCKER_NODES` still owns
   *  and whose capacity command is a 409 */
  controllable: boolean
  /** refetch the fleet snapshot so convergence shows up without waiting a poll */
  onChanged: () => void
}) {
  const view = capacityView(node, now)
  const [draft, setDraft] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [confirming, setConfirming] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (draft != null && view.desired === draft) setDraft(null)
  }, [draft, view.desired])

  const max = view.slotsMax ?? CAPACITY_SLOTS_UI_MAX
  const value = Math.min(max, draft ?? view.desired ?? view.observed ?? 0)
  const dirty = value !== (view.desired ?? view.observed ?? 0)

  const submit = (slots: number) => {
    setBusy(true)
    setError(null)
    setConfirming(false)
    api.setNodeCapacity(node.name, slots).then(
      () => {
        setBusy(false)
        onChanged()
      },
      (e) => {
        setBusy(false)
        setError(e instanceof ApiError ? e.message : 'could not set capacity')
      },
    )
  }

  const onSet = () => {
    if (drainsFleetToZero(nodes, node.name, value)) setConfirming(true)
    else submit(value)
  }

  return (
    <div className="cap-ctl">
      <CapacityState view={view} controllable={controllable} />
      {controllable && (
        <div className="cap-stepper">
          <button
            type="button"
            className="cap-step"
            aria-label={`fewer slots on ${node.name}`}
            disabled={busy || value <= 0}
            onClick={() => setDraft(Math.max(0, value - 1))}
          >
            –
          </button>
          <span className="cap-step-value" aria-live="polite">
            {value}
          </span>
          <button
            type="button"
            className="cap-step"
            aria-label={`more slots on ${node.name}`}
            disabled={busy || value >= max}
            onClick={() => setDraft(Math.min(max, value + 1))}
          >
            +
          </button>
          <button type="button" className="cap-set" disabled={busy || !dirty} onClick={onSet}>
            {busy ? 'setting…' : 'set'}
          </button>
          <span className="cap-max dim">
            {view.slotsMax != null ? `max ${view.slotsMax}` : 'max: node-enforced'}
          </span>
        </div>
      )}
      {confirming && (
        <div className="cap-alarm" data-tone="bad">
          <div className="cap-alarm-head">
            <span className="badge badge-red">fleet-wide drain</span>
            <span>this is the last capacity in the fleet</span>
          </div>
          <div className="cap-alarm-body">
            Draining {node.name} to 0 leaves nothing anywhere to place work. Running containers
            finish — a drain never kills — but queued launches keep burning the 30-minute maximum
            queue wait and escalate with <code>no_free_slots_timeout</code>. There is no maintenance
            mode that pauses that clock.
          </div>
          <div className="cap-alarm-actions">
            <button type="button" className="cap-set danger" onClick={() => submit(value)}>
              drain anyway
            </button>
            <button type="button" className="cap-set" onClick={() => setConfirming(false)}>
              cancel
            </button>
          </div>
        </div>
      )}
      {error && <div className="cap-error">{error}</div>}
      <CapacityAudit view={view} />
    </div>
  )
}

const TONE: Record<CapacityStateKind, 'bad' | 'warn' | null> = {
  rejected: 'bad',
  unacknowledged: 'bad',
  seed: 'warn',
  stale: 'warn',
  converging: null,
  converged: null,
  unknown: null,
}

/** `1 slot`, `2 slots`, `? slots` — the counts are read aloud often enough that
 *  the singular is worth getting right. */
function plural(n: number | null): string {
  return n === 1 ? 'slot' : 'slots'
}

/**
 * `2 → 4 converging`. The observed number is rendered by the caller and stays
 * primary; this is only the delta, dimmed, because intent never places work
 * (design #293 §2). Renders nothing without a desired value — there is no
 * arrow to draw. Used from both the quiet line and the alarm heads, so the
 * acknowledgement of a 202 reads the same wherever it appears.
 */
function ConvergingChip({ desired }: { desired: number | null }) {
  if (desired == null) return null
  return (
    <>
      <span className="cap-arrow"> → </span>
      <span className="cap-want">{desired}</span>
      <span className="badge badge-blue cap-badge">converging</span>
    </>
  )
}

/** The six states of design #293 §10, each said in its own words. */
function CapacityState({ view, controllable }: { view: CapacityView; controllable: boolean }) {
  const { state, observed, desired, note, observedAgoMs, converging } = view
  const tone = TONE[state]
  const ago = observedAgoMs != null ? fmtDuration(observedAgoMs) : null

  if (state === 'unknown') {
    return (
      <div className="cap-line dim">
        <span className="cap-now">{observed ?? '?'}</span> {plural(observed)} ·{' '}
        {controllable
          ? 'no provenance reported — the dispatcher predates the capacity fields'
          : 'capacity owned by DOCKER_NODES'}
      </div>
    )
  }

  if (tone == null) {
    return (
      <div className="cap-line">
        <span className="cap-now">{observed ?? '?'}</span>
        <span className="dim"> {plural(observed)}</span>
        {state === 'converging' && <ConvergingChip desired={desired} />}
        <span className="cap-since dim">
          {state === 'converging'
            ? ' accepted — waiting for the node to report'
            : ago
              ? ` · node reported ${ago} ago`
              : ''}
        </span>
      </div>
    )
  }

  return (
    <div className="cap-alarm" data-tone={tone}>
      <div className="cap-alarm-head">
        <span className={`badge ${tone === 'bad' ? 'badge-red' : 'badge-orange'}`}>
          {state === 'seed' ? 'capacity from boot seed' : state}
        </span>
        <span className="cap-now">{observed ?? '?'}</span>
        <span className="dim"> {plural(observed)} in use by the scheduler</span>
        {converging && <ConvergingChip desired={desired} />}
      </div>
      <div className="cap-alarm-body">
        {state === 'seed' &&
          'This node has never reported its own capacity — the number above is the DOCKER_NODES boot seed standing in for a report that never arrived. It is not confirmed by the node.'}
        {state === 'stale' &&
          `The node last reported its capacity ${ago} ago. Nothing is confirming the number above; an announce or a placement probe should refresh it within seconds.`}
        {state === 'unacknowledged' &&
          `Desired ${desired ?? '?'}, node still reporting ${observed ?? '?'}. The intent is recorded and was pushed, but the node has neither adopted nor refused it — the signature of a daemon too old to understand the command.`}
        {state === 'rejected' && (
          <>
            The node refused the value{note ? <>: “{note}”</> : '.'} The dispatcher has stopped
            re-pushing it, so this stands until you set a value the node will take.
          </>
        )}
      </div>
    </div>
  )
}

/** The last capacity change, when the snapshot carries the audit stamp. Design
 *  #293 §9 is honest that this is last-writer-only, not a history, so it is
 *  worded as one change rather than as a log. */
function CapacityAudit({ view }: { view: CapacityView }) {
  if (view.setBy == null && view.setAt == null) return null
  const at = view.setAt ? new Date(view.setAt) : null
  const when = at && !Number.isNaN(at.getTime()) ? at.toLocaleString() : null
  return (
    <div className="cap-audit dim" title="the last change only — capacity changes keep no history">
      {view.desired ?? '?'} {plural(view.desired)} — last set by {view.setBy ?? 'unknown'}
      {when ? `, ${when}` : ''}
    </div>
  )
}
