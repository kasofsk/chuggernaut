import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type DesignEntry } from '../api'
import { ProjectPage } from '../components/ProjectPage'
import { DocMarkdown } from '../components/DocMarkdown'
import { StateBadge, stateColor, stateRank } from '../components/StateBadge'
import { SkeletonLines } from '../components/Skeleton'
import { IconSearch } from '../components/icons'

type SortKey = 'interesting' | 'seq' | 'title' | 'open'
type Lens = 'all' | 'stale' | 'inflight' | 'untouched'

const SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: 'interesting', label: 'Interesting' },
  { key: 'seq', label: 'Number' },
  { key: 'title', label: 'Title' },
  { key: 'open', label: 'Open jobs' },
]

const LENS_OPTIONS: { key: Lens; label: string }[] = [
  { key: 'all', label: 'All designs' },
  { key: 'stale', label: 'Status stale' },
  { key: 'inflight', label: 'In flight' },
  { key: 'untouched', label: 'No jobs' },
]

export function interestRank(d: DesignEntry): number {
  if (d.status_stale) return 0
  if (d.open > 0) return 1
  if (d.jobs.length === 0) return 2
  return 3
}

export function matchesLens(d: DesignEntry, lens: Lens): boolean {
  if (lens === 'stale') return d.status_stale
  if (lens === 'inflight') return d.open > 0
  if (lens === 'untouched') return d.jobs.length === 0
  return true
}

export function matchesQuery(d: DesignEntry, q: string): boolean {
  const needle = q.trim().toLowerCase()
  if (!needle) return true
  return `${d.seq ?? ''} ${d.title} ${d.slug} ${d.status ?? ''}`.toLowerCase().includes(needle)
}

export function compareDesigns(a: DesignEntry, b: DesignEntry, sort: SortKey): number {
  if (sort === 'title') return a.title.localeCompare(b.title)
  if (sort === 'open') return b.open - a.open || (b.seq ?? 0) - (a.seq ?? 0)
  if (sort === 'seq') return (b.seq ?? 0) - (a.seq ?? 0)
  return interestRank(a) - interestRank(b) || (b.seq ?? 0) - (a.seq ?? 0)
}

/**
 * The `Status:` line split into the word an operator scans for and the rest of
 * the document's sentence. The token runs to the first delimiter — period,
 * comma, opening parenthesis or whitespace — because the tree writes all of
 * them: `PROPOSED. Written against…`, `PROPOSED, **amended 2026-07-30**…`
 * (#308), `DRAFT (interactive session…)` (#169). Splitting on the period alone
 * would hand #308 its whole sentence as a badge.
 *
 * This is presentation, not a schema: the platform hands the line over
 * unparsed and compares it to nothing (design #321 Decision 8), and the status
 * vocabulary belongs to job #86. A line that leads with a delimiter has no
 * token, and reads as detail alone rather than as an empty badge.
 */
