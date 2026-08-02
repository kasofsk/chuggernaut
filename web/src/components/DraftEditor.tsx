import { useEffect, useRef, useState } from 'react'
import {
  ApiError,
  api,
  type Job,
  type JobFull,
  type JobPatch,
  type JobTypeSummary,
} from '../api'
import { useDebouncedCallback } from '../useEvents'
import { prefersReducedMotion, useTypewriter } from '../useTypewriter'
import { Markdown } from './Markdown'
import { RichSelect } from './RichSelect'
import { JobAttachments } from './Attachments'
import { JobInputFields, inputsOrUndefined, suppliedInputs, useJobTypeDetail } from './JobInputs'
import { GroupPicker, useGroupOptions } from './JobGroups'
import { depCandidates } from '../jobFilters'

type Field =
  | 'title'
  | 'description'
  | 'type'
  | 'knowledge_tags'
  | 'groups'
  | 'deps'
  | 'timeout'
  | 'model'
  | 'inputs'

const sameNums = (a: number[], b: number[]) =>
  a.length === b.length && a.every((x, i) => x === b[i])
const sameStrs = (a: string[], b: string[]) =>
  a.length === b.length && a.every((x, i) => x === b[i])
const sameMap = (a: Record<string, string>, b: Record<string, string>) => {
  const keys = Object.keys(a)
  return keys.length === Object.keys(b).length && keys.every((k) => a[k] === b[k])
}

/**
 * The Draft job editor (spec §72): the chat session and the operator edit the
 * SAME draft. Local edits PATCH the full field set on blur and on a ~1s debounce;
 * incoming `job-updated` (parent refetch → new `job` prop) is merged field by
 * field — never clobbering the field the operator is editing, flashing the rest.
 * A 409 (the draft left Draft) surfaces inline and lets the parent flip to the
 * read view.
 */
