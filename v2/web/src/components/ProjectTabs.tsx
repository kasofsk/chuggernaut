import { NavLink } from 'react-router-dom'

/** Project-level navigation: running jobs vs. the job type library. */
export function ProjectTabs({ owner, project }: { owner: string; project: string }) {
  return (
    <nav className="tabs">
      <NavLink to={`/p/${owner}/${project}`} end>
        Jobs
      </NavLink>
      <NavLink to={`/p/${owner}/${project}/library`}>Library</NavLink>
      <NavLink to={`/p/${owner}/${project}/tags`}>Tags</NavLink>
      <NavLink to={`/p/${owner}/${project}/files`}>Files</NavLink>
    </nav>
  )
}
