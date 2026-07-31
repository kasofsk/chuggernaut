import { Fragment, useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type ProjectConfig } from '../api'
import { ProjectPage } from '../components/ProjectPage'
import { SkeletonLines } from '../components/Skeleton'

/** Read-only project settings: git origin, environment vars, secret names. */
export function SettingsPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [cfg, setCfg] = useState<ProjectConfig | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setCfg(null)
    api
      .projectConfig(owner, project)
      .then((c) => {
        setCfg(c)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, navigate])

  return (
    <ProjectPage owner={owner} project={project} error={error}>
      {!cfg &&
        !error &&
        ['Git origin', 'Environment variables', 'Secrets'].map((h) => (
          <section className="card" key={h}>
            <div className="row-head">
              <h2>{h}</h2>
            </div>
            <SkeletonLines n={3} />
          </section>
        ))}
      {cfg && (
        <>
          <section className="card">
            <div className="row-head">
              <h2>Git origin</h2>
            </div>
            {cfg.origin ? (
              <dl className="meta">
                <dt>remote</dt>
                <dd>
                  <code>{cfg.origin.url}</code>
                </dd>
                <dt>default branch</dt>
                <dd>
                  <code>{cfg.origin.main_branch}</code>
                </dd>
                <dt>GitHub repo</dt>
                <dd>{cfg.origin.github_repo ? <code>{cfg.origin.github_repo}</code> : <Absent />}</dd>
                <dt>deploy key</dt>
                <dd>
                  <Presence ok={cfg.origin_credentials.deploy_key} />
                </dd>
                <dt>PAT</dt>
                <dd>
                  <Presence ok={cfg.origin_credentials.pat} />
                </dd>
              </dl>
            ) : (
              <div className="dim">Not linked.</div>
            )}
          </section>

          <section className="card">
            <div className="row-head">
              <h2>Environment variables</h2>
            </div>
            {cfg.vars.length > 0 ? (
              <dl className="meta">
                {cfg.vars.map((v) => (
                  <Fragment key={v.name}>
                    <dt>{v.name}</dt>
                    <dd>
                      <code>{v.value}</code>
                    </dd>
                  </Fragment>
                ))}
              </dl>
            ) : (
              <div className="dim">None.</div>
            )}
          </section>

          <section className="card">
            <div className="row-head">
              <h2>Secrets</h2>
            </div>
            {cfg.secrets.length > 0 ? (
              <ul className="name-list">
                {cfg.secrets.map((s) => (
                  <li key={s}>
                    <code>{s}</code>
                  </li>
                ))}
              </ul>
            ) : (
              <div className="dim">None.</div>
            )}
          </section>
        </>
      )}
    </ProjectPage>
  )
}

function Presence({ ok }: { ok: boolean }) {
  return ok ? <span className="badge">present</span> : <span className="dim">missing</span>
}

function Absent() {
  return <span className="dim">—</span>
}
