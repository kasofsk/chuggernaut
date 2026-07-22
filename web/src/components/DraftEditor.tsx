import { useEffect, useRef, useState } from 'react'
import {
  ApiError,
  api,
  type Job,
  type JobPatch,
  type JobTypeSummary,
} from '../api'
import { useDebouncedCallback } from '../useEvents'
import { Markdown } from './Markdown'
import { RichSelect } from './RichSelect'

// The editable fields, used as keys for focus/dirty/flash tracking. `eval` is
// not edited here — it round-trips unchanged on the full-replace PATCH.
type Field = 'title' | 'description' | 'type' | 'knowledge_tags' | 'deps' | 'timeout' | 'model'

const sameNums = (a: number[], b: number[]) =>
  a.length === b.length && a.every((x, i) => x === b[i])
const sameStrs = (a: string[], b: string[]) =>
  a.length === b.length && a.every((x, i) => x === b[i])

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
  onRelease,
  onRevoke,
  onLeftDraft,
}: {
  owner: string
  project: string
  job: Job
  onRelease: () => void
  onRevoke: () => void
  /** called on a 409 so the parent refetches and flips to the read view */
  onLeftDraft: () => void
}) {
  const [title, setTitle] = useState(job.title)
  const [description, setDescription] = useState(job.description)
  const [type, setType] = useState(job.type)
  const [tags, setTags] = useState<string[]>(job.knowledge_tags)
  const [deps, setDeps] = useState<number[]>(job.deps)
  const [timeout, setTimeoutVal] = useState(job.timeout ?? '')
  const [model, setModel] = useState(job.model ?? '')

  const [preview, setPreview] = useState(false)
  const [depQuery, setDepQuery] = useState('')
  const [saveError, setSaveError] = useState<string | null>(null)

  // Pickers: the type vocabulary, the tag vocabulary, and existing jobs (for the
  // dependency picker), fetched once for the editor.
  const [jobTypes, setJobTypes] = useState<JobTypeSummary[]>([])
  const [availTags, setAvailTags] = useState<string[]>([])
  const [allJobs, setAllJobs] = useState<Job[]>([])
  useEffect(() => {
    api.jobTypes(owner, project).then(setJobTypes, () => {})
    api.tags(owner, project).then(setAvailTags, () => {})
    api.jobs(owner, project).then(setAllJobs, () => {})
  }, [owner, project])

  // Which field the operator is editing (never adopt a server value over it),
  // which fields have a local edit not yet echoed by the server (don't let a
  // stale refetch revert it), and which just flashed after adopting a remote
  // change. focus/dirty are refs — read inside the reconcile effect, no re-render.
  const focusedRef = useRef<Field | null>(null)
  const dirtyRef = useRef<Set<Field>>(new Set())
  const localRef = useRef({ title, description, type, tags, deps, timeout, model })
  localRef.current = { title, description, type, tags, deps, timeout, model }
  const [flash, setFlash] = useState<Set<Field>>(new Set())
  const flashField = (f: Field) => {
    setFlash((s) => new Set(s).add(f))
    setTimeout(() => setFlash((s) => {
      const n = new Set(s)
      n.delete(f)
      return n
    }), 1200)
  }

  // Merge an incoming server snapshot. Per field: if the server matches local,
  // our edit landed — clear its dirty flag. Otherwise adopt the remote value and
  // flash — unless the operator is on that field, or it holds an un-echoed local
  // edit (dirty), in which case we keep the operator's version.
  useEffect(() => {
    const cur = localRef.current
    const merge = (f: Field, equal: boolean, apply: () => void) => {
      if (equal) {
        dirtyRef.current.delete(f)
        return
      }
      if (focusedRef.current === f || dirtyRef.current.has(f)) return
      apply()
      flashField(f)
    }
    merge('title', job.title === cur.title, () => setTitle(job.title))
    merge('description', job.description === cur.description, () => setDescription(job.description))
    merge('type', job.type === cur.type, () => setType(job.type))
    merge('knowledge_tags', sameStrs(job.knowledge_tags, cur.tags), () => setTags(job.knowledge_tags))
    merge('deps', sameNums(job.deps, cur.deps), () => setDeps(job.deps))
    merge('timeout', (job.timeout ?? '') === cur.timeout, () => setTimeoutVal(job.timeout ?? ''))
    merge('model', (job.model ?? '') === cur.model, () => setModel(job.model ?? ''))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job])

  // Build the full PATCH payload from the latest state (this closure is recreated
  // each render, so the debounced call reads current values at fire time). `eval`
  // rides along unchanged so the full replace doesn't wipe per-job evaluators.
  const buildPayload = (): JobPatch => ({
    type,
    title,
    description,
    deps,
    knowledge_tags: tags,
    eval: job.eval,
    timeout: timeout.trim() || null,
    model: model.trim() || null,
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

  // A local edit: mark the field dirty (so a stale refetch won't revert it) and
  // schedule the debounced full PATCH.
  const edit = (f: Field) => {
    dirtyRef.current.add(f)
    debouncedPatch()
  }
  const blur = () => {
    focusedRef.current = null
    patchNow()
  }
  const fieldClass = (f: Field) => `field${flash.has(f) ? ' draft-flash' : ''}`

  // Discrete edits (tags, deps, type) go through the debounced PATCH, not an
  // immediate one: the debounced call fires after the state update has
  // re-rendered, so buildPayload reads the new value rather than the stale
  // closure. `edit()` marks the field dirty and schedules that debounced PATCH.
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

  // Dependency candidates: existing non-revoked jobs, excluding self and any
  // already chosen. Matching mirrors the create form's picker.
  const depMatches = allJobs
    .filter((j) => j.id !== job.id && j.state !== 'Revoked' && !deps.includes(j.id))
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

  // Tag options: the vocabulary plus any tags already on the job that aren't in it.
  const tagOptions = [...new Set([...availTags, ...tags])]

  return (
    <section className="card create-job draft-editor">
      {saveError && <div className="error banner">{saveError}</div>}

      <label className={fieldClass('title')}>
        <span>Title <span className="dim">(what this run is for)</span></span>
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
          Description <span className="dim">(the ticket — markdown; injected into the work and eval prompts)</span>
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
        <span>Job type</span>
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

      <div className={fieldClass('deps')}>
        <span>Depends on <span className="dim">(jobs that must finish first)</span></span>
        {deps.length > 0 && (
          <div className="tag-row">
            {deps.map((d) => {
              const j = allJobs.find((x) => x.id === d)
              return (
                <button
                  type="button"
                  key={d}
                  className="tag tag-on"
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

      <div className={fieldClass('knowledge_tags')}>
        <span>
          Knowledge tags{' '}
          <span className="dim">(a tag's meaning lives in tags/&#123;tag&#125;.md)</span>
        </span>
        {tagOptions.length > 0 ? (
          <div className="tag-row">
            {tagOptions.map((t) => (
              <button
                type="button"
                key={t}
                className={tags.includes(t) ? 'tag tag-on' : 'tag'}
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

      <details className="draft-advanced">
        <summary>Advanced <span className="dim">(work model and timeout overrides)</span></summary>
        <label className={fieldClass('timeout')}>
          <span>
            Work timeout{' '}
            <span className="dim">(overrides the type default for this job's work)</span>
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
            <span className="dim">(overrides the type, project and platform defaults)</span>
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

      <div className="create-actions">
        <button
          type="button"
          title="finalize and release this draft: validation runs, then the dispatcher takes over"
          onClick={onRelease}
        >
          release
        </button>
        <button type="button" className="danger" onClick={onRevoke}>
          revoke
        </button>
        <span className="dim draft-save-hint">edits save automatically</span>
      </div>
    </section>
  )
}