export function DraftEditor({
  owner,
  project,
  job,
  isBatch = false,
  onRelease,
  onFinalize,
  onRevoke,
  onLeftDraft,
}: {
  owner: string
  project: string
  job: JobFull
  /** true for a Draft batch: the plain deps picker and the finalize/release/
   *  revoke actions are hidden here — the ConsistEditor renders the members
   *  consist and owns those actions (§2.1 draft batches). */
  isBatch?: boolean
  onRelease: () => void
  onFinalize: () => void
  onRevoke: () => void
  /** called on a 409 so the parent refetches and flips to the read view */
  onLeftDraft: () => void
}) {
  const [title, setTitle] = useState(job.title)
  const [description, setDescription] = useState(job.description)
  const [type, setType] = useState(job.type)
  const [tags, setTags] = useState<string[]>(job.knowledge_tags)
  const [groups, setGroups] = useState<string[]>(job.groups ?? [])
  const [deps, setDeps] = useState<number[]>(job.deps)
  const [timeout, setTimeoutVal] = useState(job.timeout ?? '')
  const [model, setModel] = useState(job.model ?? '')
  const [inputs, setInputs] = useState<Record<string, string>>(job.inputs ?? {})

  const [preview, setPreview] = useState(false)
  const [depQuery, setDepQuery] = useState('')
  const [saveError, setSaveError] = useState<string | null>(null)

  const [jobTypes, setJobTypes] = useState<JobTypeSummary[]>([])
  const [availTags, setAvailTags] = useState<string[]>([])
  const groupChoices = useGroupOptions(owner, project)
  const [allJobs, setAllJobs] = useState<Job[]>([])
  const typeDetail = useJobTypeDetail(owner, project, type)
  const declaredInputs = typeDetail?.job_type?.inputs ?? []
  useEffect(() => {
    api.jobTypes(owner, project).then(setJobTypes, () => {})
    api.tags(owner, project).then((ts) => setAvailTags(ts.map((t) => t.name)), () => {})
    api.jobs(owner, project).then(setAllJobs, () => {})
  }, [owner, project])

  const focusedRef = useRef<Field | null>(null)
  const dirtyRef = useRef<Set<Field>>(new Set())
  const localRef = useRef({ title, description, type, tags, groups, deps, timeout, model, inputs })
  localRef.current = { title, description, type, tags, groups, deps, timeout, model, inputs }
  const [flash, setFlash] = useState<Set<Field>>(new Set())
  const flashField = (f: Field) => {
    setFlash((s) => new Set(s).add(f))
    setTimeout(() => setFlash((s) => {
      const n = new Set(s)
      n.delete(f)
      return n
    }), 1200)
  }

  const { textActive, typewrite } = useTypewriter()
  const [pulsing, setPulsing] = useState<Set<Field>>(new Set())
  const [valFlash, setValFlash] = useState<Set<string>>(new Set())
  const pulseField = (f: Field) => {
    setPulsing((s) => new Set(s).add(f))
    setTimeout(() => setPulsing((s) => {
      const n = new Set(s)
      n.delete(f)
      return n
    }), 900)
  }
  const flashValue = (key: string) => {
    setValFlash((s) => new Set(s).add(key))
    setTimeout(() => setValFlash((s) => {
      const n = new Set(s)
      n.delete(key)
      return n
    }), 1200)
  }

  useEffect(() => {
    const cur = localRef.current
    const reduced = prefersReducedMotion()
    const canAdopt = (f: Field, equal: boolean) => {
      if (equal) {
        dirtyRef.current.delete(f)
        return false
      }
      return focusedRef.current !== f && !dirtyRef.current.has(f)
    }
    const adoptText = (f: Field, from: string, to: string, set: (v: string) => void) => {
      if (!canAdopt(f, from === to)) return
      typewrite(f, from, to, set)
      flashField(f)
    }
    const adoptChoice = (f: Field, equal: boolean, apply: () => void, added: string[]) => {
      if (!canAdopt(f, equal)) return
      apply()
      flashField(f)
      if (!reduced) {
        pulseField(f)
        added.forEach(flashValue)
      }
    }
    adoptText('title', cur.title, job.title, setTitle)
    adoptText('description', cur.description, job.description, setDescription)
    adoptText('timeout', cur.timeout, job.timeout ?? '', setTimeoutVal)
    adoptText('model', cur.model, job.model ?? '', setModel)
    adoptChoice('type', job.type === cur.type, () => setType(job.type), [])
    adoptChoice(
      'knowledge_tags',
      sameStrs(job.knowledge_tags, cur.tags),
      () => setTags(job.knowledge_tags),
      job.knowledge_tags.filter((t) => !cur.tags.includes(t)).map((t) => `tag:${t}`),
    )
    adoptChoice(
      'groups',
      sameStrs(job.groups ?? [], cur.groups),
      () => setGroups(job.groups ?? []),
      [],
    )
    adoptChoice(
      'deps',
      sameNums(job.deps, cur.deps),
      () => setDeps(job.deps),
      job.deps.filter((d) => !cur.deps.includes(d)).map((d) => `dep:${d}`),
    )
    adoptChoice('inputs', sameMap(job.inputs ?? {}, cur.inputs), () => setInputs(job.inputs ?? {}), [])
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job])

  const buildPayload = (): JobPatch => ({
    type,
    title,
    description,
    deps,
    knowledge_tags: tags,
    groups,
    eval: job.eval,
    require_approval: job.require_approval,
    timeout: timeout.trim() || null,
    model: model.trim() || null,
    inputs: typeDetail ? suppliedInputs(declaredInputs, inputs) : inputsOrUndefined(inputs),
  })

  function patchNow() {
    api.patchJob(owner, project, job.id, buildPayload()).then(
      () => setSaveError(null),
      (e) => {
        if (e instanceof ApiError && e.status === 409) {
          setSaveError('this draft is no longer editable — it left Draft; switching to the read view…')
          onLeftDraft()
        } else setSaveError(e instanceof Error ? e.message : 'save failed')
      },
    )
  }
  const debouncedPatch = useDebouncedCallback(patchNow, 1000)

  const edit = (f: Field) => {
    dirtyRef.current.add(f)
    debouncedPatch()
  }
  const blur = () => {
    focusedRef.current = null
    patchNow()
  }
  const fieldClass = (f: Field) =>
    `field${flash.has(f) ? ' draft-flash' : ''}${pulsing.has(f) ? ' draft-pulse' : ''}`
  const wiz = (f: Field) =>
    textActive.has(f) || pulsing.has(f) ? (
      <span className="draft-wiz" title="updating from chat" aria-hidden="true">
        ✦
      </span>
    ) : null

  function toggleTag(t: string) {
    setTags((ts) => (ts.includes(t) ? ts.filter((x) => x !== t) : [...ts, t]))
    edit('knowledge_tags')
  }
  function addDep(id: number) {
    setDeps((ds) => (ds.includes(id) ? ds : [...ds, id]))
    setDepQuery('')
    edit('deps')
  }
  function removeDep(id: number) {
    setDeps((ds) => ds.filter((x) => x !== id))
    edit('deps')
  }

  function editGroups(next: string[]) {
    setGroups(next)
    edit('groups')
  }

  const depMatches = depCandidates(allJobs, deps, depQuery, job.id)

  const tagOptions = [...new Set([...availTags, ...tags])]

  return (
    <section className="card create-job draft-editor">
      {saveError && <div className="error banner">{saveError}</div>}

      <label className={fieldClass('title')}>
        <span>Title <span className="dim">(what this run is for)</span>{wiz('title')}</span>
        <input
          value={title}
          onFocus={() => (focusedRef.current = 'title')}
          onBlur={blur}
          onChange={(e) => {
            setTitle(e.target.value)
            edit('title')
          }}
        />
      </label>

      <div className={fieldClass('description')}>
        <span>
          Description <span className="dim">(the ticket — markdown; injected into the work and eval prompts)</span>{wiz('description')}
          <button type="button" className="link draft-preview-toggle" onClick={() => setPreview((p) => !p)}>
            {preview ? 'edit' : 'preview'}
          </button>
        </span>
        {preview ? (
          description.trim() ? (
            <Markdown text={description} className="draft-preview" />
          ) : (
            <div className="draft-preview dim">nothing to preview</div>
          )
        ) : (
          <textarea
            className="draft-desc"
            rows={14}
            value={description}
            onFocus={() => (focusedRef.current = 'description')}
            onBlur={blur}
            onChange={(e) => {
              setDescription(e.target.value)
              edit('description')
            }}
          />
        )}
      </div>

      <div className={fieldClass('type')}>
        <span>Job type{wiz('type')}</span>
        <div className="type-select">
          <RichSelect
            value={type}
            onChange={(v) => {
              setType(v)
              edit('type')
            }}
            placeholder="pick a job type…"
            options={jobTypes.map((t) => ({
              value: t.name,
              label: t.display_name,
              description: t.description || undefined,
              detail: t.display_name !== t.name ? t.name : undefined,
            }))}
          />
          {type && (
            <a
              className="option-peek"
              href={`/p/${owner}/${project}/job-types/${encodeURIComponent(type)}`}
              target="_blank"
              rel="noreferrer"
              title="see what this job type does (library, new tab)"
            >
              ↗
            </a>
          )}
        </div>
      </div>

      <JobInputFields
        declared={declaredInputs}
        values={inputs}
        fieldClassName={fieldClass('inputs')}
        onFieldFocus={() => (focusedRef.current = 'inputs')}
        onFieldBlur={blur}
        onChange={(name, value) => {
          setInputs((vs) => ({ ...vs, [name]: value }))
          edit('inputs')
        }}
      />

      {!isBatch && (
      <div className={fieldClass('deps')}>
        <span>Depends on <span className="dim">(jobs that must finish first)</span>{wiz('deps')}</span>
        {deps.length > 0 && (
          <div className="tag-row">
            {deps.map((d) => {
              const j = allJobs.find((x) => x.id === d)
              return (
                <button
                  type="button"
                  key={d}
                  className={`tag tag-on${valFlash.has(`dep:${d}`) ? ' draft-val-flash' : ''}`}
                  title="remove"
                  onClick={() => removeDep(d)}
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
              <button type="button" key={j.id} className="dep-suggestion" onClick={() => addDep(j.id)}>
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
      )}

      <div className={fieldClass('knowledge_tags')}>
        <span>
          Knowledge tags{' '}
          <span className="dim">(a tag's meaning lives in .chug/tags/&#123;tag&#125;.md)</span>{wiz('knowledge_tags')}
        </span>
        {tagOptions.length > 0 ? (
          <div className="tag-row">
            {tagOptions.map((t) => (
              <button
                type="button"
                key={t}
                className={`${tags.includes(t) ? 'tag tag-on' : 'tag'}${
                  valFlash.has(`tag:${t}`) ? ' draft-val-flash' : ''
                }`}
                onClick={() => toggleTag(t)}
              >
                {t}
              </button>
            ))}
          </div>
        ) : (
          <span className="dim">no tags defined in this project</span>
        )}
      </div>

      <GroupPicker
        value={groups}
        options={groupChoices}
        fieldClassName={fieldClass('groups')}
        labelExtra={wiz('groups')}
        onAdd={(name) => editGroups([...groups, name])}
        onRemove={(name) => editGroups(groups.filter((g) => g !== name))}
      />

      <details className="draft-advanced">
        <summary>Advanced <span className="dim">(work model and timeout overrides)</span></summary>
        <label className={fieldClass('timeout')}>
          <span>
            Work timeout{' '}
            <span className="dim">(overrides the type default for this job's work)</span>{wiz('timeout')}
          </span>
          <input
            value={timeout}
            placeholder="e.g. 45m, 2h, 1h30m"
            onFocus={() => (focusedRef.current = 'timeout')}
            onBlur={blur}
            onChange={(e) => {
              setTimeoutVal(e.target.value)
              edit('timeout')
            }}
          />
        </label>
        <label className={fieldClass('model')}>
          <span>
            Work model{' '}
            <span className="dim">(overrides the type, project and platform defaults)</span>{wiz('model')}
          </span>
          <input
            value={model}
            placeholder="e.g. claude-opus-4-8, claude-fable-5"
            onFocus={() => (focusedRef.current = 'model')}
            onBlur={blur}
            onChange={(e) => {
              setModel(e.target.value)
              edit('model')
            }}
          />
        </label>
      </details>

      <JobAttachments owner={owner} project={project} seq={job.id} />

      {isBatch ? (
        <div className="create-actions">
          <span className="dim draft-save-hint">edits save automatically · membership below</span>
        </div>
      ) : (
      <div className="create-actions">
        <button
          type="button"
          title="release this draft: validation runs, then the dispatcher takes over"
          onClick={onRelease}
        >
          release
        </button>
        <button
          type="button"
          className="link"
          title="finalize this draft to Frozen: validation runs, then it parks unscheduled (re-batchable) until you release it"
          onClick={onFinalize}
        >
          finalize
        </button>
        <button type="button" className="danger" onClick={onRevoke}>
          revoke
        </button>
        <span className="dim draft-save-hint">edits save automatically</span>
      </div>
      )}
    </section>
  )
}
