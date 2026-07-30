import { describe, expect, it } from 'vitest'
import type { FleetNode } from './api'
import { CAPACITY_STALE_MS, capacityView, drainsFleetToZero } from './capacity'

// Design #293 §10: the six display states must stay distinguishable — a generic
// spinner or a bare number is what let the 2026-07-26 fleet run for weeks on an
// unconfirmed boot seed. These pin the ranking (which state wins when several
// apply), the over-cap drain case, and the fleet-wide-zero guard. Nodes are
// built as the real wire type so a snapshot-shape change breaks the test.

const NOW = Date.parse('2026-07-26T23:20:00Z')
const FRESH = '2026-07-26T23:19:50Z'
const OLD = '2026-07-26T23:05:00Z'

function node(over: Partial<FleetNode> = {}): FleetNode {
  return {
    name: 'air',
    slots: 2,
    occupied: 0,
    available: true,
    version: '0.1.0+abc',
    running: [],
    capacity_source: 'node',
    capacity_observed_at: FRESH,
    ...over,
  }
}

describe('capacityView — state mapping', () => {
  it('reads converged when the node reports the desired number', () => {
    const v = capacityView(node({ slots_desired: 2, capacity_state: 'converged' }), NOW)
    expect(v.state).toBe('converged')
    expect(v.observed).toBe(2)
    expect(v.desired).toBe(2)
  })

  it('reads converging while a set is in flight — the normal post-202 state', () => {
    const v = capacityView(node({ slots_desired: 4, capacity_state: 'pending' }), NOW)
    expect(v.state).toBe('converging')
    expect(v.observed).toBe(2)
    expect(v.desired).toBe(4)
  })

  it('reads unacknowledged when the node silently ignored the op', () => {
    expect(
      capacityView(node({ slots_desired: 4, capacity_state: 'unacknowledged' }), NOW).state,
    ).toBe('unacknowledged')
  })

  it('reads rejected and carries the daemon reason the operator must act on', () => {
    const v = capacityView(
      node({ slots_desired: 8, capacity_state: 'rejected', capacity_note: 'node max is 2' }),
      NOW,
    )
    expect(v.state).toBe('rejected')
    expect(v.note).toBe('node max is 2')
  })

  it('reads seed when the node has never reported its own capacity', () => {
    const v = capacityView(node({ capacity_source: 'seed', capacity_observed_at: null }), NOW)
    expect(v.state).toBe('seed')
    expect(v.observedAgoMs).toBeNull()
  })

  it('reads stale when the last report is well behind now', () => {
    const v = capacityView(
      node({ capacity_observed_at: OLD, slots_desired: 2, capacity_state: 'converged' }),
      NOW,
    )
    expect(v.state).toBe('stale')
    expect(v.observedAgoMs).toBeGreaterThan(CAPACITY_STALE_MS)
  })

  it('reads unknown for a node with no capacity provenance (docker endpoint)', () => {
    expect(
      capacityView(node({ capacity_source: null, capacity_observed_at: null }), NOW).state,
    ).toBe('unknown')
  })
})

describe('capacityView — ranking when several states apply', () => {
  it('a rejection outranks a seed number: only the operator can clear it', () => {
    const n = node({
      capacity_source: 'seed',
      capacity_observed_at: null,
      slots_desired: 8,
      capacity_state: 'rejected',
      capacity_note: 'node max is 2',
    })
    expect(capacityView(n, NOW).state).toBe('rejected')
  })

  it('a seed number outranks a pending push: it was never confirmed at all', () => {
    const n = node({
      capacity_source: 'seed',
      capacity_observed_at: null,
      slots_desired: 4,
      capacity_state: 'pending',
    })
    const v = capacityView(n, NOW)
    expect(v.state).toBe('seed')
    // …but the set still has to be acknowledged: outranked is not swallowed.
    expect(v.converging).toBe(true)
  })

  it('carries converging alongside staleness for the same reason', () => {
    const n = node({ capacity_observed_at: OLD, slots_desired: 4, capacity_state: 'pending' })
    const v = capacityView(n, NOW)
    expect(v.state).toBe('stale')
    expect(v.converging).toBe(true)
  })

  it('does not claim converging for a settled or refused node', () => {
    expect(capacityView(node({ capacity_state: 'converged' }), NOW).converging).toBe(false)
    expect(capacityView(node({ capacity_state: 'rejected' }), NOW).converging).toBe(false)
    expect(capacityView(node({ capacity_state: 'unacknowledged' }), NOW).converging).toBe(false)
  })

  it('staleness outranks the intent states: a silent node has not converged', () => {
    const n = node({ capacity_observed_at: OLD, slots_desired: 4, capacity_state: 'pending' })
    expect(capacityView(n, NOW).state).toBe('stale')
  })

  it('a report right on the staleness bound is not yet stale', () => {
    const at = new Date(NOW - CAPACITY_STALE_MS).toISOString()
    expect(capacityView(node({ capacity_observed_at: at }), NOW).state).toBe('converged')
  })
})

