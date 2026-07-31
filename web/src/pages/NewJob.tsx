import { useEffect, useState, type FormEvent, type KeyboardEvent } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'
import {
  ApiError,
  api,
  type Evaluator,
  type EvaluatorInput,
  type Job,
  type JobTypeSummary,
} from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { RichSelect } from '../components/RichSelect'
import { SkeletonLines } from '../components/Skeleton'
import { AttachmentComposer, uploadFiles } from '../components/Attachments'
import {
  JobInputFields,
  inputFieldErrors,
  inputValueErrors,
  suppliedInputs,
  useJobTypeDetail,
} from '../components/JobInputs'
import { GroupPicker, useGroupOptions } from '../components/JobGroups'
import { depCandidates } from '../jobFilters'

/**
 * Create-job page (#171): the compose form; deps render as coupled cars, and a
 * Draft/Frozen/Ready selector picks the initial state. `?type=` preselects a
 * job type (linked from the Library).
 */
export function NewJobPage() {
  const { owner = '', project = '' } = useParams()
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const [jobTypes, setJobTypes] = useState<JobTypeSummary[]>([])
  const [availableTags, setAvailableTags] = useState<string[]>([])
  const [jobs, setJobs] = useState<Job[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setLoading(true)
    Promise.all([api.jobTypes(owner, project), api.tags(owner, project), api.jobs(owner, project)])
      .then(([types, tags, js]) => {
        setJobTypes(types)
        setAvailableTags(tags.map((t) => t.name))
        setJobs(js)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
      .finally(() => setLoading(false))
  }, [owner, project, navigate])

  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      <CreateJob
        owner={owner}
        project={project}
        loading={loading}
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

/** Departure switch position — the state the job is created in. */
type JobMode = 'draft' | 'frozen' | 'ready'

/**
 * Submit-button label. Once tapped it reports the in-flight create rather than
 * the mode: the button and the mode selector are both frozen for the duration,
 * so repeating the mode there would say nothing the selector isn't showing.
 */
export function createJobSubmitLabel(mode: JobMode, submitting: boolean): string {
  return submitting ? 'Creating…' : `Create ${mode}`
}

function CreateJob({
  owner,
  project,
  loading,
  jobTypes,
  availableTags,
  jobs,
  initialType,
  onError,
}: {
  owner: string
  project: string
  /** pickers fetch still in flight — skeleton the type cards / dep search */
  loading: boolean
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

  const typeDetail = useJobTypeDetail(owner, project, type)
  const declaredInputs = typeDetail?.job_type?.inputs ?? []
  const [inputValues, setInputValues] = useState<Record<string, string>>({})
  const [inputErrors, setInputErrors] = useState<Record<string, string>>({})
  useEffect(() => {
    setInputValues({})
    setInputErrors({})
  }, [type])

  const [title, setTitle] = useState('')
  const [description, setDescription] = useState('')
  const [deps, setDeps] = useState<number[]>([])
  const [depQuery, setDepQuery] = useState('')
  const [evalRows, setEvalRows] = useState<EvalRow[]>([])
  const [selectedTags, setSelectedTags] = useState<string[]>([])
  const [tags, setTags] = useState('')
  const [groups, setGroups] = useState<string[]>([])
  const groupChoices = useGroupOptions(owner, project)
  const [timeout, setTimeout] = useState('')
  const [files, setFiles] = useState<File[]>([])
  const typeTimeout = typeDetail?.job_type?.resources?.task_timeout ?? null
  const [model, setModel] = useState('')
  const typeModel = typeDetail?.job_type?.work?.model ?? null
  const [mode, setMode] = useState<JobMode>('frozen')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  function toggleTag(tag: string) {
    setSelectedTags((ts) => (ts.includes(tag) ? ts.filter((t) => t !== tag) : [...ts, tag]))
  }

  const depMatches = depCandidates(jobs, deps, depQuery)
  function setEvalRow(i: number, patch: Partial<EvalRow>) {
    setEvalRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)))
  }

  const extraTags = tags
    .split(',')
    .map((t) => t.trim())
    .filter(Boolean)
  const allTags = [...selectedTags, ...extraTags.filter((t) => !selectedTags.includes(t))]

  function build(draft: boolean) {
    if (submitting) return
    const evals: EvaluatorInput[] = []
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
    if (Object.keys(inputValueErrors(declaredInputs, inputValues)).length) {
      setInputErrors({})
      setError('fix the flagged inputs above')
      return
    }
    setError(null)
    setSubmitting(true)
    api
      .createJob(owner, project, {
        type,
        title: title.trim() || undefined,
        description: description.trim() || undefined,
        deps: deps.length ? deps : undefined,
        knowledge_tags: allTags.length ? allTags : undefined,
        groups: groups.length ? groups : undefined,
        eval: evals.length ? evals : undefined,
        timeout: timeout.trim() || undefined,
        model: model.trim() || undefined,
        inputs: suppliedInputs(declaredInputs, inputValues),
        draft: draft || undefined,
      })
      .then(
        async (job) => {
          if (files.length) {
            const failed = await uploadFiles(owner, project, job.id, files)
            if (failed.length)
              onError(`some attachments didn't upload (${failed.join(', ')}) — retry on the job page`)
          }
          if (mode === 'ready') await api.release(owner, project, job.id).catch(() => {})
          navigate(`/p/${owner}/${project}/jobs/${job.id}`)
        },
        (e) => {
          setSubmitting(false)
          const msg = e instanceof Error ? e.message : 'create failed'
          const fields = inputFieldErrors(e)
          setInputErrors(fields)
          if (Object.keys(fields).length) {
            setError('the dispatcher rejected these inputs')
            return
          }
          setError(msg)
          onError(msg)
        },
      )
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    build(mode === 'draft')
  }
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

        <AttachmentComposer files={files} onChange={setFiles} />

        <div className="field">
          <span>Job type</span>
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
                  work command:{' '}
                  <a
                    href={`/p/${owner}/${project}/files?path=${encodeURIComponent(typeDetail.job_type.work.run)}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {typeDetail.job_type.work.run} ↗
                  </a>
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
                  ) : e.run ? (
                    <a
                      href={`/p/${owner}/${project}/files?path=${encodeURIComponent(e.run)}`}
                      target="_blank"
                      rel="noreferrer"
                    >
                      {e.run} ↗
                    </a>
                  ) : null}
                </span>
              ))}
            </div>
          )}
        </div>

        <JobInputFields
          declared={declaredInputs}
          values={inputValues}
          serverErrors={inputErrors}
          onChange={(name, value) => {
            setInputValues((vs) => ({ ...vs, [name]: value }))
            setInputErrors(({ [name]: _dropped, ...rest }) => rest)
          }}
        />

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
              {loading && depMatches.length === 0 && <SkeletonLines n={2} />}
              {!loading && depMatches.length === 0 && <div className="dim">no matching jobs</div>}
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
            <span className="dim">(optional; a tag's meaning lives in .chug/tags/&#123;tag&#125;.md)</span>
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

        <GroupPicker
          value={groups}
          options={groupChoices}
          onAdd={(name) => setGroups((gs) => [...gs, name])}
          onRemove={(name) => setGroups((gs) => gs.filter((g) => g !== name))}
        />

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
          <div className="mode-seg" role="radiogroup" aria-label="Initial state">
            {(
              [
                ['draft', 'Draft', 'editable — finish writing it in the live editor'],
                ['frozen', 'Frozen', 'parked — release it later (batchable)'],
                ['ready', 'Ready', 'released to the dispatcher immediately'],
              ] as const
            ).map(([m, label, tip]) => (
              <button
                key={m}
                type="button"
                role="radio"
                aria-checked={mode === m}
                className={`mode-seg-opt${mode === m ? ' mode-seg-on' : ''}`}
                title={tip}
                disabled={submitting}
                onClick={() => setMode(m)}
              >
                {label}
              </button>
            ))}
          </div>
          <button
            type="submit"
            className="btn-primary-glow"
            disabled={submitting}
            aria-busy={submitting}
          >
            {createJobSubmitLabel(mode, submitting)}
          </button>
        </div>
        {error && <div className="error">{error}</div>}
      </form>
    </div>
  )
}
