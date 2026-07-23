import type { ReactNode } from 'react'

/**
 * App shell: content region only. The #161 icon rail was removed (redundant
 * with the header tabs); Cluster and platform settings moved into the
 * ProjectTabs row so every page keeps one consistent nav.
 */
export function AppShell({ children }: { children: ReactNode }) {
  return <div className="shell-main">{children}</div>
}
