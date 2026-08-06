import { describe, expect, it } from 'vitest'
import type { Job } from './api'
import { groupHref, groupNameError } from './groups'
import {
  EMPTY_FILTERS,
  filtersFromParams,
  filtersToParams,
  groupOptions,
  matchesFilters,
  stateSelectFilters,
  stateSelectValue,
  STATE_ALL,
  STATE_MULTI,
} from './jobFilters'

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

describe('the state filter', () => {
  const done = job({ id: 400, state: 'Done' })
  const working = job({ id: 401, state: 'Work' })

  it('shows every job under "All", finished included', () => {
    const f = stateSelectFilters(EMPTY_FILTERS, STATE_ALL)
    expect(matchesFilters(done, f, NO_CLAIMS)).toBe(true)
    expect(matchesFilters(working, f, NO_CLAIMS)).toBe(true)
    expect(matchesFilters(done, EMPTY_FILTERS, NO_CLAIMS)).toBe(false)
  })

  it('composes (AND) with search, so "All" is not an escape hatch', () => {
    const f = { ...stateSelectFilters(EMPTY_FILTERS, STATE_ALL), q: '401' }
    expect(matchesFilters(done, f, NO_CLAIMS)).toBe(false)
    expect(matchesFilters(working, f, NO_CLAIMS)).toBe(true)
  })

  it('returns to the active-only default, and to one named state', () => {
    const all = stateSelectFilters(EMPTY_FILTERS, STATE_ALL)
    expect(stateSelectFilters(all, '')).toEqual(EMPTY_FILTERS)
    expect(stateSelectFilters(all, 'Work')).toEqual({ ...EMPTY_FILTERS, states: ['Work'] })
  })

  it('rides the URL, and reads back as the dropdown position', () => {
    const all = stateSelectFilters(EMPTY_FILTERS, STATE_ALL)
    expect(filtersToParams(all).get('finished')).toBe('1')
    expect(stateSelectValue(filtersFromParams(filtersToParams(all)))).toBe(STATE_ALL)
    expect(stateSelectValue(EMPTY_FILTERS)).toBe('')
    expect(stateSelectValue({ ...EMPTY_FILTERS, states: ['Work'] })).toBe('Work')
    expect(stateSelectValue({ ...EMPTY_FILTERS, states: ['Work', 'Done'] })).toBe(STATE_MULTI)
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
