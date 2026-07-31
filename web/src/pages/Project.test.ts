import { describe, expect, it } from 'vitest'
import type { Job } from '../api'
import { completedTip, taskTimeHint } from './Project'

function job(over: Partial<Job>): Job {
  return {
    id: 1,
    project: 'acme/api',
    type: 'code',
    title: 'a job',
    deps: [],
    state: 'Done',
    branch: 'job/1',
    base_ref: null,
    knowledge_tags: [],
    eval: [],
    timeout: null,
    model: null,
    factory: null,
    created_at: '2026-07-24T09:00:00Z',
    ready_at: null,
    ...over,
  } as Job
}

describe('taskTimeHint', () => {
  it("humanizes the record's task time", () => {
    expect(taskTimeHint(job({ task_time_ms: 45_000 }))).toBe('45s')
    expect(taskTimeHint(job({ task_time_ms: 18 * 60_000 }))).toBe('18m 00s')
    expect(taskTimeHint(job({ task_time_ms: 3 * 3600_000 }))).toBe('3h 00m')
  })

  it('shows no hint when the record carries no task time', () => {
    expect(taskTimeHint(job({}))).toBeNull()
    expect(taskTimeHint(job({ task_time_ms: null }))).toBeNull()
  })

  it('keeps a genuine zero distinct from nothing to show', () => {
    expect(taskTimeHint(job({ task_time_ms: 0 }))).toBe('0s')
  })
})

describe('completedTip', () => {
  it('carries the ISO instant and the created→completed wall clock', () => {
    const tip = completedTip(
      job({ created_at: '2026-07-24T09:00:00Z', completed_at: '2026-07-24T11:00:00Z' }),
    )
    expect(tip).toBe('2026-07-24T11:00:00Z · 2h 00m from creation to completion')
  })

  it('is absent for a live job', () => {
    expect(completedTip(job({ state: 'Work' }))).toBeUndefined()
  })
})
