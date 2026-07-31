
import { describe, expect, it } from 'vitest'
import samplesJson from './wire-samples.json'
import { wireSamples } from './wire-samples.gen'

/** Response records the client hands back to a caller (see `api` in api.ts).
 *  A sample that quietly disappeared would take its `satisfies` check with it,
 *  so the covered set is pinned here rather than inferred from the file. */
const COVERED = [
  'DeployReport',
  'DesignEntry',
  'DispatcherConfigSnapshot',
  'FleetStatus',
  'GroupEntry',
  'Identity',
  'Job',
  'JobSummary',
  'JobType',
  'QueueSnapshot',
  'Task',
  'TaskResolution',
  'TaskResult',
] as const

describe('wire samples', () => {
  it('carry the emitter’s bytes verbatim', () => {
    expect(wireSamples).toEqual(samplesJson)
  })

  it('cover every response record the client returns', () => {
    expect(Object.keys(wireSamples).sort()).toEqual([...COVERED])
  })

  it('narrow on their discriminant the way the generated unions claim', () => {
    const result = wireSamples.TaskResult
    expect(result.kind).toBe('Agent')
    if (result.kind !== 'Agent') throw new Error('TaskResult sample is not the Agent arm')
    expect(result.pass).toBe(false)
    expect(result.abort).toBe(true)

    const resolution = wireSamples.TaskResolution
    expect(resolution.kind).toBe('Escalation')
    if (resolution.kind !== 'Escalation') throw new Error('TaskResolution sample is not Escalation')
    expect(resolution.action).toBe('Retry')

    const refresh = wireSamples.FleetStatus.nodes[0]?.refresh_outcome
    expect(refresh?.result.result).toBe('failed')
  })

  it('serialize timestamps as strings the UI can hand to Date', () => {
    for (const stamp of [wireSamples.Job.created_at, wireSamples.Task.created_at]) {
      expect(typeof stamp).toBe('string')
      expect(Number.isNaN(Date.parse(stamp))).toBe(false)
    }
  })

  it('omit skip_serializing_if fields rather than sending null', () => {
    expect('channel' in wireSamples.Job).toBe(false)
    expect('description' in wireSamples.JobSummary).toBe(false)
  })
})
