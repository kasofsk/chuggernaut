import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type JobTypeDetail } from '../api'
import { ProjectPage } from '../components/ProjectPage'
import { SkeletonCards } from '../components/Skeleton'

type PromptEntry = {
  /** which task uses it: "work" or the evaluator's name */
  role: string
  kind: string
  path: string
  content: string | null
}

/**
 * Every prompt in the project, organized by the job type that uses it: the
 * work task's prompt and each agent/human evaluator's instructions, with
 * contents inline and outlinks to the file viewer.
 */
export function PromptsPage() {
  const { owner = '', project = '' } = useParams()
  const navigate = useNavigate()
  const [groups, setGroups] = useState<{ t: JobTypeDetail; prompts: PromptEntry[] }[]>([])
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setLoaded(false)
    async function load() {
      const summaries = await api.jobTypes(owner, project)
      const details = await Promise.all(summaries.map((s) => api.jobType(owner, project, s.name)))
      const cache = new Map<string, string | null>()
      const fetchContent = async (path: string) => {
        if (!cache.has(path)) {
          cache.set(
            path,
            await api.file(owner, project, path).then(
              (f) => f.content,
              () => null,
            ),
          )
        }
        return cache.get(path) ?? null
      }
      const out = []
      for (const t of details) {
        const jt = t.job_type
        if (!jt) continue
        const prompts: PromptEntry[] = []
        if (jt.work.prompt) {
          prompts.push({
            role: 'work',
            kind: jt.work.type,
            path: jt.work.prompt,
            content: await fetchContent(jt.work.prompt),
          })
        }
        for (const e of jt.eval) {
          if (e.prompt) {
            prompts.push({
              role: e.name,
              kind: e.type,
              path: e.prompt,
              content: await fetchContent(e.prompt),
            })
          }
        }
        out.push({ t, prompts })
      }
      return out
    }
    load().then(
      (gs) => {
        setGroups(gs)
        setLoaded(true)
        setError(null)
      },
      (e) => {
        if (e instanceof ApiError && e.status === 401) navigate('/login')
        else setError(e instanceof Error ? e.message : 'load failed')
      },
    )
  }, [owner, project, navigate])

  return (
    <ProjectPage owner={owner} project={project} error={error}>
      {!loaded && !error && <SkeletonCards titleWidth="10rem" lines={5} />}
      {loaded &&
        groups.map(({ t, prompts }) => (
          <section className="card" key={t.name} id={t.name}>
            <div className="row-head">
              <div>
                <h2 className="type-title">
                  <Link to={`/p/${owner}/${project}/job-types/${encodeURIComponent(t.name)}`}>
                    {t.job_type?.display_name || t.name}
                  </Link>
                </h2>
                <div className="dim type-slug">{t.name}</div>
              </div>
            </div>
            {prompts.length === 0 && <div className="dim">no prompts — command tasks only</div>}
            {prompts.map((p) => (
              <div key={`${p.role}:${p.path}`}>
                <h3 className="subhead">
                  {p.role} <span className="dim">· {p.kind} · </span>
                  <Link
                    className="type-slug"
                    to={`/p/${owner}/${project}/files?path=${encodeURIComponent(p.path)}`}
                  >
                    {p.path} ↗
                  </Link>
                </h3>
                {p.content !== null ? (
                  <pre className="prompt">{p.content}</pre>
                ) : (
                  <div className="error">{p.path}: not found on the default branch</div>
                )}
              </div>
            ))}
          </section>
        ))}
      {loaded && groups.length === 0 && !error && (
        <section className="card">
          <div className="dim">no job types yet</div>
        </section>
      )}
    </ProjectPage>
  )
}
