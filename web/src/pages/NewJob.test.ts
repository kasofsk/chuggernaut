import { describe, expect, it } from 'vitest'
import { createJobSubmitLabel } from './NewJob'

// Ticket #283: the create button used to be gated on a `useRef`, so tapping it
// re-fired nothing visible — no disable, no label change. The submit state is
// state now; this pins the label half of that (the disable half is the same
// `submitting` flag, wired to `disabled`/`aria-busy` on the same element).

describe('createJobSubmitLabel', () => {
  it('names the departure switch position when idle', () => {
    expect(createJobSubmitLabel('draft', false)).toBe('Create draft')
    expect(createJobSubmitLabel('frozen', false)).toBe('Create frozen')
    expect(createJobSubmitLabel('ready', false)).toBe('Create ready')
  })

  it('reports the in-flight create for every mode', () => {
    expect(createJobSubmitLabel('draft', true)).toBe('Creating…')
    expect(createJobSubmitLabel('frozen', true)).toBe('Creating…')
    expect(createJobSubmitLabel('ready', true)).toBe('Creating…')
  })
})
