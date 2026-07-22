import type { ReactNode } from 'react'
import { Link, NavLink, useLocation } from 'react-router-dom'
import {
  IconChat,
  IconCode,
  IconFile,
  IconGear,
  IconGrid,
  IconHome,
  IconServer,
  IconTag,
} from './icons'

/**
 * App shell (#161): the slim left icon nav rail + main content region. The rail
 * is contextual — inside a project (`/p/:owner/:project…`) it links the section
 * icons (jobs, job types, prompts, tags, files, settings); elsewhere it shows a
 * reduced home/cluster set. Hidden on /login. On narrow viewports the rail drops
 * to a bottom bar (see styles.css) so nothing steals horizontal room.
 */
export function AppShell({ children }: { children: ReactNode }) {
  const { pathname } = useLocation()
  if (pathname === '/login') return <>{children}</>

  // Derive project context from the path so the rail can be a single global
  // element rather than threaded through every page.
  const m = pathname.match(/^\/p\/([^/]+)\/([^/]+)/)
  const owner = m ? decodeURIComponent(m[1]) : null
  const project = m ? decodeURIComponent(m[2]) : null
  const base = owner && project ? `/p/${owner}/${project}` : null

  return (
    <div className="shell">
      <nav className="rail" aria-label="Primary">
        <Link className="rail-logo" to="/" title="Chuggernaut — all projects" aria-label="Home">
          <img src="/favicon.png" alt="" width={26} height={26} />
        </Link>
        <div className="rail-icons">
          {base ? (
            <>
              <RailIcon to={base} end label="Jobs">
                <IconCode />
              </RailIcon>
              <RailIcon to={`${base}/job-types`} label="Job types">
                <IconGrid />
              </RailIcon>
              <RailIcon to={`${base}/prompts`} label="Prompts">
                <IconChat />
              </RailIcon>
              <RailIcon to={`${base}/tags`} label="Tags">
                <IconTag />
              </RailIcon>
              <RailIcon to={`${base}/files`} label="Files">
                <IconFile />
              </RailIcon>
              <RailIcon to={`${base}/settings`} label="Settings">
                <IconGear />
              </RailIcon>
            </>
          ) : (
            <>
              <RailIcon to="/" end label="Projects">
                <IconHome />
              </RailIcon>
              <RailIcon to="/cluster" label="Cluster">
                <IconServer />
              </RailIcon>
              <RailIcon to="/settings" label="Platform settings">
                <IconGear />
              </RailIcon>
            </>
          )}
        </div>
        <Link className="rail-avatar" to="/settings" title="Platform settings" aria-label="Account">
          {(owner?.[0] ?? 'C').toUpperCase()}
        </Link>
      </nav>
      <div className="shell-main">{children}</div>
    </div>
  )
}

function RailIcon({
  to,
  end,
  label,
  children,
}: {
  to: string
  end?: boolean
  label: string
  children: ReactNode
}) {
  return (
    <NavLink className="rail-icon" to={to} end={end} title={label} aria-label={label}>
      {children}
      <span className="rail-tip">{label}</span>
    </NavLink>
  )
}
