import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type JobTypeDetail } from '../api'
import { ProjectTabs } from '../components/ProjectTabs'
import { EvaluatorTable } from '../components/EvaluatorTable'
import { YamlView } from '../components/YamlView'

/**
 * The job type library: every jobs/{type}.yaml at default-branch HEAD, shown
 * as the platform sees it (defaults merged). Jobs are instances of these; the
 * work phase and the evaluation phase both run tasks declared here.
 */
export function LibraryPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [types, setTypes] = useState<JobTypeDetail[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api
      .jobTypes(owner, project)
      .then((summaries) => Promise.all(summaries.map((s) => api.jobType(owner, project, s.name))))
      .then((ts) => {
        setTypes(ts)
        setError(null)
        // Cards render after the fetch, so a #type anchor (e.g. the create
        // form's peek links) must scroll once they exist.
        const anchor = decodeURIComponent(window.location.hash.slice(1))
        if (anchor) {
          requestAnimationFrame(() => document.getElementById(anchor)?.scrollIntoView())
        }
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
        <h1>
          {owner}/{project}
        </h1>
      </header>
      <ProjectTabs owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}

      {types.map((t) => (
        <TypeCard key={t.name} t={t} owner={owner} project={project} />
      ))}
      {types.length === 0 && !error && (
        <section className="card">
          <div className="dim">
            no job types yet — add <code>jobs/&#123;type&#125;.yaml</code> on the default branch
          </div>
        </section>
      )}
    </div>
  )
}

function TypeCard({
  t,
  owner,
  project,
  expanded = false,
}: {
  t: JobTypeDetail
  owner: string
  project: string
  /** dedicated type page: YAML shown in full, title not a link */
  expanded?: boolean
}) {
  const jt = t.job_type
  return (
    <section className="card" id={t.name}>
      <div className="row-head">
        <div>
          <h2 className="type-title">
            {expanded ? (
              jt?.display_name || t.name
            ) : (
              <Link to={`/p/${owner}/${project}/job-types/${encodeURIComponent(t.name)}`}>
                {jt?.display_name || t.name}
              </Link>
            )}
          </h2>
          <div className="dim type-slug">{t.name}</div>
        </div>
        <Link to={`/p/${owner}/${project}/jobs/new?type=${encodeURIComponent(t.name)}`}>
          <button>new job</button>
        </Link>
      </div>
      {t.errors.length > 0 && (
        <div className="error banner">
          {t.errors.map((e, i) => (
            <div key={i}>{e}</div>
          ))}
        </div>
      )}
      {jt?.description && <p className="dim">{jt.description}</p>}
      {jt && (
        <>
          <h3 className="subhead">1 · Work</h3>
          <dl className="meta">
            <dt>task</dt>
            <dd>
              {jt.work.type}
              {jt.work.type === 'command' ? (
                <>
                  {' · '}
                  <code>{jt.work.run}</code>
                </>
              ) : (
                <>
                  {' · '}
                  {jt.work.prompt && (
                    <Link to={`/p/${owner}/${project}/files?path=${encodeURIComponent(jt.work.prompt)}`}>
                      {jt.work.prompt} ↗
                    </Link>
                  )}
                </>
              )}
              {jt.work.model ? ` · ${jt.work.model}` : ''}
              {jt.work.review ? ' · inline review' : ''}
            </dd>
            {jt.image && (
              <>
                <dt>image</dt>
                <dd>{jt.image}</dd>
              </>
            )}
            <dt>budgets</dt>
            <dd className="dim">
              work_retries {jt.work_retries ?? 0} · eval_retries {jt.eval_retries ?? 1} ·
              rework_budget {jt.rework_budget ?? 0}
              {jt.job_deadline ? ` · deadline ${jt.job_deadline}` : ''}
              {jt.resources?.task_timeout ? ` · task_timeout ${jt.resources.task_timeout}` : ''}
            </dd>
            {(jt.work.secrets.length > 0 || jt.vars.length > 0) && (
              <>
                <dt>env</dt>
                <dd className="dim">
                  {[...jt.work.secrets.map((s) => `secret ${s}`), ...jt.vars.map((v) => `var ${v}`)].join(', ')}
                </dd>
              </>
            )}
          </dl>
          <h3 className="subhead">2 · Evaluation</h3>
          <EvaluatorTable owner={owner} project={project} evaluators={jt.eval} />
          <h3 className="subhead">3 · Wrap-up</h3>
          <div className="dim">
            {jt.wrap_up.type === 'none'
              ? 'nothing to merge — the job is Done when evaluation passes; the branch is scratch'
              : 'squash-merge the job branch to the default branch (merge queue + merge gate)'}
          </div>
        </>
      )}
      {expanded ? (
        <>
          <h3 className="subhead">
            jobs/{t.name}.yaml <span className="dim">· at {t.ref.slice(0, 10)}</span>
          </h3>
          <YamlView yaml={t.yaml} full />
        </>
      ) : (
        <details className="yaml">
          <summary className="dim">
            jobs/{t.name}.yaml <span className="dim">· at {t.ref.slice(0, 10)}</span>
          </summary>
          <YamlView yaml={t.yaml} />
        </details>
      )}
    </section>
  )
}


/** Dedicated page for one job type: the card plus the full YAML, expanded. */
export function JobTypePage() {
  const { owner = '', project = '', name = '' } = useParams()
  const navigate = useNavigate()
  const [t, setT] = useState<JobTypeDetail | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api.jobType(owner, project, name).then(
      (d) => {
        setT(d)
        setError(null)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      },
    )
  }, [owner, project, name, navigate])

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <Link to={`/p/${owner}/${project}`}>
          {owner}/{project}
        </Link>
        <Link to={`/p/${owner}/${project}/job-types`}>Job types</Link>
      </header>
      <ProjectTabs owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      {t ? <TypeCard t={t} owner={owner} project={project} expanded /> : !error && 'loading…'}
    </div>
  )
}
