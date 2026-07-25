import type { ReactNode } from 'react'
import { ProjectHeader } from './ProjectHeader'

/**
 * The project-scoped page shell: the shared masthead + tabs (#161), then the
 * page's own content, with a load-error banner in one canonical place above it.
 * The card-list pages use this instead of repeating the wrapper, so the banner's
 * placement and the page padding are decided once.
 */
export function ProjectPage({
  owner,
  project,
  error,
  children,
}: {
  owner: string
  project: string
  /** load failure to surface above the content; null/undefined renders no banner */
  error?: string | null
  children: ReactNode
}) {
  return (
    <div className="page">
      <ProjectHeader owner={owner} project={project} />
      {error && <div className="error banner">{error}</div>}
      {children}
    </div>
  )
}
