import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api } from '../api'
import { ProjectHeader } from '../components/ProjectHeader'
import { Skeleton, SkeletonLines } from '../components/Skeleton'

/**
 * Knowledge tags: every tags/{tag}.md at default-branch HEAD, with its
 * contents — the meaning a tag carries when a job is created with it.
 */
export function TagsPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [tags, setTags] = useState<{ name: string; content: string }[]>([])
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setLoaded(false) // param change: back to skeletons until the new fetch lands
    api
      .tags(owner, project)
      .then((names) =>
        Promise.all(
          names.map((name) =>
            api.file(owner, project, `tags/${name}.md`).then(
              (f) => ({ name, content: f.content }),
              () => ({ name, content: '(unreadable)' }),
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
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}

      {!loaded &&
        !error &&
        [0, 1].map((i) => (
          <section className="card" key={i}>
            <Skeleton width="8rem" height="1.2em" />
            <SkeletonLines n={4} />
          </section>
        ))}
      {loaded &&
        tags.map((t) => (
          <section className="card" key={t.name} id={t.name}>
            <div className="row-head">
              <div>
                <h2 className="type-title">{t.name}</h2>
                <div className="dim type-slug">
                  <Link to={`/p/${owner}/${project}/files?path=${encodeURIComponent(`tags/${t.name}.md`)}`}>
                    tags/{t.name}.md
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
            no tags yet — a tag is a <code>tags/&#123;name&#125;.md</code> file on the default
            branch describing what the tag means; tag a job at creation to attach it
          </div>
        </section>
      )}
    </div>
  )
}
