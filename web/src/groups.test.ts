import { describe, expect, it } from 'vitest'
import type { Job } from './api'
import { groupHref, groupNameError } from './groups'
import { EMPTY_FILTERS, filtersFromParams, filtersToParams, groupOptions, matchesFilters } from './jobFilters'

// Job groups on the jobs surface (design #321 slice C2): where a chip leads,
// what the picker refuses before the round trip, and the group filter — which
// has to show *finished* jobs, since annotating a Done job is the case the
// feature exists for.

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
    created_at: '2026-07-30T09:00:00Z',
    ready_at: null,
    ...over,
  } as Job
}

const NO_CLAIMS = new Set<number>()

describe('groupHref', () => {
  it('sends a design/ name to its document', () => {
    expect(groupHref('acme', 'api', 'design/321-job-groups')).toBe(
      '/p/acme/api/designs/321-job-groups',
    )
  })

  it('sends every other name to the jobs list filtered to it', () => {
    expect(groupHref('acme', 'api', 'beacon-import')).toBe('/p/acme/api?group=beacon-import')
    // A namespaced non-design group, and a design/ name whose slug is itself a
    // path — neither is a design route, so both fall back to the filter.
    expect(groupHref('acme', 'api', 'ops/fleet-refresh')).toBe(
      '/p/acme/api?group=ops%2Ffleet-refresh',
    )
    expect(groupHref('acme', 'api', 'design/a/b')).toBe('/p/acme/api?group=design%2Fa%2Fb')
  })
})

describe('groupNameError', () => {
  it('accepts the shapes the dispatcher accepts', () => {
    expect(groupNameError('design/321-job-groups', [])).toBeNull()
    expect(groupNameError('beacon-import', ['design/311-job-inputs'])).toBeNull()
  })

  it('names the rule a bad candidate broke', () => {
    expect(groupNameError('Design/321', [])).toMatch(/lowercase/)
    expect(groupNameError('/leading-slash', [])).toMatch(/lowercase/)
    expect(groupNameError('x'.repeat(129), [])).toMatch(/128/)
    expect(groupNameError('dup', ['dup'])).toBe('already on this job')
    expect(groupNameError('nine', ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'])).toMatch(/8 groups/)
  })
})

describe('the group filter', () => {
  const grouped = job({ id: 314, groups: ['design/311-job-inputs'] })
  const ungrouped = job({ id: 315, state: 'Frozen' })

  it('keeps only the jobs carrying the label', () => {
    const f = { ...EMPTY_FILTERS, group: 'design/311-job-inputs' }
    expect(matchesFilters(grouped, f, NO_CLAIMS)).toBe(true)
    expect(matchesFilters(ungrouped, f, NO_CLAIMS)).toBe(false)
  })

  it('shows finished members — a group is mostly finished jobs', () => {
    // Same Done job, hidden by the default view and revealed by the filter:
    // the group is explicit, so the hide-finished gate steps aside.
    expect(matchesFilters(grouped, EMPTY_FILTERS, NO_CLAIMS)).toBe(false)
    expect(
      matchesFilters(grouped, { ...EMPTY_FILTERS, group: 'design/311-job-inputs' }, NO_CLAIMS),
    ).toBe(true)
  })

  it('composes (AND) with search and state', () => {
    const f = { ...EMPTY_FILTERS, group: 'design/311-job-inputs', q: 'zzz' }
    expect(matchesFilters(grouped, f, NO_CLAIMS)).toBe(false)
    expect(matchesFilters(grouped, { ...f, q: '314' }, NO_CLAIMS)).toBe(true)
    expect(
      matchesFilters(grouped, { ...f, q: '', states: ['Frozen'] }, NO_CLAIMS),
    ).toBe(false)
  })

  it('rides the URL, so a filtered view is shareable', () => {
    const f = { ...EMPTY_FILTERS, group: 'design/311-job-inputs' }
    const params = filtersToParams(f)
    expect(params.get('group')).toBe('design/311-job-inputs')
    expect(filtersFromParams(params)).toEqual(f)
    expect(filtersFromParams(new URLSearchParams()).group).toBe('')
  })

  it('offers every label the loaded rows carry, deduped and sorted', () => {
    const jobs = [
      job({ id: 1, groups: ['design/311-job-inputs', 'beacon-import'] }),
      job({ id: 2, groups: ['beacon-import'] }),
      job({ id: 3 }),
    ]
    expect(groupOptions(jobs)).toEqual(['beacon-import', 'design/311-job-inputs'])
    expect(groupOptions([job({ id: 4 })])).toEqual([])
  })
})
