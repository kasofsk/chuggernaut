import { useEffect, useState, type FormEvent } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { ApiError, api, type Evaluator, type Job, type JobTypeDetail, type JobTypeSummary } from '../api'
import { ProjectTabs } from '../components/ProjectTabs'

/**
 * Create-job page: writing the ticket. Fetches the type library, the tag
 * vocabulary, and existing jobs (for the dependency picker), then navigates
 * to the new job's detail page on success. `?type=` preselects a job type
 * (linked from the Library).
 */
export function NewJobPage() {
  const { owner = '', project = '' } = useParams()
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const [jobTypes, setJobTypes] = useState<JobTypeSummary[]>([])
  const [availableTags, setAvailableTags] = useState<string[]>([])
  const [jobs, setJobs] = useState<Job[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    Promise.all([api.jobTypes(owner, project), api.tags(owner, project), api.jobs(owner, project)])
      .then(([types, tags, js]) => {
        setJobTypes(types)
        setAvailableTags(tags)
        setJobs(js)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, navigate])

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <Link to={`/p/${owner}/${project}`}>
          {owner}/{project}
        </Link>
        <h1>New job</h1>
      </header>
      <ProjectTabs owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      <section className="card">
        <CreateJob
          owner={owner}
          project={project}
          jobTypes={jobTypes}
          availableTags={availableTags}
          jobs={jobs}
          initialType={params.get('type') ?? ''}
          onCreate={(type, title, description, deps, knowledgeTags, evals) =>
            api
              .createJob(owner, project, {
                type,
                title: title || undefined,
                description: description || undefined,
                deps: deps.length ? deps : undefined,
                knowledge_tags: knowledgeTags.length ? knowledgeTags : undefined,
                eval: evals.length ? evals : undefined,
              })
              .then(
                (job) => navigate(`/p/${owner}/${project}/jobs/${job.id}`),
                (e) => setError(e instanceof Error ? e.message : 'create failed'),
              )
          }
        />
      </section>
    </div>
  )
}

// One per-job evaluator being composed in the create form. Command evaluators
// take a shell command; agent/human take a prompt file path (e.g. an
// instructions.md in the repo).
type EvalRow = { name: string; type: Evaluator['type']; action: string; required: boolean }

