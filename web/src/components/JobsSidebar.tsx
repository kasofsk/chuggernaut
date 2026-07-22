import { useState } from 'react'
import type { Job, JobState } from '../api'
import {
  activityBuckets,
  countQuick,
  countStates,
  EMPTY_FILTERS,
  filtersToParams,
  OVERVIEW_GROUPS,
  type JobFilters,
  type QuickKey,
} from '../jobFilters'
import { Sparkline } from './Sparkline'
import { IconFilter } from './icons'

const SAVED_KEY = 'chug-saved-filters'
type SavedFilter = { name: string; q: string }

function loadSaved(): SavedFilter[] {
  try {
    const raw = localStorage.getItem(SAVED_KEY)
    const v = raw ? JSON.parse(raw) : []
    return Array.isArray(v) ? v : []
  } catch {
    return []
  }
}

const QUICK: { key: QuickKey; label: string }[] = [
  { key: 'mine', label: 'My jobs' },
  { key: 'waiting', label: 'Waiting on me' },
  { key: 'failed', label: 'Failed' },
  { key: 'attention', label: 'Needs attention' },
  { key: 'recent', label: 'Recent runs' },
]

/**
 * The left content column (#162): JOBS OVERVIEW (total + activity sparkline +
 * per-state one-click filters) and QUICK FILTERS (show-finished, saved filter
 * combos). Collapses above the table on narrow viewports (styles.css).
 */
export function JobsSidebar({
  jobs,
  claimed,
  filters,
  setFilters,
}: {
  jobs: Job[]
  claimed: Set<number>
  filters: JobFilters
  setFilters: (f: JobFilters) => void
}) {
  const [saved, setSaved] = useState<SavedFilter[]>(loadSaved)

  const total = jobs.length
  const spark = activityBuckets(jobs)

  // Toggle an overview group as the active state filter (exact set match toggles off).
  const groupActive = (states: JobState[]) =>
    states.length === filters.states.length && states.every((s) => filters.states.includes(s))
  const toggleGroup = (states: JobState[]) =>
    setFilters({ ...filters, states: groupActive(states) ? [] : states })

  const toggleQuick = (k: QuickKey) =>
    setFilters({
      ...filters,
      quick: filters.quick.includes(k) ? filters.quick.filter((x) => x !== k) : [...filters.quick, k],
    })

  const persist = (list: SavedFilter[]) => {
    setSaved(list)
    localStorage.setItem(SAVED_KEY, JSON.stringify(list))
  }
  const saveCurrent = () => {
    const name = window.prompt('Name this filter view')?.trim()
    if (!name) return
    // Round-trip the whole active combination as a query string.
    persist([...saved.filter((s) => s.name !== name), { name, q: filtersToParams(filters).toString() }])
  }

  return (
    <aside className="jobs-side">
      <section className="card side-card">
        <h2>Jobs overview</h2>
        <div className="ov-spark">
          <Sparkline data={spark} width={200} height={40} />
        </div>
        <div className="ov-total">
          <span className="ov-total-n">{total}</span>
          <span className="ov-total-label">Total jobs</span>
        </div>
        <ul className="ov-states">
          {OVERVIEW_GROUPS.map((g) => {
            const n = countStates(jobs, g.states)
            return (
              <li key={g.key}>
                <button
                  type="button"
                  className={`ov-state${groupActive(g.states) ? ' ov-state-on' : ''}`}
                  onClick={() => toggleGroup(g.states)}
                  title={`filter to ${g.label}`}
                >
                  <span className={`state-dot dot-${g.color}`} />
                  <span className="ov-state-label">{g.label}</span>
                  <span className="ov-state-n">{n}</span>
                </button>
              </li>
            )
          })}
        </ul>
      </section>

      <section className="card side-card">
        <div className="side-head">
          <h2>Quick filters</h2>
          <IconFilter />
        </div>
        <div className="qf-toggle">
          <span>Show</span>
          <select
            value={filters.showFinished ? 'all' : 'active'}
            onChange={(e) => setFilters({ ...filters, showFinished: e.target.value === 'all' })}
          >
            <option value="active">Active</option>
            <option value="all">Finished</option>
          </select>
        </div>
        <ul className="qf-list">
          {QUICK.map((q) => {
            const n = countQuick(jobs, q.key, claimed)
            return (
              <li key={q.key}>
                <button
                  type="button"
                  className={`qf-item${filters.quick.includes(q.key) ? ' qf-item-on' : ''}`}
                  onClick={() => toggleQuick(q.key)}
                >
                  <span className="qf-label">{q.label}</span>
                  {n > 0 && <span className="qf-n">{n}</span>}
                </button>
              </li>
            )
          })}
        </ul>
        {saved.length > 0 && (
          <ul className="qf-saved">
            {saved.map((s) => (
              <li key={s.name}>
                <button
                  type="button"
                  className="qf-saved-item"
                  onClick={() => {
                    const p = new URLSearchParams(s.q)
                    setFilters({
                      q: p.get('q') ?? '',
                      states: (p.get('state') ?? '').split(',').filter(Boolean) as JobState[],
                      quick: (p.get('quick') ?? '').split(',').filter(Boolean) as QuickKey[],
                      showFinished: p.get('finished') === '1',
                    })
                  }}
                >
                  ★ {s.name}
                </button>
                <button
                  type="button"
                  className="qf-saved-del"
                  title="delete saved filter"
                  onClick={() => persist(saved.filter((x) => x.name !== s.name))}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="qf-actions">
          <button type="button" className="qf-save" onClick={saveCurrent}>
            + New saved filter
          </button>
          <button type="button" className="link qf-clear" onClick={() => setFilters(EMPTY_FILTERS)}>
            clear
          </button>
        </div>
      </section>
    </aside>
  )
}
