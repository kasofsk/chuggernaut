import { NavLink } from 'react-router-dom'

/** Project-level navigation: the operator inbox, running jobs, and the job type library. */
export function ProjectTabs({
  owner,
  project,
  inboxCount,
}: {
  owner: string
  project: string
  /** pending Human tasks; shown as a badge when known and non-zero */
  inboxCount?: number
}) {
  return (
    <nav className="tabs">
      <NavLink to={`/p/${owner}/${project}/inbox`}>
        Inbox
        {inboxCount !== undefined && inboxCount > 0 && (
          <span className="tab-badge">{inboxCount}</span>
        )}
      </NavLink>
      <NavLink to={`/p/${owner}/${project}`} end>
        Jobs
      </NavLink>
      <NavLink to={`/p/${owner}/${project}/job-types`}>Job types</NavLink>
      <NavLink to={`/p/${owner}/${project}/prompts`}>Prompts</NavLink>
      <NavLink to={`/p/${owner}/${project}/tags`}>Tags</NavLink>
      <NavLink to={`/p/${owner}/${project}/files`}>Files</NavLink>
    </nav>
  )
}
