import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { ApiError, api } from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { DocMarkdown } from '../components/DocMarkdown'
import { YamlView } from '../components/YamlView'
import { SkeletonLines, SkeletonTable } from '../components/Skeleton'

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
    setTree(null) // param change: back to skeletons until the new fetch lands
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
    setFile(null) // path change: back to the skeleton until the new fetch lands
    if (!path) return
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
      <ProjectHeader owner={owner} project={project} />
      {tree && (
        <div className="dim" style={{ margin: '4px 0 8px' }}>
          {tree.branch} @ {tree.ref.slice(0, 10)}
        </div>
      )}
      {error && <div className="error banner">{error}</div>}

      {path ? (
        <section className="card">
          <h2>{crumbs(path)}</h2>
          {file ? (
            /\.ya?ml$/.test(file.path) ? (
              <YamlView yaml={file.content} full />
            ) : /\.md$/.test(file.path) ? (
              // The docs tree reads as a wiki (job #88): markdown goes through
              // the renderer the job pages already use, rather than a <pre>,
              // and its relative links stay inside the browser.
              <DocMarkdown owner={owner} project={project} path={file.path} text={file.content} />
            ) : (
              <pre className="prompt yaml-full">{file.content}</pre>
            )
          ) : (
            !error && <SkeletonLines n={8} />
          )}
        </section>
      ) : (
        <section className="card">
          <h2>{crumbs(dir)}</h2>
          {!tree && !error && <SkeletonTable rows={6} widths={['14rem', '4rem']} />}
          {tree && (
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
          )}
        </section>
      )}
    </div>
  )
}
