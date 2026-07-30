import { NavLink } from 'react-router-dom'

/** Project-level navigation, plus platform-level links right-aligned. */
export function ProjectTabs({ owner, project }: { owner: string; project: string }) {
  return (
    <nav className="tabs">
      <NavLink to={`/p/${owner}/${project}`} end>
        Jobs
      </NavLink>
      <NavLink to={`/p/${owner}/${project}/job-types`}>Job types</NavLink>
      <NavLink to={`/p/${owner}/${project}/prompts`}>Prompts</NavLink>
      <NavLink to={`/p/${owner}/${project}/designs`}>Designs</NavLink>
      <NavLink to={`/p/${owner}/${project}/files`}>Files</NavLink>
      <NavLink to={`/p/${owner}/${project}/stats`}>Stats</NavLink>
      <NavLink to={`/p/${owner}/${project}/deploys`}>Deploys</NavLink>
      <NavLink to={`/p/${owner}/${project}/settings`}>Settings</NavLink>
    </nav>
  )
}
