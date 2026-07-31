import { describe, expect, it } from 'vitest'
import { createJobSubmitLabel } from './NewJob'

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
