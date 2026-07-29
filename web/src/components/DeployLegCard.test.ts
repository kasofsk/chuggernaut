import { describe, expect, it } from 'vitest'
import type { CommandResult, DeployReport, Task, TaskResult, TriageResult, WorkResult } from '../api'
import { deployReportOf, deployReportOfTasks } from './DeployLegCard'

// Ticket #275: a deploy's command work task records `TaskResult::Work` when it
// succeeds and `TaskResult::Command` only when it fails, so a selector keyed on
// `kind` sees exactly one of the two. These cases pin both, plus the shapes
// that must keep returning null. Results are built as the real api.ts types so
// a wire-shape change breaks the test rather than passing against a mock.

// `satisfies` rather than an annotation: the literal type is what both
// `WorkResult.structured` (a DeployReport) and `CommandResult.structured` (a
// bare record) accept, so neither fixture needs a cast.
const report = (to: string) =>
  ({
    from_sha: 'a'.repeat(40),
    to_sha: to,
    health: 'ok',
    rollback: false,
    legs: [
      { name: 'build-dispatcher', status: 'ok', secs: 42 },
      { name: 'web-publish', status: 'ok', secs: 5 },
    ],
  }) satisfies DeployReport

function task(result: TaskResult | null): Task {
  return {
    id: 1,
    job_seq: 274,
    project: 'chuggernaut',
    phase: 'Work',
    cycle: 0,
    kind: { kind: 'Command', run: '.chug/tasks/deploy.sh' },
    state: 'Done',
    attempt: 1,
    evaluator: null,
    stage: 0,
    infra_loss: false,
    container_id: null,
    session_id: null,
    result,
    created_at: '2026-07-25T00:00:00Z',
    started_at: null,
    completed_at: null,
  }
}

const workResult = (structured: DeployReport | null): WorkResult => ({
  kind: 'Work',
  summary: 'deployed',
  structured,
  token_usage: null,
})

const commandResult = (structured: Record<string, unknown> | null): CommandResult => ({
  kind: 'Command',
  pass: structured === null,
  exit_code: structured === null ? 0 : 1,
  output: '',
  structured,
})

const triageResult: TriageResult = {
  kind: 'Triage',
  assessment: 'looks fine',
  token_usage: null,
}

describe('deployReportOfTasks', () => {
  it('finds the report on a successful deploy (Work result)', () => {
    const tasks = [task(workResult(report('b'.repeat(40)))), task(commandResult(null))]
    expect(deployReportOfTasks(tasks)?.to_sha).toBe('b'.repeat(40))
  })

  it('finds the report on a failed deploy (Command result)', () => {
    const tasks = [task(commandResult(report('c'.repeat(40))))]
    expect(deployReportOfTasks(tasks)?.to_sha).toBe('c'.repeat(40))
  })

  it('returns null when no task harvested a report', () => {
    const tasks = [task(workResult(null)), task(commandResult(null)), task(triageResult), task(null)]
    expect(deployReportOfTasks(tasks)).toBeNull()
  })

  it('prefers the newest attempt carrying a report', () => {
    const tasks = [
      task(workResult(report('1'.repeat(40)))),
      task(workResult(report('2'.repeat(40)))),
      task(workResult(null)),
    ]
    expect(deployReportOfTasks(tasks)?.to_sha).toBe('2'.repeat(40))
  })
})

describe('deployReportOf', () => {
  it('accepts a structured payload that arrived as a JSON string', () => {
    expect(deployReportOf(JSON.stringify(report('d'.repeat(40))))?.legs).toHaveLength(2)
  })

  it('rejects a payload with no legs, unparseable JSON, and null', () => {
    expect(deployReportOf({ notes: 'not a deploy' })).toBeNull()
    expect(deployReportOf('{not json')).toBeNull()
    expect(deployReportOf(null)).toBeNull()
  })
})
