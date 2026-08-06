import { describe, expect, it } from 'vitest'
import type { JobTypeSummary } from '../api'
import { createJobSubmitLabel, defaultJobType } from './NewJob'

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

describe('defaultJobType', () => {
  const type = (name: string): JobTypeSummary => ({ name, display_name: name, description: '' })

  it('opens on code wherever the project lists it', () => {
    expect(defaultJobType([type('design'), type('code'), type('docs')])?.name).toBe('code')
  })

  it('falls back to the first type when the project declares no code type', () => {
    expect(defaultJobType([type('design'), type('docs')])?.name).toBe('design')
    expect(defaultJobType([])).toBeUndefined()
  })
})
