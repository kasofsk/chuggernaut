import { useEffect, useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { ApiError, api, type Evaluator, type Job } from '../api'
import { prefersReducedMotion } from '../useTypewriter'

/**
 * The Draft-batch consist editor (§2.1 draft batches): a batch's member list
 * shown as a little train — the locomotive plus one coupled car per member —
 * matching the deps-as-cars language of the create form (#171). Coupling a car
 * calls the members endpoint (`{ add: [seq] }`); the per-car uncouple pin calls
 * `{ remove: [seq] }`. The manifest card previews the dep union + eval union
 * the batch will absorb at finalize/release (computed client-side from the
 * member records — the draft holds no union until it leaves Draft). Finalize
 * and release with <2 cars shake the locomotive instead of proceeding.
 *
 * All motion is transform/opacity-only and collapses to instant states under
 * prefers-reduced-motion. The track scrolls in its own container so a long
 * consist never scrolls the page sideways.
 */
export function ConsistEditor({
  owner,
  project,
  job,
  onRelease,
  onFinalize,
  onRevoke,
  onEdited,
}: {
  owner: string
  project: string
  job: Job
  onRelease: () => void
  onFinalize: () => void
  onRevoke: () => void
  /** refetch the batch (and member records) after a membership edit lands */
  onEdited: () => void
}) {
  const members = useMemo(() => job.members ?? [], [job.members])
  const [allJobs, setAllJobs] = useState<Job[]>([])
  const [query, setQuery] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [bumpId, setBumpId] = useState<number | null>(null)
  const [driftId, setDriftId] = useState<number | null>(null)
  const [shake, setShake] = useState(false)

  const reload = () =>
    api.jobs(owner, project).then(setAllJobs, () => {})
  useEffect(() => {
    reload()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [owner, project])
  useEffect(() => {
    reload()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [members])

  const jobOf = (id: number) => allJobs.find((j) => j.id === id)

  function couple(id: number) {
    setBusy(true)
    api.members(owner, project, job.id, { add: [id] }).then(
      () => {
        setError(null)
        setQuery('')
        setBumpId(id)
        setTimeout(() => setBumpId(null), 420)
        onEdited()
      },
      (e) => setError(errText(e, 'could not couple that car')),
    ).finally(() => setBusy(false))
  }

  function uncouple(id: number) {
    if (busy) return
    const commit = () =>
      api.members(owner, project, job.id, { remove: [id] }).then(
        () => {
          setError(null)
          setDriftId(null)
          onEdited()
        },
        (e) => {
          setDriftId(null)
          setError(errText(e, 'could not uncouple that car'))
        },
      )
    if (prefersReducedMotion()) {
      commit()
    } else {
      setDriftId(id)
      setTimeout(commit, 380)
    }
  }

  function guarded(action: () => void) {
    if (members.length < 2) {
      setError('a consist needs at least two cars to leave the yard — couple one more')
      if (!prefersReducedMotion()) {
        setShake(true)
        setTimeout(() => setShake(false), 520)
      }
      return
    }
    setError(null)
    action()
  }

  const candidates = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return []
    const bare = q.replace('#', '')
    return allJobs
      .filter((j) => j.id !== job.id && !members.includes(j.id))
      .filter(
        (j) =>
          `#${j.id}`.includes(q) ||
          String(j.id).startsWith(bare) ||
          j.title.toLowerCase().includes(q) ||
          j.type.toLowerCase().includes(q),
      )
      .map((j) => ({ job: j, reason: ineligibility(j, job) }))
      .sort((a, b) => (a.reason ? 1 : 0) - (b.reason ? 1 : 0))
      .slice(0, 8)
  }, [allJobs, query, members, job])

  const manifest = useMemo(() => computeManifest(members, jobOf), [members, allJobs])

  return (
    <section className="card consist-editor">
      <h2>
        Consist <span className="dim">{members.length} car{members.length === 1 ? '' : 's'}</span>
      </h2>
      <p className="dim consist-lede">
        This batch rides as one train — one branch implements every coupled car, judged under the
        union of their criteria. Couple same-type Frozen jobs; uncouple with the pin.
      </p>

      {error && <div className="error banner">{error}</div>}

      <div className="consist-track" role="list" aria-label="batch consist">
        <span className={`consist-loco${shake ? ' consist-shake' : ''}`} aria-hidden="true">
          🚂
        </span>
        {members.map((id) => {
          const m = jobOf(id)
          return (
            <span key={id} className="consist-car-wrap" role="listitem">
              <span className="consist-coupler" aria-hidden="true">
                –
              </span>
              <span
                className={`consist-car${bumpId === id ? ' consist-bump' : ''}${
                  driftId === id ? ' consist-drift' : ''
                }`}
              >
                <Link className="consist-car-seq" to={`/p/${owner}/${project}/jobs/${id}`}>
                  #{id}
                </Link>
                <span className="consist-car-title" title={m?.title || undefined}>
                  {m ? m.title || <span className="dim">{m.type}</span> : <span className="dim">…</span>}
                </span>
                <button
                  type="button"
                  className="consist-uncouple"
                  title="uncouple pin — drift this car to the siding"
                  aria-label={`uncouple #${id}`}
                  disabled={busy || driftId != null}
                  onClick={() => uncouple(id)}
                >
                  ⚲
                </button>
              </span>
            </span>
          )
        })}
        {members.length === 0 && (
          <span className="dim consist-empty">no cars yet — couple a job to build the consist</span>
        )}
      </div>

      <div className="consist-add">
        <input
          placeholder="couple a car — search Frozen same-type jobs by #, title, or type…"
          value={query}
          disabled={busy}
          onChange={(e) => setQuery(e.target.value)}
        />
        {query.trim() !== '' && (
          <div className="dep-suggestions">
            {candidates.map(({ job: j, reason }) => (
              <button
                type="button"
                key={j.id}
                className={`dep-suggestion${reason ? ' consist-cand-off' : ''}`}
                disabled={!!reason || busy}
                title={reason ?? undefined}
                onClick={() => !reason && couple(j.id)}
              >
                #{j.id} <b>{j.title || j.type}</b>{' '}
                <span className="dim">
                  {j.title ? `${j.type} · ` : ''}
                  {reason ? reason : j.state}
                </span>
              </button>
            ))}
            {candidates.length === 0 && <div className="dim">no matching jobs</div>}
          </div>
        )}
      </div>

      <ManifestCard owner={owner} project={project} manifest={manifest} />

      <div className="create-actions consist-actions">
        <button
          type="button"
          title="release this batch: its members are re-validated and absorbed, then the dispatcher takes over"
          onClick={() => guarded(onRelease)}
        >
          release
        </button>
        <button
          type="button"
          className="link"
          title="finalize this batch to Frozen: members are absorbed and the batch parks re-batchable until you release it"
          onClick={() => guarded(onFinalize)}
        >
          finalize
        </button>
        <button type="button" className="danger" onClick={onRevoke}>
          revoke
        </button>
        <span className="dim draft-save-hint">membership saves as you couple</span>
      </div>
    </section>
  )
}

interface Manifest {
  deps: number[]
  evals: string[]
}

/** The dep union (each member's external deps, minus fellow members) and the
 *  eval union (member `eval` lists deduped by name) the batch will absorb —
 *  the same computation the dispatcher runs at finalize/release, previewed. */
function computeManifest(members: number[], jobOf: (id: number) => Job | undefined): Manifest {
  const set = new Set(members)
  const deps = new Set<number>()
  const evals = new Map<string, Evaluator>()
  for (const id of members) {
    const m = jobOf(id)
    if (!m) continue
    for (const d of m.deps) if (!set.has(d)) deps.add(d)
    for (const e of m.eval) if (!evals.has(e.name)) evals.set(e.name, e)
  }
  return { deps: [...deps].sort((a, b) => a - b), evals: [...evals.keys()] }
}

/** A stamped manifest card that re-stamps (a brief press animation) whenever
 *  the computed union changes — so an edit visibly restamps the paperwork. */
function ManifestCard({
  owner,
  project,
  manifest,
}: {
  owner: string
  project: string
  manifest: Manifest
}) {
  const key = `${manifest.deps.join(',')}|${manifest.evals.join(',')}`
  const [stamp, setStamp] = useState(false)
  const first = useRef(true)
  useEffect(() => {
    if (first.current) {
      first.current = false
      return
    }
    if (prefersReducedMotion()) return
    setStamp(true)
    const t = setTimeout(() => setStamp(false), 500)
    return () => clearTimeout(t)
  }, [key])

  return (
    <div className={`manifest-card${stamp ? ' manifest-restamp' : ''}`}>
      <div className="manifest-head">
        Manifest <span className="dim">computed union — stamped at release</span>
      </div>
      <dl className="manifest-body">
        <dt>deps</dt>
        <dd>
          {manifest.deps.length
            ? manifest.deps.map((d, i) => (
                <span key={d}>
                  {i > 0 && ', '}
                  <Link to={`/p/${owner}/${project}/jobs/${d}`}>#{d}</Link>
                </span>
              ))
            : '—'}
        </dd>
        <dt>evaluators</dt>
        <dd>
          {manifest.evals.length ? (
            manifest.evals.map((n) => (
              <code key={n} className="manifest-eval">
                {n}
              </code>
            ))
          ) : (
            <span className="dim">type defaults only</span>
          )}
        </dd>
      </dl>
    </div>
  )
}

/** Why `cand` can't couple onto `batch` (null = eligible). Mirrors the
 *  creation rules: Frozen, same type, not already batched, not itself a batch. */
function ineligibility(cand: Job, batch: Job): string | null {
  if ((cand.members ?? []).length > 0) return 'a batch can’t be a car'
  if (cand.batch_id != null) return `already coupled to batch #${cand.batch_id}`
  if (cand.type !== batch.type) return `wrong gauge — ${cand.type} car on a ${batch.type} train`
  if (cand.state !== 'Frozen') return `not frozen — ${cand.state.toLowerCase()}`
  return null
}

function errText(e: unknown, fallback: string): string {
  if (e instanceof ApiError && e.status === 409) return 'this batch is no longer a Draft — reload to edit'
  return e instanceof Error && e.message ? e.message : fallback
}
