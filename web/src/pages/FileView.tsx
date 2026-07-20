import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { ApiError, api } from '../api'
import { ProjectTabs } from '../components/ProjectTabs'
import { YamlView } from '../components/YamlView'

type TreeEntry = { path: string; type: string; size: number | null }

/**
 * Repo browser at default-branch HEAD: GitHub-style directory listing
 * (?dir=...) and file view (?path=...). One recursive tree fetch; navigation
 * is client-side.
 */
export function FileViewPage() {
  const { owner = '', project = '' } = useParams()
  const [params] = useSearchParams()
  const path = params.get('path') ?? ''
  const dir = params.get('dir') ?? ''
  const navigate = useNavigate()
  const [tree, setTree] = useState<{ branch: string; ref: string; entries: TreeEntry[] } | null>(null)
  const [file, setFile] = useState<{ path: string; ref: string; content: string } | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api.tree(owner, project).then(
      (t) => {
        setTree(t)
        setError(null)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      },
    )
  }, [owner, project, navigate])

  useEffect(() => {
    if (!path) {
      setFile(null)
      return
    }
    api.file(owner, project, path).then(
      (f) => setFile(f),
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else
          setError(
            e instanceof ApiError && e.status === 404
              ? `${path}: not found on the default branch`
              : e instanceof Error
                ? e.message
                : 'load failed',
          )
      },
    )
  }, [owner, project, path, navigate])

  const base = `/p/${owner}/${project}/files`
  const crumbs = (p: string) => {
    const parts = p.split('/').filter(Boolean)
    return (
      <span className="type-slug">
        <Link to={base}>{project}</Link>
        {parts.map((seg, i) => {
          const prefix = parts.slice(0, i + 1).join('/')
          const isLast = i === parts.length - 1
          return (
            <span key={prefix}>
              {' / '}
              {isLast ? seg : <Link to={`${base}?dir=${encodeURIComponent(prefix)}`}>{seg}</Link>}
            </span>
          )
        })}
      </span>
    )
  }

  // Immediate children of `dir`, dirs first — the tree fetch is recursive.
  const children = (() => {
    if (!tree) return []
    const prefix = dir ? `${dir}/` : ''
    const dirs = new Set<string>()
    const files: TreeEntry[] = []
    for (const e of tree.entries) {
      if (!e.path.startsWith(prefix)) continue
      const rest = e.path.slice(prefix.length)
      if (!rest) continue
      const slash = rest.indexOf('/')
      if (slash >= 0) dirs.add(rest.slice(0, slash))
      else if (e.type === 'blob') files.push(e)
      else dirs.add(rest)
    }
    return [
      ...[...dirs].sort().map((d) => ({ kind: 'dir' as const, name: d })),
      ...files
        .sort((a, b) => a.path.localeCompare(b.path))
        .map((f) => ({ kind: 'file' as const, name: f.path.slice(prefix.length), size: f.size })),
    ]
  })()

  return (
    <div className="page">
      <header className="topbar">
        <Link to="/">Chuggernaut</Link>
        <h1>
          {owner}/{project}
        </h1>
        {tree && (
          <span className="dim">
            {tree.branch} @ {tree.ref.slice(0, 10)}
          </span>
        )}
      </header>
      <ProjectTabs owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}

      {path ? (
        <section className="card">
          <h2>{crumbs(path)}</h2>
          {file ? (
            /\.ya?ml$/.test(file.path) ? (
              <YamlView yaml={file.content} full />
            ) : (
              <pre className="prompt yaml-full">{file.content}</pre>
            )
          ) : (
            !error && 'loading…'
          )}
        </section>
      ) : (
        <section className="card">
          <h2>{crumbs(dir)}</h2>
          <div className="table-scroll">
            <table className="jobs">
            <tbody>
              {dir && (
                <tr>
                  <td>
                    <Link
                      to={
                        dir.includes('/')
                          ? `${base}?dir=${encodeURIComponent(dir.slice(0, dir.lastIndexOf('/')))}`
                          : base
                      }
                    >
                      ..
                    </Link>
                  </td>
                  <td></td>
                </tr>
              )}
              {children.map((c) => (
                <tr key={c.name}>
                  <td>
                    {c.kind === 'dir' ? (
                      <Link to={`${base}?dir=${encodeURIComponent(dir ? `${dir}/${c.name}` : c.name)}`}>
                        📁 {c.name}/
                      </Link>
                    ) : (
                      <Link to={`${base}?path=${encodeURIComponent(dir ? `${dir}/${c.name}` : c.name)}`}>
                        {c.name}
                      </Link>
                    )}
                  </td>
                  <td className="dim" style={{ textAlign: 'right' }}>
                    {c.kind === 'file' && c.size != null ? `${c.size} B` : ''}
                  </td>
                </tr>
              ))}
              {children.length === 0 && tree && (
                <tr>
                  <td className="dim">empty</td>
                  <td></td>
                </tr>
              )}
            </tbody>
            </table>
          </div>
        </section>
      )}
    </div>
  )
}
