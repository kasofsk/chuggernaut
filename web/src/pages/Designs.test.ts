import { describe, expect, it } from 'vitest'
import type { DesignEntry } from '../api'
import {
  compareDesigns,
  interestRank,
  matchesLens,
  matchesQuery,
  splitStatus,
  statusColor,
} from './Designs'

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
    expect(matchesQuery(finished, 'proposed')).toBe(false)
  })
})

describe('the status line split', () => {
  const live: [string, string, string][] = [
    ['169-handoff', 'DRAFT (interactive session 2026-07-24, operator + Claude). Produced', 'DRAFT'],
    ['238-ingest', 'FINDING. Written against the tree at `01624fd`, the parent of the C9', 'FINDING'],
    ['311-inputs', 'PROPOSED. Written against the tree at `acdb2c6`. Every claim about', 'PROPOSED'],
    ['293-capacity', 'PROPOSED. Written against the tree at `a90d660`; every claim about', 'PROPOSED'],
    ['313-identity', 'PROPOSED. Written against the tree at `d7ebfae`. Every claim about', 'PROPOSED'],
    ['310-scheduled', 'PROPOSED. Written against the tree at `55f6595`. Every claim about', 'PROPOSED'],
    ['308-gha', 'PROPOSED, **amended 2026-07-30** (job #320). The original was written', 'PROPOSED'],
    ['321-groups', 'PROPOSED. Written against the tree at `00dd0dc`. Every claim about', 'PROPOSED'],
    ['322-macos', 'PROPOSED. Written against the tree at `61b721d` (2026-07-30). Every', 'PROPOSED'],
    ['309-native', 'PROPOSED. Written against the tree at `b801b76`. Every claim about', 'PROPOSED'],
    ['323-onboarding', 'PROPOSED. Written against the tree at `470cc0c` (2026-07-30). Every', 'PROPOSED'],
  ]

  it('badges the leading word of every live status line', () => {
    for (const [slug, status, token] of live) {
      expect(splitStatus(status).token, slug).toEqual(token)
    }
  })

  it('keeps the whole remainder as detail, losing only the delimiter', () => {
    expect(splitStatus(live[6][1]).detail).toEqual(
      '**amended 2026-07-30** (job #320). The original was written',
    )
    expect(splitStatus(live[0][1]).detail).toEqual(
      '(interactive session 2026-07-24, operator + Claude). Produced',
    )
    expect(splitStatus('PROPOSED').detail).toEqual('')
  })

  it('has no token to badge when the line leads with a delimiter', () => {
    expect(splitStatus('(no idea yet) — see #86')).toEqual({
      token: '',
      detail: '(no idea yet) — see #86',
    })
  })

  it('gives an unrecognized token a neutral badge rather than dropping it', () => {
    expect(statusColor('PROPOSED')).toEqual('blue')
    expect(statusColor('draft')).toEqual('purple')
    expect(splitStatus('draft. lowercase').token).toEqual('draft')
    expect(statusColor('SUPERSEDED')).toEqual('gray')
    expect(statusColor('¯\\_(ツ)_/¯')).toEqual('gray')
  })
})
