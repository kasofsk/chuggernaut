import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api } from '../api'
import { ProjectPage } from '../components/ProjectPage'
import { SkeletonCards } from '../components/Skeleton'

/**
 * Knowledge tags: every .chug/tags/{tag}.md at default-branch HEAD, with its
 * contents — the meaning a tag carries when a job is created with it.
 */
export function TagsPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [tags, setTags] = useState<{ name: string; path: string; content: string }[]>([])
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setLoaded(false) // param change: back to skeletons until the new fetch lands
    api
      .tags(owner, project)
      // Read each tag back at the path the listing resolved to — the file
      // endpoint reads verbatim, and a project that predates the config root
      // still keeps its tags at the repo root (spec §1.1).
      .then((names) =>
        Promise.all(
          names.map(({ name, path }) =>
            api.file(owner, project, path).then(
              (f) => ({ name, path, content: f.content }),
              () => ({ name, path, content: '(unreadable)' }),
            ),
          ),
        ),
      )
      .then((ts) => {
        setTags(ts)
        setLoaded(true)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      })
  }, [owner, project, navigate])

  return (
    <ProjectPage owner={owner} project={project} error={error}>
      {!loaded && !error && <SkeletonCards titleWidth="8rem" lines={4} />}
      {loaded &&
        tags.map((t) => (
          <section className="card" key={t.name} id={t.name}>
            <div className="row-head">
              <div>
                <h2 className="type-title">{t.name}</h2>
                <div className="dim type-slug">
                  <Link to={`/p/${owner}/${project}/files?path=${encodeURIComponent(t.path)}`}>
                    {t.path}
                  </Link>
                </div>
              </div>
            </div>
            <pre className="prompt">{t.content}</pre>
          </section>
        ))}
      {loaded && tags.length === 0 && !error && (
        <section className="card">
          <div className="dim">
            no tags yet — a tag is a <code>.chug/tags/&#123;name&#125;.md</code> file on the default
            branch describing what the tag means; tag a job at creation to attach it
          </div>
        </section>
      )}
    </ProjectPage>
  )
}