describe('capacityView — over cap and zero capacity', () => {
  it('tolerates running above the cap: a draining node, never a negative count', () => {
    const v = capacityView(node({ slots: 2, occupied: 3 }), NOW)
    expect(v.overCap).toBe(true)
    expect(v.observed).toBe(2)
  })

  it('a fully drained node still holding work is over cap', () => {
    expect(capacityView(node({ slots: 0, occupied: 1 }), NOW).overCap).toBe(true)
  })

  it('a fully drained idle node is not over cap', () => {
    const v = capacityView(node({ slots: 0, occupied: 0 }), NOW)
    expect(v.overCap).toBe(false)
    expect(v.observed).toBe(0)
  })

  it('a node at exactly its cap is not over cap', () => {
    expect(capacityView(node({ slots: 2, occupied: 2 }), NOW).overCap).toBe(false)
  })

  it('an unsized node is never over cap — its cap is unknown, not zero', () => {
    const v = capacityView(node({ slots: null, occupied: 1 }), NOW)
    expect(v.overCap).toBe(false)
    expect(v.observed).toBeNull()
  })
})

describe('capacityView — snapshot fields the dispatcher does not publish yet', () => {
  it('reads slots_max and the audit stamp when a newer dispatcher sends them', () => {
    const n = {
      ...node({ slots_desired: 2, capacity_state: 'converged' }),
      slots_max: 6,
      capacity_set_by: 'alice@example.com',
      capacity_set_at: '2026-07-26T23:14:02Z',
    } as FleetNode
    const v = capacityView(n, NOW)
    expect(v.slotsMax).toBe(6)
    expect(v.setBy).toBe('alice@example.com')
    expect(v.setAt).toBe('2026-07-26T23:14:02Z')
  })

  it('leaves them null when the snapshot omits them', () => {
    const v = capacityView(node(), NOW)
    expect(v.slotsMax).toBeNull()
    expect(v.setBy).toBeNull()
    expect(v.setAt).toBeNull()
  })
})

describe('drainsFleetToZero', () => {
  const fleet = [node({ name: 'air', slots: 2 }), node({ name: 'nuc', slots: 2 })]

  it('does not warn while another node still has capacity', () => {
    expect(drainsFleetToZero(fleet, 'air', 0)).toBe(false)
  })

  it('warns on the last node with capacity', () => {
    const drained = [node({ name: 'air', slots: 0 }), node({ name: 'nuc', slots: 2 })]
    expect(drainsFleetToZero(drained, 'nuc', 0)).toBe(true)
  })

  it('never warns on a raise', () => {
    expect(drainsFleetToZero(fleet, 'air', 4)).toBe(false)
  })

  it('stays quiet when the fleet already has no capacity', () => {
    const dead = [node({ name: 'air', slots: 0 }), node({ name: 'nuc', slots: 0 })]
    expect(drainsFleetToZero(dead, 'air', 0)).toBe(false)
  })

  it('ignores an out-of-service peer: it places nothing while it is out', () => {
    const one = [node({ name: 'air', slots: 2 }), node({ name: 'nuc', slots: 2, available: false })]
    expect(drainsFleetToZero(one, 'air', 0)).toBe(true)
  })
})