function CreateJob({
  owner,
  project,
  jobTypes,
  availableTags,
  jobs,
  initialType,
  onCreate,
}: {
  owner: string
  project: string
  jobTypes: JobTypeSummary[]
  /** tags/*.md stems at default HEAD — the repo-versioned tag vocabulary */
  availableTags: string[]
  /** existing jobs, for the dependency picker */
  jobs: Job[]
  /** preselected type (from ?type=; empty = first available) */
  initialType: string
  onCreate: (
    type: string,
    title: string,
    description: string,
    deps: number[],
    knowledgeTags: string[],
    evals: Evaluator[],
  ) => void
}) {
  const [type, setType] = useState(initialType)
  const [typeQuery, setTypeQuery] = useState('')
  // Default selection once the list loads: ?type= wins, then "Feature",
  // then the first available type. Runs only until a selection exists;
  // searching never clears the selection, so this cannot fight the input.
  useEffect(() => {
    if (type || !jobTypes.length) return
    const feature = jobTypes.find((t) => /feature/i.test(t.display_name) || /feature/i.test(t.name))
    const pick = jobTypes.find((t) => t.name === initialType) ?? feature ?? jobTypes[0]
    setType(pick.name)
  }, [jobTypes, type, initialType])

  // Full definition of the selected type — for the prompt links below the
  // picker (what the work agent and each evaluator will actually receive).
  const [typeDetail, setTypeDetail] = useState<JobTypeDetail | null>(null)
  useEffect(() => {
    if (!type) {
      setTypeDetail(null)
      return
    }
    api.jobType(owner, project, type).then(setTypeDetail, () => setTypeDetail(null))
  }, [owner, project, type])

  const typeMatches = jobTypes.filter((t) => {
    const q = typeQuery.trim().toLowerCase()
    if (!q) return true
    return (
      t.display_name.toLowerCase().includes(q) ||
      t.name.toLowerCase().includes(q) ||
      t.description.toLowerCase().includes(q)
    )
  })
  const [title, setTitle] = useState('')
  const [description, setDescription] = useState('')
  const [deps, setDeps] = useState<number[]>([])
  const [depQuery, setDepQuery] = useState('')
  const [evalRows, setEvalRows] = useState<EvalRow[]>([])
  const [selectedTags, setSelectedTags] = useState<string[]>([])
  const [tags, setTags] = useState('')

  function toggleTag(tag: string) {
    setSelectedTags((ts) => (ts.includes(tag) ? ts.filter((t) => t !== tag) : [...ts, tag]))
  }
  const [error, setError] = useState<string | null>(null)

  // Valid dependency targets: any existing non-revoked job. A job being
  // created can never close a cycle — nothing depends on it yet.
  const depMatches = jobs
    .filter((j) => j.state !== 'Revoked' && !deps.includes(j.id))
    .filter((j) => {
      const q = depQuery.trim().toLowerCase()
      if (!q) return true
      return (
        `#${j.id}`.includes(q) ||
        String(j.id).startsWith(q.replace('#', '')) ||
        j.title.toLowerCase().includes(q) ||
        j.type.toLowerCase().includes(q)
      )
    })
    .slice(0, 8)
  function setEvalRow(i: number, patch: Partial<EvalRow>) {
    setEvalRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)))
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    const evals: Evaluator[] = []
    for (const r of evalRows) {
      const name = r.name.trim()
      const action = r.action.trim()
      if (!name && !action) continue
      if (!name || !action) {
        setError(`evaluator needs both a name and a ${r.type === 'command' ? 'command' : 'prompt path'}`)
        return
      }
      evals.push({
        name,
        type: r.type,
        run: r.type === 'command' ? action : undefined,
        prompt: r.type !== 'command' ? action : undefined,
        required: r.required ? undefined : false, // omit = required (the default)
      })
    }
    const extra = tags
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean)
    const knowledgeTags = [...selectedTags, ...extra.filter((t) => !selectedTags.includes(t))]
    if (!type) {
      setError('pick a job type')
      return
    }
    setError(null)
    onCreate(type, title.trim(), description.trim(), deps, knowledgeTags, evals)
  }

  return (
    <form className="create-job" onSubmit={submit}>
      <label className="field">
        <span>Title <span className="dim">(what this run is for)</span></span>
        <input value={title} onChange={(e) => setTitle(e.target.value)} autoFocus />
      </label>

      <label className="field">
        <span>Description <span className="dim">(the ticket — injected into the work and eval prompts)</span></span>
        <textarea rows={5} value={description} onChange={(e) => setDescription(e.target.value)} />
      </label>

      <div className="field">
        <span>Job type</span>
        {jobTypes.length > 3 && (
          <input
            placeholder="filter…"
            value={typeQuery}
            onChange={(e) => setTypeQuery(e.target.value)}
          />
        )}
        <div className="dep-suggestions">
          {typeMatches.map((t) => (
            <div className="option-row" key={t.name}>
              <button
                type="button"
                className={t.name === type ? 'dep-suggestion option-on' : 'dep-suggestion'}
                onClick={() => setType(t.name)}
              >
                <b>{t.display_name}</b>
                {t.description && <span className="dim"> — {t.description}</span>}
                {t.display_name !== t.name && <span className="dim"> · {t.name}</span>}
              </button>
              <a
                className="option-peek"
                href={`/p/${owner}/${project}/job-types/${encodeURIComponent(t.name)}`}
                target="_blank"
                rel="noreferrer"
                title={`see what ${t.display_name} does (library, new tab)`}
              >
                ↗
              </a>
            </div>
          ))}
          {typeMatches.length === 0 && <div className="dim">no matching job types</div>}
        </div>
        {typeDetail?.job_type && (
          <div className="dim prompt-links">
            {typeDetail.job_type.work.prompt && (
              <span>
                work prompt:{' '}
                <a
                  href={`/p/${owner}/${project}/files?path=${encodeURIComponent(typeDetail.job_type.work.prompt)}`}
                  target="_blank"
                  rel="noreferrer"
                >
                  {typeDetail.job_type.work.prompt} ↗
                </a>
              </span>
            )}
            {typeDetail.job_type.work.run && (
              <span>
                work command: <code>{typeDetail.job_type.work.run}</code>
              </span>
            )}
            {typeDetail.job_type.eval.map((e) => (
              <span key={e.name}>
                {e.name}:{' '}
                {e.prompt ? (
                  <a
                    href={`/p/${owner}/${project}/files?path=${encodeURIComponent(e.prompt)}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {e.prompt} ↗
                  </a>
                ) : (
                  <code>{e.run}</code>
                )}
              </span>
            ))}
          </div>
        )}
      </div>

      <div className="field">
        <span>Depends on <span className="dim">(jobs that must finish first)</span></span>
        {deps.length > 0 && (
          <div className="tag-row">
            {deps.map((d) => {
              const j = jobs.find((x) => x.id === d)
              return (
                <button
                  type="button"
                  key={d}
                  className="tag tag-on"
                  title="remove"
                  onClick={() => setDeps((ds) => ds.filter((x) => x !== d))}
                >
                  #{d}{j?.title ? ` ${j.title}` : j ? ` ${j.type}` : ''} ×
                </button>
              )
            })}
          </div>
        )}
        <input
          placeholder="search jobs by #, title, or type…"
          value={depQuery}
          onChange={(e) => setDepQuery(e.target.value)}
        />
        {depQuery.trim() !== '' && (
          <div className="dep-suggestions">
            {depMatches.map((j) => (
              <button
                type="button"
                key={j.id}
                className="dep-suggestion"
                onClick={() => {
                  setDeps((ds) => [...ds, j.id])
                  setDepQuery('')
                }}
              >
                #{j.id} <b>{j.title || j.type}</b>{' '}
                <span className="dim">
                  {j.title ? j.type : ''} · {j.state}
                </span>
              </button>
            ))}
            {depMatches.length === 0 && <div className="dim">no matching jobs</div>}
          </div>
        )}
      </div>

      <div className="field">
        <span>
          Extra evaluation criteria <span className="dim">(optional; added on top of the type's)</span>
        </span>
        {evalRows.map((r, i) => (
          <div className="kv-row" key={i}>
            <input
              className="kv-key"
              placeholder="name"
              value={r.name}
              onChange={(e) => setEvalRow(i, { name: e.target.value })}
            />
            <select
              value={r.type}
              onChange={(e) => setEvalRow(i, { type: e.target.value as EvalRow['type'] })}
            >
              <option value="command">command</option>
              <option value="agent">agent</option>
              <option value="human">human</option>
            </select>
            <input
              className="kv-val"
              placeholder={r.type === 'command' ? 'shell command, e.g. ./ci.sh' : 'prompt path, e.g. evals/instructions.md'}
              value={r.action}
              onChange={(e) => setEvalRow(i, { action: e.target.value })}
            />
            <label className="dim" title="unchecked = advisory (doesn't gate)">
              <input
                type="checkbox"
                checked={r.required}
                onChange={(e) => setEvalRow(i, { required: e.target.checked })}
              />{' '}
              req
            </label>
            <button
              type="button"
              className="kv-del"
              title="remove"
              onClick={() => setEvalRows((rs) => rs.filter((_, j) => j !== i))}
            >
              ×
            </button>
          </div>
        ))}
        <button
          type="button"
          className="link"
          onClick={() =>
            setEvalRows((rs) => [...rs, { name: '', type: 'command', action: '', required: true }])
          }
        >
          + add evaluator
        </button>
      </div>

      <div className="field">
        <span>
          Knowledge tags{' '}
          <span className="dim">(optional; a tag's meaning lives in tags/&#123;tag&#125;.md)</span>
        </span>
        {availableTags.length > 0 && (
          <div className="tag-row">
            {availableTags.map((t) => (
              <button
                type="button"
                key={t}
                className={selectedTags.includes(t) ? 'tag tag-on' : 'tag'}
                onClick={() => toggleTag(t)}
              >
                {t}
              </button>
            ))}
          </div>
        )}
        <input
          list="tag-suggestions"
          placeholder={availableTags.length ? 'extra tags — comma, separated' : 'comma, separated'}
          value={tags}
          onChange={(e) => setTags(e.target.value)}
        />
        <datalist id="tag-suggestions">
          {availableTags
            .filter((t) => !selectedTags.includes(t))
            .map((t) => (
              <option key={t} value={t} />
            ))}
        </datalist>
      </div>

      <div className="create-actions">
        <button type="submit">create job</button>
      </div>
      {error && <div className="error">{error}</div>}
    </form>
  )
}
