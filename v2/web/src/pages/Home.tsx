import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api, type Identity } from '../api'

// Project chooser: lists the projects on the identity's role map, plus a
// free-form owner/project field (platform admins have implicit access
// everywhere, so their role map may be empty).
export function Home() {
  const [identity, setIdentity] = useState<Identity | null>(null)
  const [slug, setSlug] = useState('')
  const navigate = useNavigate()

  useEffect(() => {
    api
      .me()
      .then(setIdentity)
      .catch(() => navigate('/login'))
  }, [navigate])

  if (!identity) return null

  const projects = Object.keys(identity.project_roles)
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
                <span className="dim"> — {identity.project_roles[p]}</span>
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
    </div>
  )
}
