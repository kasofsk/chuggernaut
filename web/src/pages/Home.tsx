import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api, type Identity } from '../api'

// Project chooser: lists the projects visible to the caller (platform
// admins see the whole registry), plus a free-form owner/project field.
export function Home() {
  const [identity, setIdentity] = useState<Identity | null>(null)
  const [projects, setProjects] = useState<string[]>([])
  const [slug, setSlug] = useState('')
  const navigate = useNavigate()

  useEffect(() => {
    api
      .me()
      .then(setIdentity)
      .catch(() => navigate('/login'))
    api
      .projects()
      .then(setProjects)
      .catch(() => {})
  }, [navigate])

  if (!identity) return null

  const last = localStorage.getItem('last-project')

  function open(p: string) {
    localStorage.setItem('last-project', p)
    navigate(`/p/${p}`)
  }

  return (
    <div className="page">
      <header className="topbar">
        <h1>Chuggernaut</h1>
        <span className="who">
          {identity.sub}
          {identity.platform_admin ? ' (admin)' : ''}
        </span>
      </header>
      <div className="card">
        <h2>Projects</h2>
        {projects.length > 0 && (
          <ul className="project-list">
            {projects.map((p) => (
              <li key={p}>
                <button className="link" onClick={() => open(p)}>
                  {p}
                </button>
                <span className="dim">
                  {' '}
                  — {identity.project_roles[p] ?? (identity.platform_admin ? 'admin' : '')}
                </span>
              </li>
            ))}
          </ul>
        )}
        {last && !projects.includes(last) && (
          <p>
            <button className="link" onClick={() => open(last)}>
              {last}
            </button>
            <span className="dim"> — recent</span>
          </p>
        )}
        <form
          onSubmit={(e) => {
            e.preventDefault()
            if (slug.includes('/')) open(slug)
          }}
        >
          <input
            placeholder="owner/project"
            value={slug}
            onChange={(e) => setSlug(e.target.value)}
          />
          <button type="submit">open</button>
        </form>
      </div>

      {identity.platform_admin && <NewProject onCreated={open} />}
    </div>
  )
}

/**
 * Platform-admin project creation: bare repo, SSH hook, and the Code starter
 * template (jobs/code.yaml + reusable tasks/) seeded as the first commit.
 */
function NewProject({ onCreated }: { onCreated: (slug: string) => void }) {
  const [owner, setOwner] = useState('')
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  return (
    <div className="card">
      <h2>New project</h2>
      <p className="dim">
        Creates the repo and seeds it with the Code starter (an agent
        implements the job ticket, a second agent reviews it) plus reusable
        tasks under <code>tasks/</code>. The owner is a namespace (org-style
        grouping label), not a user — users and roles are managed separately.
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault()
          const o = owner.trim()
          const n = name.trim()
          if (!o || !n) return
          setBusy(true)
          api.createProject(o, n).then(
            () => onCreated(`${o}/${n}`),
            (err) => {
              setBusy(false)
              setError(err instanceof Error ? err.message : 'create failed')
            },
          )
        }}
      >
        <input placeholder="owner" value={owner} onChange={(e) => setOwner(e.target.value)} />{' '}
        <input placeholder="name" value={name} onChange={(e) => setName(e.target.value)} />{' '}
        <button type="submit" disabled={busy || !owner.trim() || !name.trim()}>
          create project
        </button>
      </form>
      {error && <div className="error">{error}</div>}
    </div>
  )
}