export function splitStatus(status: string): { token: string; detail: string } {
  const text = status.trim()
  const token = text.match(/^[^\s.,(]+/)?.[0] ?? ''
  return { token, detail: text.slice(token.length).replace(/^[\s.,;:]+/, '') }
}

/** Whether the document's own status line calls the work finished: the
 *  `IMPLEMENTED` token, minus `IMPLEMENTED IN PART`, which shares that token
 *  (`splitStatus` returns the leading word) but which `docs/design-docs.md`
 *  defines as live work with slices still open. */
export function isImplemented(d: DesignEntry): boolean {
  if (!d.status) return false
  const { token, detail } = splitStatus(d.status)
  return token.toUpperCase() === 'IMPLEMENTED' && !/^\W*in part\b/i.test(detail)
}

const STATUS_COLORS: Record<string, string> = {
  PROPOSED: 'blue',
  DRAFT: 'purple',
  FINDING: 'green',
}

export function statusColor(token: string): string {
  return STATUS_COLORS[token.toUpperCase()] ?? 'gray'
}

/** The document's own words about itself, as a badge: the line's leading token,
 *  plus the warning for when its jobs say otherwise. The warning rides against
 *  the badge so the two read as one claim beside the jobs roll-up — "PROPOSED
 *  ⚠ stale · 6/6 done" — rather than as a second unrelated chip. Shared by the
 *  index row and the detail header so the two cannot drift on what "stale"
 *  looks like. */
function DesignStatusBadge({ entry }: { entry: DesignEntry }) {
  if (!entry.status) return <span className="dim">no status line</span>
  const { token } = splitStatus(entry.status)
  if (!token) return <span className="dim">status line has no leading word</span>
  return (
    <span className="design-status-flag">
      <span className={`badge badge-${statusColor(token)}`} title={entry.status}>
        {token}
      </span>
      {entry.status_stale && (
        <span
          className="badge badge-orange design-stale"
          title="every job filed against this design is terminal — at least one of them a job other than the one that wrote the document — and the status line still says otherwise; the repo stays the source of truth, so an amendment job resolves it"
        >
          ⚠ stale
        </span>
      )}
    </span>
  )
}

/** The rest of the status line, muted and literal — never markdown: #308's
 *  remainder carries `**amended 2026-07-30**`, and inline-bolding a fragment
 *  the API already truncated reads worse than showing it as written. It is the
 *  only place the document's own qualification lives, so it is never dropped. */
function DesignStatusDetail({ entry }: { entry: DesignEntry }) {
  const detail = entry.status ? splitStatus(entry.status).detail : ''
  if (!detail) return null
  return <div className="dim design-status-detail">{detail}</div>
}

/** The member jobs as a per-state histogram, in the `StateBadge` colors — so a
 *  state means the same thing here as it does on the jobs table. A design with
 *  no jobs says so rather than rendering an empty row. */
function DesignHistogram({ entry }: { entry: DesignEntry }) {
  if (entry.jobs.length === 0) return <div className="design-hist dim">no jobs filed</div>
  return (
    <div className="design-hist">
      {Object.entries(entry.counts)
        .sort((a, b) => stateRank(a[0]) - stateRank(b[0]))
        .map(([state, n]) => (
          <span key={state} className={`badge badge-${stateColor(state)} hist-pill`}>
            {n} {state}
          </span>
        ))}
      <span className="dim">{entry.open > 0 ? `${entry.open} open` : 'nothing open'}</span>
    </div>
  )
}

/**
 * The designs index: one row per document under `docs/design/`, with its status
 * line, its jobs' states and the staleness flag.
 */
export function DesignsPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [designs, setDesigns] = useState<DesignEntry[]>([])
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [q, setQ] = useState('')
  const [lens, setLens] = useState<Lens>('all')
  const [hideImplemented, setHideImplemented] = useState(false)
  const [sort, setSort] = useState<SortKey>('interesting')

  useEffect(() => {
    setLoaded(false)
    api.designs(owner, project).then(
      (ds) => {
        setDesigns(ds)
        setLoaded(true)
        setError(null)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      },
    )
  }, [owner, project, navigate])

  const matching = designs.filter((d) => matchesLens(d, lens) && matchesQuery(d, q))
  const visible = matching
    .filter((d) => !hideImplemented || !isImplemented(d))
    .sort((a, b) => compareDesigns(a, b, sort))
  const stale = designs.filter((d) => d.status_stale).length
  const hidden = matching.length - visible.length

  return (
    <ProjectPage owner={owner} project={project} error={error}>
      <section className="card jobs-main">
        <div className="jobs-toolbar">
          <div className="jobs-toolbar-title">
            <h2 className="jobs-h1">Designs</h2>
            {loaded && (
              <div className="dim design-count">
                {designs.length} under <code>docs/design/</code>
                {stale > 0 && ` · ${stale} with a stale status`}
                {hidden > 0 && ` · ${hidden} implemented hidden`}
              </div>
            )}
          </div>
          <div className="jobs-controls">
            <div className="search-field">
              <IconSearch />
              <input
                type="search"
                placeholder="Search designs…"
                value={q}
                onChange={(e) => setQ(e.target.value)}
                aria-label="Search designs"
              />
            </div>
            <select
              className="state-filter"
              value={lens}
              onChange={(e) => setLens(e.target.value as Lens)}
              aria-label="Filter designs"
            >
              {LENS_OPTIONS.map((o) => (
                <option key={o.key} value={o.key}>
                  {o.label}
                </option>
              ))}
            </select>
            <label
              className="design-hide-implemented"
              title="hide designs whose status line says IMPLEMENTED — IMPLEMENTED IN PART still has open slices, so it stays"
            >
              <input
                type="checkbox"
                checked={hideImplemented}
                onChange={(e) => setHideImplemented(e.target.checked)}
              />
              hide implemented
            </label>
            <select
              className="sort-select"
              value={sort}
              onChange={(e) => setSort(e.target.value as SortKey)}
              aria-label="Sort designs by"
            >
              {SORT_OPTIONS.map((o) => (
                <option key={o.key} value={o.key}>
                  sort: {o.label}
                </option>
              ))}
            </select>
          </div>
        </div>

        {!loaded && !error && (
          <div className="design-note">
            <SkeletonLines n={8} />
          </div>
        )}
        {loaded && (
          <ul className="design-list">
            {visible.map((d) => (
              <li className="design-row" key={d.path}>
                <div className="design-head">
                  <Link className="design-title" to={`/p/${owner}/${project}/designs/${d.slug}`}>
                    {d.seq != null && <span className="design-seq">#{d.seq} · </span>}
                    {d.title}
                  </Link>
                </div>
                <div className="design-meta">
                  <DesignStatusBadge entry={d} />
                  <DesignHistogram entry={d} />
                </div>
                <DesignStatusDetail entry={d} />
              </li>
            ))}
          </ul>
        )}
        {loaded && visible.length === 0 && !error && (
          <div className="dim design-note">
            {designs.length === 0 ? (
              <>
                no designs yet — a design is a{' '}
                <code>docs/design/&#123;seq&#125;-&#123;slug&#125;.md</code> file on the default
                branch, and a job joins it by carrying the group{' '}
                <code>design/&#123;slug&#125;</code>
              </>
            ) : (
              'no design matches this filter'
            )}
          </div>
        )}
      </section>
    </ProjectPage>
  )
}

/**
 * One design: the document rendered, with its member jobs beside it. Two reads
 * on purpose — the registry carries the roll-up and the path, and the body comes
 * back through the file endpoint, which is what keeps a git blob read off the
 * registry query (design #321 Decision 7).
 */
export function DesignPage() {
  const { owner = '', project = '', slug = '' } = useParams()
  const navigate = useNavigate()
  const [entry, setEntry] = useState<DesignEntry | null>(null)
  const [doc, setDoc] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setEntry(null)
    setDoc(null)
    api
      .designs(owner, project)
      .then((ds) => {
        const found = ds.find((d) => d.slug === slug)
        if (!found) throw new ApiError(404, { error: `${slug}: no such design at default HEAD` })
        setEntry(found)
        setError(null)
        return api.file(owner, project, found.path)
      })
      .then((f) => setDoc(f.content))
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, slug, navigate])

  return (
    <ProjectPage owner={owner} project={project} error={error}>
      <div className="design-layout">
        <section className="card design-doc">
          <div className="row-head">
            <div>
              <h2 className="type-title">{entry?.title ?? slug}</h2>
              <div className="dim type-slug">
                <Link to={`/p/${owner}/${project}/designs`}>designs</Link>
                {' / '}
                {entry ? (
                  <Link to={`/p/${owner}/${project}/files?path=${encodeURIComponent(entry.path)}`}>
                    {entry.path}
                  </Link>
                ) : (
                  slug
                )}
              </div>
            </div>
          </div>
          {entry && (
            <div className="design-status">
              <DesignStatusBadge entry={entry} />
              <DesignStatusDetail entry={entry} />
            </div>
          )}
          {doc && entry ? (
            <DocMarkdown owner={owner} project={project} path={entry.path} text={doc} />
          ) : (
            !error && <SkeletonLines n={10} />
          )}
        </section>

        <section className="card design-jobs">
          <h2>jobs</h2>
          {!entry && !error && <SkeletonLines n={4} />}
          {entry && <DesignHistogram entry={entry} />}
          {entry && (
            <ul className="design-members">
              {entry.jobs.map((j) => (
                <li key={j.id}>
                  <Link to={`/p/${owner}/${project}/jobs/${j.id}`} className="design-member-title">
                    <span className="design-seq">#{j.id}</span> {j.title || '(untitled)'}
                  </Link>
                  <div className="design-member-meta">
                    <span className="dim">{j.type}</span>
                    <StateBadge state={j.state} />
                  </div>
                </li>
              ))}
            </ul>
          )}
          {entry && (
            <div className="dim design-group-name">
              group <code>{entry.name}</code>
            </div>
          )}
        </section>
      </div>
    </ProjectPage>
  )
}
