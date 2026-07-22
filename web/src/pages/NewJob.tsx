import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'
import {
  ApiError,
  api,
  type Evaluator,
  type Job,
  type JobTypeDetail,
  type JobTypeSummary,
} from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { RichSelect } from '../components/RichSelect'
import { TicketStub } from '../components/TicketStub'

const reducedMotion = () =>
  typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches

// A stable little glyph per job type so the type cards read as distinct at a
// glance (purely decorative — derived from the name, no config needed).
const GLYPHS = ['⚙️', '🧩', '🚀', '🛠️', '🔭', '📦', '🧪', '⚡', '🌐', '🧭']
function glyphFor(name: string): string {
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0
  return GLYPHS[h % GLYPHS.length]
}

/**
 * Create-job page as a departure (#171): a form beside a live train-ticket stub
 * that materialises the job as you compose it, type cards instead of a plain
 * select, deps rendered as coupled cars, and a track-switch for the draft/
 * release choice. `?type=` preselects a job type (linked from the Library).
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
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      <CreateJob
        owner={owner}
        project={project}
        jobTypes={jobTypes}
        availableTags={availableTags}
        jobs={jobs}
        initialType={params.get('type') ?? ''}
        onError={setError}
      />
    </div>
  )
}

type EvalRow = { name: string; type: Evaluator['type']; action: string; required: boolean }

function CreateJob({
  owner,
  project,
  jobTypes,
  availableTags,
  jobs,
  initialType,
  onError,
}: {
  owner: string
  project: string
  jobTypes: JobTypeSummary[]
  availableTags: string[]
  jobs: Job[]
  initialType: string
  onError: (msg: string) => void
}) {
  const navigate = useNavigate()
  const [type, setType] = useState(initialType)
  useEffect(() => {
    if (type || !jobTypes.length) return
    const feature = jobTypes.find((t) => /feature/i.test(t.display_name) || /feature/i.test(t.name))
    const pick = jobTypes.find((t) => t.name === initialType) ?? feature ?? jobTypes[0]
    setType(pick.name)
  }, [jobTypes, type, initialType])

  const [typeDetail, setTypeDetail] = useState<JobTypeDetail | null>(null)
  useEffect(() => {
    if (!type) {
      setTypeDetail(null)
      return
    }
    api.jobType(owner, project, type).then(setTypeDetail, () => setTypeDetail(null))
  }, [owner, project, type])

  const [title, setTitle] = useState('')
  const [description, setDescription] = useState('')
  const [deps, setDeps] = useState<number[]>([])
  const [depQuery, setDepQuery] = useState('')
  const [evalRows, setEvalRows] = useState<EvalRow[]>([])
  const [selectedTags, setSelectedTags] = useState<string[]>([])
  const [tags, setTags] = useState('')
  const [timeout, setTimeout] = useState('')
  const typeTimeout = typeDetail?.job_type?.resources?.task_timeout ?? null
  const [model, setModel] = useState('')
  const typeModel = typeDetail?.job_type?.work?.model ?? null
  // Departure switch: 'release' schedules the run; 'draft' parks it on the siding.
  const [mode, setMode] = useState<'release' | 'draft'>('release')
  // Create-time flourish for the ticket stub.
  const [anim, setAnim] = useState<'none' | 'depart' | 'siding'>('none')
  const [error, setError] = useState<string | null>(null)
  const busy = useRef(false)

  function toggleTag(tag: string) {
    setSelectedTags((ts) => (ts.includes(tag) ? ts.filter((t) => t !== tag) : [...ts, tag]))
  }

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

  const extraTags = tags
    .split(',')
    .map((t) => t.trim())
    .filter(Boolean)
  const allTags = [...selectedTags, ...extraTags.filter((t) => !selectedTags.includes(t))]
  const typeLabel = jobTypes.find((t) => t.name === type)?.display_name

  function build(draft: boolean) {
    if (busy.current) return
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
        required: r.required ? undefined : false,
      })
    }
    if (!type) {
      setError('pick a job type')
      return
    }
    setError(null)
    busy.current = true
    api
      .createJob(owner, project, {
        type,
        title: title.trim() || undefined,
        description: description.trim() || undefined,
        deps: deps.length ? deps : undefined,
        knowledge_tags: allTags.length ? allTags : undefined,
        eval: evals.length ? evals : undefined,
        timeout: timeout.trim() || undefined,
        model: model.trim() || undefined,
        draft: draft || undefined,
      })
      .then(
        (job) => {
          // Punch + depart (release) / slide to siding (draft), then route.
          const go = () => navigate(`/p/${owner}/${project}/jobs/${job.id}`)
          if (reducedMotion()) return go()
          setAnim(draft ? 'siding' : 'depart')
          window.setTimeout(go, 600)
        },
        (e) => {
          busy.current = false
          const msg = e instanceof Error ? e.message : 'create failed'
          setError(msg)
          onError(msg)
        },
      )
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    build(mode === 'draft')
  }
  // ⌘/Ctrl-Enter submits per the current switch position.
  function onKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault()
      build(mode === 'draft')
    }
  }

  return (
    <div className="newjob-layout">
      <form className="create-job card" onSubmit={submit} onKeyDown={onKey}>
        <label className="field">
          <span>Title <span className="dim">(what this run is for)</span></span>
          <input value={title} onChange={(e) => setTitle(e.target.value)} />
        </label>

        <label className="field">
          <span>Description <span className="dim">(the ticket — injected into the work and eval prompts)</span></span>
          <textarea rows={5} value={description} onChange={(e) => setDescription(e.target.value)} />
        </label>

        <div className="field">
          <span>Job type</span>
          <div className="type-cards">
            {jobTypes.map((t) => (
              <button
                type="button"
                key={t.name}
                className={`type-card${t.name === type ? ' type-card-on' : ''}`}
                aria-pressed={t.name === type}
                onClick={() => setType(t.name)}
              >
                <span className="type-card-glyph">{glyphFor(t.name)}</span>
                <span className="type-card-name">{t.display_name}</span>
                {t.description && <span className="type-card-desc">{t.description}</span>}
              </button>
            ))}
          </div>
          {/* RichSelect stays under the hood for keyboard/a11y (#171). */}
          <details className="type-list-fallback">
            <summary>Pick from a list</summary>
            <div className="type-select">
              <RichSelect
                value={type}
                onChange={setType}
                placeholder="pick a job type…"
                options={jobTypes.map((t) => ({
                  value: t.name,
                  label: t.display_name,
                  description: t.description || undefined,
                  detail: t.display_name !== t.name ? t.name : undefined,
                }))}
              />
            </div>
          </details>
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
          <span>Depends on <span className="dim">(jobs that must finish first — coupled cars)</span></span>
          {deps.length > 0 && (
            <div className="coupled">
              <span className="coupled-loco" aria-hidden="true">🚂</span>
              {deps.map((d) => {
                const j = jobs.find((x) => x.id === d)
                return (
                  <span key={d} className="car-wrap">
                    <span className="car-coupler" aria-hidden="true">–</span>
                    <button
                      type="button"
                      className="car"
                      title={`uncouple${j?.title ? ` — ${j.title}` : ''}`}
                      onClick={() => setDeps((ds) => ds.filter((x) => x !== d))}
                    >
                      #{d} ×
                    </button>
                  </span>
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

        <label className="field">
          <span>
            Work timeout{' '}
            <span className="dim">
              (optional; overrides the type default for this job's work — evaluators keep the default)
            </span>
          </span>
          <input
            value={timeout}
            onChange={(e) => setTimeout(e.target.value)}
            placeholder={typeTimeout ? `${typeTimeout} (type default)` : 'e.g. 45m, 2h, 1h30m'}
          />
        </label>

        <label className="field">
          <span>
            Work model{' '}
            <span className="dim">
              (optional; overrides the type, project and platform defaults for this job's work agent)
            </span>
          </span>
          <input
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder={typeModel ? `${typeModel} (default)` : 'e.g. claude-opus-4-8, claude-fable-5'}
          />
        </label>

        <div className="departure">
          <div
            className="switch"
            data-mode={mode}
            role="switch"
            aria-checked={mode === 'release'}
            tabIndex={0}
            title="park as an editable Draft, or release to the dispatcher"
            onClick={() => setMode((m) => (m === 'release' ? 'draft' : 'release'))}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                setMode((m) => (m === 'release' ? 'draft' : 'release'))
              }
            }}
          >
            <span className="switch-knob" />
            <span className="switch-opt">Park on siding</span>
            <span className="switch-opt">Release</span>
          </div>
          <button type="submit" className="btn-primary-glow" disabled={busy.current}>
            {mode === 'draft' ? 'Park on siding' : 'Release job'} <span className="dim">⌘⏎</span>
          </button>
        </div>
        {error && <div className="error">{error}</div>}
      </form>

      <div className="ticket-col">
        <TicketStub
          anim={anim}
          data={{
            owner,
            project,
            title: title.trim(),
            typeLabel,
            deps,
            evals: evalRows.map((r) => r.name.trim()).filter(Boolean),
            tags: allTags,
            destination: mode === 'draft' ? 'siding' : 'release',
          }}
        />
      </div>
    </div>
  )
}
