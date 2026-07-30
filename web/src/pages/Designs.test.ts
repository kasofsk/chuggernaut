import { describe, expect, it } from 'vitest'
import type { DesignEntry } from '../api'
import { compareDesigns, interestRank, matchesLens, matchesQuery } from './Designs'

// The index's default order is the whole point of the view: a design whose jobs
// are all finished while its `Status:` line still says PROPOSED is the row an
// operator has to act on, and it must not sit below eight quiet ones.

function design(over: Partial<DesignEntry> & { slug: string }): DesignEntry {
  return {
    path: `docs/design/${over.slug}.md`,
    seq: null,
    title: over.slug,
    status: 'PROPOSED',
    status_stale: false,
    name: `design/${over.slug}`,
    jobs: [],
    counts: {},
    open: 0,
    ...over,
  }
}

const job = { id: 1, type: 'code', title: 'a job', state: 'Done' as const }

const stale = design({
  slug: '311-inputs', seq: 311, status_stale: true, jobs: [job], counts: { Done: 1 },
})
const inflight = design({
  slug: '321-groups', seq: 321, jobs: [job], counts: { Work: 1 }, open: 1,
})
const untouched = design({ slug: '313-identity', seq: 313 })
// Filed, finished, and honest about it: no status line, so nothing to be stale.
const finished = design({
  slug: '293-capacity', seq: 293, status: null, jobs: [job], counts: { Done: 1 },
})

describe('the designs index', () => {
  it('ranks stale, then in-flight, then untouched, then quiet', () => {
    expect([stale, inflight, untouched, finished].map(interestRank)).toEqual([0, 1, 2, 3])
  })

  it('sorts the interesting rows first, newest seq breaking ties', () => {
    const order = [finished, untouched, inflight, stale]
      .sort((a, b) => compareDesigns(a, b, 'interesting'))
      .map((d) => d.slug)
    expect(order).toEqual(['311-inputs', '321-groups', '313-identity', '293-capacity'])
  })

  it('sorts by seq newest-first, a design with no seq last', () => {
    const noSeq = design({ slug: 'scratch' })
    const order = [noSeq, stale, inflight]
      .sort((a, b) => compareDesigns(a, b, 'seq'))
      .map((d) => d.slug)
    expect(order).toEqual(['321-groups', '311-inputs', 'scratch'])
  })

  it('filters by lens, and a design with no jobs is a row of its own', () => {
    const all = [stale, inflight, untouched, finished]
    const under = (lens: 'stale' | 'inflight' | 'untouched' | 'all') =>
      all.filter((d) => matchesLens(d, lens)).map((d) => d.slug)
    expect(under('stale')).toEqual(['311-inputs'])
    expect(under('inflight')).toEqual(['321-groups'])
    expect(under('untouched')).toEqual(['313-identity'])
    expect(under('all')).toHaveLength(4)
  })

  it('searches the seq, title, slug and status text', () => {
    expect(matchesQuery(stale, '311')).toBe(true)
    expect(matchesQuery(stale, 'INPUTS')).toBe(true)
    expect(matchesQuery(stale, 'proposed')).toBe(true)
    expect(matchesQuery(stale, '  ')).toBe(true)
    expect(matchesQuery(finished, 'proposed')).toBe(false) // no status line to match
  })
})
