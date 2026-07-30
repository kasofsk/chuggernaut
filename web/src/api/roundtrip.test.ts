// The Rust → TypeScript round trip (NORTH-STAR §2 exit gate).
//
// `tsc -b` passing on `types.gen.ts` proves the generated types are well
// formed; it says nothing about whether they match what the server sends. The
// bytes in `wire-samples.json` come from `chuggernaut schema api-samples`,
// which serializes real Rust values through serde — the same code path that
// answers an HTTP request — so checking the UI's types against them closes the
// loop that the schema alone leaves open (a `skip_serializing_if` schemars
// reads differently, an adjacent tag, a chrono format).
//
// The heavy lifting is static: every payload appears in `wire-samples.gen.ts`
// as a fresh literal with `satisfies <GeneratedType>`, so a missing field, a
// wrong type, or an extra key serde emits fails the build. What runs here is
// what a type cannot state — that the generated module still carries the
// emitter's exact bytes, that the sample set still covers the surface the
// client returns, and that the unions narrow at runtime the way their types
// promise.

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
    // `wire-samples.gen.ts` is generated from `wire-samples.json`; if the two
    // disagree, the `satisfies` checks are validating something the server
    // never sent. (`npm run codegen:check` fails the same way in CI.)
    expect(wireSamples).toEqual(samplesJson)
  })

  it('cover every response record the client returns', () => {
    expect(Object.keys(wireSamples).sort()).toEqual([...COVERED])
  })

  it('narrow on their discriminant the way the generated unions claim', () => {
    // Adjacent tagging (`{"kind": "...", ...}`) is the shape most likely to
    // schematize differently from how serde writes it, so assert both a
    // response union and a request union round-trip through their tag.
    const result = wireSamples.TaskResult
    expect(result.kind).toBe('Agent')
    if (result.kind !== 'Agent') throw new Error('TaskResult sample is not the Agent arm')
    expect(result.pass).toBe(false)
    expect(result.abort).toBe(true)

    const resolution = wireSamples.TaskResolution
    expect(resolution.kind).toBe('Escalation')
    if (resolution.kind !== 'Escalation') throw new Error('TaskResolution sample is not Escalation')
    expect(resolution.action).toBe('Retry')

    // A nested internally-tagged enum, one level down from the record.
    const refresh = wireSamples.FleetStatus.nodes[0]?.refresh_outcome
    expect(refresh?.result.result).toBe('failed')
  })

  it('serialize timestamps as strings the UI can hand to Date', () => {
    // chrono's `DateTime<Utc>` schematizes as `string`/`date-time`, so every
    // component that formats a timestamp assumes a parseable string.
    for (const stamp of [wireSamples.Job.created_at, wireSamples.Task.created_at]) {
      expect(typeof stamp).toBe('string')
      expect(Number.isNaN(Date.parse(stamp))).toBe(false)
    }
  })

  it('omit skip_serializing_if fields rather than sending null', () => {
    // The `Job` sample sets every optional field, so the one field the record
    // never carries is the projection-only `channel` — proof the list and
    // single-job shapes really are different types, not one tolerant one.
    expect('channel' in wireSamples.Job).toBe(false)
    expect('description' in wireSamples.JobSummary).toBe(false)
  })
})
