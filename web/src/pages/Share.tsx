import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ApiError, api, type Job, type JobTypeSummary } from '../api'
import { fmtBytes, isImage, uploadFiles } from '../components/Attachments'

const SHARE_CACHE = 'chug-share-v1'
const SHARED_KEY = '/__shared'
const LAST_PROJECT = 'chug-share-project'

/**
 * PWA share-target landing (job #174): the OS share sheet hands a screenshot to
 * the service worker (see public/sw.js), which stashes it and redirects here.
 * This screen reads the file back from `/__shared` and offers the minimal mobile
 * flow — attach it to a brand-new job (title + image, type defaulting to `web`)
 * or to a recent existing job — then navigates to that job. The whole point is
 * screenshot → share → Chuggernaut → done, no manual navigation.
 */
export function SharePage() {
  const navigate = useNavigate()
  const [file, setFile] = useState<File | null>(null)
  const [previewUrl, setPreviewUrl] = useState<string | null>(null)
  const [projects, setProjects] = useState<string[]>([])
  const [project, setProject] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [mode, setMode] = useState<'new' | 'existing'>('new')
  const [busy, setBusy] = useState(false)

  const [title, setTitle] = useState('')
  const [jobTypes, setJobTypes] = useState<JobTypeSummary[]>([])
  const [type, setType] = useState('')
  const [jobs, setJobs] = useState<Job[]>([])
  const [jobQuery, setJobQuery] = useState('')

  const [owner, projName] = useMemo(
    () => (project.includes('/') ? (project.split('/') as [string, string]) : ['', '']),
    [project],
  )

  useEffect(() => {
    let revoked = false
    fetch(SHARED_KEY)
      .then(async (res) => {
        if (!res.ok) return
        const blob = await res.blob()
        const name = decodeURIComponent(res.headers.get('X-Filename') || '') || 'shared.png'
        const shareTitle = decodeURIComponent(res.headers.get('X-Share-Title') || '')
        const f = new File([blob], name, { type: blob.type })
        setFile(f)
        if (shareTitle) setTitle(shareTitle)
        if (isImage(blob.type)) {
          const u = URL.createObjectURL(blob)
          if (!revoked) setPreviewUrl(u)
        }
        if ('caches' in window) caches.open(SHARE_CACHE).then((c) => c.delete(SHARED_KEY)).catch(() => {})
      })
      .catch(() => {})
    return () => {
      revoked = true
    }
  }, [])
  useEffect(() => () => { if (previewUrl) URL.revokeObjectURL(previewUrl) }, [previewUrl])

  useEffect(() => {
    api.projects().then(
      (ps) => {
        setProjects(ps)
        const last = localStorage.getItem(LAST_PROJECT)
        setProject(last && ps.includes(last) ? last : ps[0] ?? '')
        setError(null)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'failed to load projects')
      },
    ).finally(() => setLoading(false))
  }, [navigate])

  useEffect(() => {
    if (!owner || !projName) return
    api.jobTypes(owner, projName).then(
      (ts) => {
        setJobTypes(ts)
        const web = ts.find((t) => /(^|-)web($|-)/i.test(t.name)) ?? ts.find((t) => /web/i.test(t.name))
        setType(web?.name ?? ts[0]?.name ?? 'web')
      },
      () => {
        setJobTypes([])
        setType('web')
      },
    )
    api.jobs(owner, projName).then(setJobs, () => setJobs([]))
  }, [owner, projName])

  function rememberProject() {
    if (project) localStorage.setItem(LAST_PROJECT, project)
  }

  async function createNew() {
    if (!file || !owner) return
    setBusy(true)
    setError(null)
    try {
      const job = await api.createJob(owner, projName, {
        type,
        title: title.trim() || undefined,
      })
      await uploadFiles(owner, projName, job.id, [file])
      rememberProject()
      navigate(`/p/${owner}/${projName}/jobs/${job.id}`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'could not create the job')
      setBusy(false)
    }
  }

  async function attachTo(job: Job) {
    if (!file || !owner) return
    setBusy(true)
    setError(null)
    try {
      await uploadFiles(owner, projName, job.id, [file])
      rememberProject()
      navigate(`/p/${owner}/${projName}/jobs/${job.id}`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'could not attach the file')
      setBusy(false)
    }
  }

  const jobMatches = jobs
    .filter((j) => j.state !== 'Revoked')
    .filter((j) => {
      const q = jobQuery.trim().toLowerCase()
      if (!q) return true
      return `#${j.id}`.includes(q) || j.title.toLowerCase().includes(q) || j.type.toLowerCase().includes(q)
    })
    .sort((a, b) => b.id - a.id)
    .slice(0, 20)

  return (
    <div className="page share-page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>Attach to a job</h1>
      </header>

      {error && <div className="error banner">{error}</div>}

      <section className="card">
        <h2>Shared file</h2>
        {file ? (
          <div className="share-file">
            {previewUrl ? (
              <img className="share-preview" src={previewUrl} alt={file.name} />
            ) : (
              <span className="attach-glyph" aria-hidden="true">📄</span>
            )}
            <div className="attach-meta">
              <span className="attach-name">{file.name}</span>
              <span className="dim attach-size">{fmtBytes(file.size)}</span>
            </div>
          </div>
        ) : (
          <p className="dim">
            {loading ? 'loading…' : 'No shared file — open Chuggernaut from your phone’s share sheet to attach a screenshot.'}
          </p>
        )}
      </section>

      {file && (
        <section className="card">
          <label className="field">
            <span>Project</span>
            <select value={project} onChange={(e) => setProject(e.target.value)} disabled={busy || !projects.length}>
              {projects.length === 0 && <option value="">no projects</option>}
              {projects.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </label>

          <div className="mode-seg share-mode" role="radiogroup" aria-label="Attach to">
            {(
              [
                ['new', 'New job'],
                ['existing', 'Existing job'],
              ] as const
            ).map(([m, label]) => (
              <button
                key={m}
                type="button"
                role="radio"
                aria-checked={mode === m}
                className={`mode-seg-opt${mode === m ? ' mode-seg-on' : ''}`}
                onClick={() => setMode(m)}
              >
                {label}
              </button>
            ))}
          </div>

          {mode === 'new' ? (
            <div className="share-new">
              <label className="field">
                <span>Title <span className="dim">(what this run is for)</span></span>
                <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="e.g. layout bug on the job page" />
              </label>
              <label className="field">
                <span>Job type</span>
                <select value={type} onChange={(e) => setType(e.target.value)}>
                  {jobTypes.length === 0 && <option value={type}>{type}</option>}
                  {jobTypes.map((t) => (
                    <option key={t.name} value={t.name}>
                      {t.display_name}
                    </option>
                  ))}
                </select>
              </label>
              <button type="button" className="btn-primary-glow" onClick={createNew} disabled={busy || !owner}>
                {busy ? 'Creating…' : 'Create job with screenshot'}
              </button>
            </div>
          ) : (
            <div className="share-existing">
              <input
                placeholder="search jobs by #, title, or type…"
                value={jobQuery}
                onChange={(e) => setJobQuery(e.target.value)}
              />
              <ul className="share-job-list">
                {jobMatches.map((j) => (
                  <li key={j.id}>
                    <button type="button" className="share-job" onClick={() => attachTo(j)} disabled={busy}>
                      <span className="share-job-id">#{j.id}</span>
                      <span className="share-job-title">{j.title || j.type}</span>
                      <span className="dim">{j.state}</span>
                    </button>
                  </li>
                ))}
                {jobMatches.length === 0 && <li className="dim">no matching jobs</li>}
              </ul>
            </div>
          )}
        </section>
      )}
    </div>
  )
}
