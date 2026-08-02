import { describe, expect, it } from 'vitest'
import type { Job, JobState, Task } from './api'
import { APPROVAL_EVALUATOR, approvalIsEditable, isApprovalTask } from './approval'

function task(over: Partial<Task>): Task {
  return {
    id: 3,
    job_seq: 1,
    project: 'acme/api',
    phase: 'Evaluation',
    cycle: 1,
    kind: { kind: 'Human', prompt: 'sign off' },
    state: 'Pending',
    attempt: 1,
    evaluator: APPROVAL_EVALUATOR,
    stage: 1,
    infra_loss: false,
    created_at: '2026-08-01T09:00:00Z',
    ...over,
  } as Task
}

describe('isApprovalTask', () => {
  it('matches only the synthesized evaluation-phase gate', () => {
    expect(isApprovalTask(task({}))).toBe(true)
    expect(isApprovalTask(task({ evaluator: 'tests' }))).toBe(false)
    expect(isApprovalTask(task({ phase: 'Work', evaluator: undefined }))).toBe(false)
  })
})

describe('approvalIsEditable', () => {
  it('allows the pre-Work states and nothing past them', () => {
    const editable: JobState[] = ['Draft', 'Frozen', 'Blocked', 'Ready', 'Stalled']
    const locked: JobState[] = [
      'Batched',
      'Work',
      'Evaluation',
      'WrapUp',
      'Escalated',
      'Done',
      'Revoked',
    ]
    for (const state of editable) expect(approvalIsEditable({ state } as Job)).toBe(true)
    for (const state of locked) expect(approvalIsEditable({ state } as Job)).toBe(false)
  })
})
