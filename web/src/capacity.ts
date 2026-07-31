import type { FleetNode } from './api'

/**
 * The cluster view's worker-capacity control (design #293 §10) — its pure half.
 *
 * The fleet snapshot carries three independent things about a node's capacity:
 * what the node *reports* (`slots`, the only number placement ever reads), where
 * that number *came from* (`capacity_source`, `capacity_observed_at`), and how
 * far it is from the operator's *intent* (`slots_desired`, `capacity_state`,
 * `capacity_note`). Collapsing them into a spinner or a bare number is what let
 * a fleet run for weeks on a boot seed nothing had confirmed (design #293
 * "Problem"), so the mapping below is ranked and unit-tested rather than
 * improvised in JSX.
 */

/** A capacity report older than this reads as *stale*. The announce is ~15s and
 *  every placement attempt pulls one over ping, so minutes of silence means the
 *  number on screen is no longer being confirmed by anyone. */
export const CAPACITY_STALE_MS = 5 * 60 * 1000

/** Stepper ceiling used when the node's own `slots_max` is unknown. Advisory
 *  either way — design #293 §6 puts enforcement at the daemon, which is the only
 *  place that knows what the hardware can serve. */
export const CAPACITY_SLOTS_UI_MAX = 32

/**
 * The six states of design #293 §10, plus `unknown`.
 *
 * `seed` and `stale` are provenance ("is anyone confirming this number?");
 * `converging`/`unacknowledged`/`rejected`/`converged` are reconciliation ("how
 * far is the node from what the operator asked for?"). They can co-occur, so
 * {@link capacityView} ranks them by what the operator has to act on first.
 */
export type CapacityStateKind =
  | 'converged'
  | 'converging'
  | 'unacknowledged'
  | 'rejected'
  | 'seed'
  | 'stale'
  | 'unknown'

export interface CapacityView {
  state: CapacityStateKind
  /** The number placement uses. `None` for a node observed only through a
   *  running container, whose cap is unknown from occupancy alone. */
  observed: number | null
  /** The operator's last request, when one has ever been made. */
  desired: number | null
  /** The daemon's refusal reason, on `rejected`. */
  note: string | null
  /** A set is in flight, *independent of* the ranked state. `seed` and `stale`
   *  are provenance and outrank it, but a set on a seed node — the flagship case
   *  of design #293 — must still acknowledge the 202, so the strip shows the
   *  converging chip alongside the alarm rather than swallowing it. */
  converging: boolean
  /** How long since the node last reported its capacity; null when it never has. */
  observedAgoMs: number | null
  /** Running above the cap — a node finishing what it holds and taking nothing
   *  new (design #293 §5, drain never kills). A legitimate state, not a bug. */
  overCap: boolean
  /** The node's advertised ceiling, when the snapshot carries one. */
  slotsMax: number | null
  /** Audit stamp of the last capacity change. Last-writer only, not a history. */
  setBy: string | null
  setAt: string | null
}

/**
 * `slots_max`, `capacity_set_by` and `capacity_set_at` are not on the generated
 * `FleetNode`: the dispatcher does not publish them in `fleet.status` yet, so
 * design #293 §10's "stepper bounded by `slots_max`" and §9's audit line stay
 * inert until a dispatcher slice adds them. Read defensively rather than through
 * a hand-edited wire type — the api serves the snapshot verbatim precisely so a
 * newer dispatcher's fields reach the UI without an api deploy
 * (`platform_fleet_get`), and this is the reader half of that bargain.
 */
function extraNumber(node: FleetNode, field: string): number | null {
  const raw = (node as unknown as Record<string, unknown>)[field]
  return typeof raw === 'number' && Number.isInteger(raw) && raw >= 0 ? raw : null
}

function extraString(node: FleetNode, field: string): string | null {
  const raw = (node as unknown as Record<string, unknown>)[field]
  return typeof raw === 'string' && raw !== '' ? raw : null
}

/** Observation + intent + provenance + timestamps → one display state. */
export function capacityView(node: FleetNode, now: number): CapacityView {
  const observed = node.slots ?? null
  const at = node.capacity_observed_at ? Date.parse(node.capacity_observed_at) : NaN
  const observedAgoMs = Number.isNaN(at) ? null : Math.max(0, now - at)
  return {
    state: capacityState(node, observedAgoMs),
    observed,
    desired: node.slots_desired ?? null,
    note: node.capacity_note ?? null,
    converging: node.capacity_state === 'pending',
    observedAgoMs,
    overCap: observed != null && node.occupied > observed,
    slotsMax: extraNumber(node, 'slots_max'),
    setBy: extraString(node, 'capacity_set_by'),
    setAt: extraString(node, 'capacity_set_at'),
  }
}

/**
 * Ranked, most-actionable first: a **rejected** value is terminal — the
 * reconciler has stopped re-pushing and only the operator can clear it — and a
 * **seed** number was never confirmed by the node at all, which is the signature
 * of the 2026-07-26 incident and outranks anything derived from it. **Stale**
 * comes next because it makes every reconciliation state below it suspect: a
 * node that stopped reporting cannot be said to have converged. Only then the
 * intent states, and `converged` when there is nothing to say.
 */
function capacityState(node: FleetNode, observedAgoMs: number | null): CapacityStateKind {
  if (!node.capacity_source && !node.capacity_state && !node.capacity_observed_at) return 'unknown'
  if (node.capacity_state === 'rejected') return 'rejected'
  if (node.capacity_source === 'seed') return 'seed'
  if (observedAgoMs != null && observedAgoMs > CAPACITY_STALE_MS) return 'stale'
  if (node.capacity_state === 'unacknowledged') return 'unacknowledged'
  if (node.capacity_state === 'pending') return 'converging'
  return 'converged'
}

/**
 * Would setting `name` to `slots` leave the fleet with no capacity anywhere?
 *
 * Design #293 §5: a full drain never kills a running container, but the §3.5
 * maximum queue wait (default 30 min) keeps running, so queued launches escalate
 * with `no_free_slots_timeout`. A maintenance mode that pauses that clock is a
 * separate design — this warns, it does not prevent.
 *
 * Out-of-service nodes contribute nothing: they place no work while they are
 * out, so counting their caps would suppress the warning in exactly the case
 * that most deserves it. Per-node drains need no confirmation — only the last
 * node with capacity does.
 */
export function drainsFleetToZero(nodes: FleetNode[], name: string, slots: number): boolean {
  if (slots > 0) return false
  const capacity = (n: FleetNode) => (n.available ? (n.slots ?? 0) : 0)
  const before = nodes.reduce((total, n) => total + capacity(n), 0)
  const others = nodes.reduce((total, n) => total + (n.name === name ? 0 : capacity(n)), 0)
  return before > 0 && others === 0
}
