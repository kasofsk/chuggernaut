import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { api } from '../api'
import { ProjectTabs } from './ProjectTabs'
import { TrainHeader } from './TrainHeader'
import { IconGlobe, IconLock } from './icons'

/**
 * The redesign-I page chrome (#161): a 'SOURCE CODE / {owner} / {project}'
 * masthead riding the bespoke train scene (#164), with a repo-visibility chip
 * and the section tabs (glow underline on the active tab lives in styles.css).
 * Shared by the project-scoped pages so the identity is consistent.
 */
export function ProjectHeader({ owner, project }: { owner: string; project: string }) {
  // Repo-visibility chip: linked-origin projects surface their remote host;
  // classic bare-repo projects read as a private local repo. Best-effort — a
  // 404 (classic) or any error just falls back to the private chip.
  const [repo, setRepo] = useState<{ label: string; linked: boolean } | null>(null)
  useEffect(() => {
    let ok = true
    api.origin(owner, project).then(
      (s) => {
        if (!ok) return
        if (s.origin?.github_repo) setRepo({ label: s.origin.github_repo, linked: true })
        else if (s.origin) setRepo({ label: 'Linked repo', linked: true })
        else setRepo({ label: 'Private repo', linked: false })
      },
      () => ok && setRepo({ label: 'Private repo', linked: false }),
    )
    return () => {
      ok = false
    }
  }, [owner, project])

  return (
    <>
      <header className="proj-header">
        <TrainHeader />
        <div className="proj-header-body">
          <div className="proj-eyebrow">Source code</div>
          <div className="proj-titleline">
            <h1 className="proj-title">
              <Link to="/" className="proj-owner">
                {owner}
              </Link>
              <span className="proj-sep">/</span>
              <Link to={`/p/${owner}/${project}`} className="proj-name">
                {project}
              </Link>
            </h1>
            <span className={`repo-chip${repo?.linked ? ' repo-chip-linked' : ''}`}>
              {repo?.linked ? <IconGlobe /> : <IconLock />}
              {repo?.label ?? 'Private repo'}
            </span>
          </div>
        </div>
      </header>
      <ProjectTabs owner={owner} project={project} />
    </>
  )
}
