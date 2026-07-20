import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ApiError, api, type PlatformConfig } from '../api'

/** Read-only platform settings (admins only): fleet, agent defaults, paths. */
export function PlatformSettingsPage() {
  const navigate = useNavigate()
  const [cfg, setCfg] = useState<PlatformConfig | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [forbidden, setForbidden] = useState(false)

  useEffect(() => {
    api
      .platformConfig()
      .then((c) => {
        setCfg(c)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else if (e instanceof ApiError && e.status === 403) setForbidden(true)
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [navigate])

  const d = cfg?.dispatcher

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>Platform settings</h1>
      </header>
      {error && <div className="error banner">{error}</div>}
      {forbidden && (
        <section className="card">
          <div className="dim">Platform admin required.</div>
        </section>
      )}

      {cfg && (
        <>
          <section className="card">
            <div className="row-head">
              <h2>Worker nodes</h2>
            </div>
            {d ? (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>name</th>
                      <th>endpoint</th>
                      <th>slots</th>
                    </tr>
                  </thead>
                  <tbody>
                    {d.nodes.map((n) => (
                      <tr key={n.name}>
                        <td>{n.name}</td>
                        <td>
                          <code>{n.endpoint}</code>
                        </td>
                        <td>{n.slots}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="dim">Dispatcher offline.</div>
            )}
          </section>

          {d && (
            <>
              <section className="card">
                <div className="row-head">
                  <h2>Agent defaults</h2>
                </div>
                <dl className="meta">
                  <dt>provider</dt>
                  <dd>
                    <code>{d.agent_provider_default}</code>
                  </dd>
                  <dt>model</dt>
                  <dd>{d.agent_model_default ? <code>{d.agent_model_default}</code> : <span className="dim">—</span>}</dd>
                  <dt title="platform image for operator-dispatched triage agents (§1.2)">triage image</dt>
                  <dd>{d.triage_image ? <code>{d.triage_image}</code> : <span className="dim">— (triage unavailable)</span>}</dd>
                  <dt>secrets encryption</dt>
                  <dd>{d.secrets_encryption ? <span className="badge">on</span> : <span className="dim">off</span>}</dd>
                </dl>
              </section>

              <section className="card">
                <div className="row-head">
                  <h2>Paths &amp; endpoints</h2>
                </div>
                <dl className="meta">
                  <dt>repos root</dt>
                  <dd>
                    <code>{d.repos_root}</code>
                  </dd>
                  <dt>repo URL base</dt>
                  <dd>
                    <code>{d.repo_url_base}</code>
                  </dd>
                  <dt>NATS URL</dt>
                  <dd>
                    <code>{d.nats_url}</code>
                  </dd>
                  <dt>NATS URL (containers)</dt>
                  <dd>{d.nats_url_container ? <code>{d.nats_url_container}</code> : <span className="dim">—</span>}</dd>
                  <dt>channel binary</dt>
                  <dd>{d.channel_binary ? <code>{d.channel_binary}</code> : <span className="dim">—</span>}</dd>
                  <dt>hook binary</dt>
                  <dd>{d.hook_bin ? <code>{d.hook_bin}</code> : <span className="dim">—</span>}</dd>
                </dl>
              </section>
            </>
          )}

          <section className="card">
            <div className="row-head">
              <h2>Agent credentials</h2>
            </div>
            {cfg.agent_secrets.length > 0 ? (
              <ul className="name-list">
                {cfg.agent_secrets.map((s) => (
                  <li key={s}>
                    <code>{s}</code>
                  </li>
                ))}
              </ul>
            ) : (
              <div className="dim">None.</div>
            )}
          </section>

          <section className="card">
            <div className="row-head">
              <h2>Web push</h2>
            </div>
            <dl className="meta">
              <dt>VAPID key</dt>
              <dd>{cfg.vapid_public ? <span className="badge">configured</span> : <span className="dim">—</span>}</dd>
            </dl>
          </section>
        </>
      )}
    </div>
  )
}
