import { Link, NavLink } from 'react-router-dom'
import { ProjectTabs } from './ProjectTabs'
import { TrainHeader } from './TrainHeader'

/**
 * The redesign-I page chrome (#161): a 'SOURCE CODE / {owner} / {project}'
 * masthead riding the bespoke train scene (#164), with a repo-visibility chip
 * and the section tabs (glow underline on the active tab lives in styles.css).
 * Shared by the project-scoped pages so the identity is consistent.
 */
export function ProjectHeader({ owner, project }: { owner: string; project: string }) {
  return (
    <>
      <header className="proj-header">
        <TrainHeader />
        <div className="proj-header-body">
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
          </div>
        </div>
        <span className="proj-header-links">
          <NavLink to="/cluster">Cluster</NavLink>
          <NavLink to="/settings">Platform</NavLink>
        </span>
      </header>
      <ProjectTabs owner={owner} project={project} />
    </>
  )
}
