import { useMemo } from 'react'
import { Link } from 'react-router-dom'
import type { Components } from 'react-markdown'
import rehypeSlug from 'rehype-slug'
import { Markdown } from './Markdown'

// A whole repo document, rendered. The difference from plain `Markdown` is the
// links: a document's hrefs are written relative to the document's own place in
// the tree (`[#309](./309-host-native-execution.md)`), not to the SPA route it
// happens to be displayed under. Left verbatim the browser resolves them
// against the current URL and the operator lands on a route that does not
// exist — a silent bounce to the projects home. Cross-document links are the
// browsable half of the docs wiki (job #88), so they resolve here instead.

const ABSOLUTE = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i

// The other half of an intra-document link: `docs/design/321-job-groups.md`
// links its own decisions as `#decision-7-the-api-surface`, written against
// GitHub's heading slugs. `rehype-slug` uses the same github-slugger, so the
// anchors those documents already contain land where they were written to.
const REHYPE_PLUGINS = [rehypeSlug]

/**
 * Where a link inside the document at `docPath` points, or null when the href
 * is not a repo path — an absolute URL or an in-page anchor is left to the
 * browser. `base` is the project's file-browser route.
 */
export function docLinkTarget(base: string, docPath: string, href: string): string | null {
  if (!href || href.startsWith('#') || ABSOLUTE.test(href)) return null
  // The fragment names a heading in the *target* document, not a path segment,
  // so it rides along after the query rather than through `encodeURIComponent`
  // — the file browser renders through this same component, so it lands.
  const hash = href.indexOf('#')
  const rel = hash < 0 ? href : href.slice(0, hash)
  const frag = hash < 0 ? '' : href.slice(hash)
  if (!rel) return null
  const dir = docPath.slice(0, docPath.lastIndexOf('/') + 1)
  // A leading slash means repo root; otherwise start from the document's own
  // directory and let `..` walk up, the way git resolves the same text.
  const parts = rel.startsWith('/') ? [] : dir.split('/').filter(Boolean)
  for (const seg of rel.split('/')) {
    if (!seg || seg === '.') continue
    if (seg === '..') parts.pop()
    else parts.push(seg)
  }
  if (parts.length === 0) return null
  const last = parts[parts.length - 1]
  // No extension on the final segment (or a trailing slash) reads as a
  // directory, and the browser takes those on `?dir=` instead.
  const kind = rel.endsWith('/') || !last.includes('.') ? 'dir' : 'path'
  return `${base}?${kind}=${encodeURIComponent(parts.join('/'))}${kind === 'path' ? frag : ''}`
}

export function DocMarkdown({
  owner,
  project,
  path,
  text,
  className = '',
}: {
  owner: string
  project: string
  path: string
  text: string
  className?: string
}) {
  const components = useMemo<Components>(() => {
    const base = `/p/${owner}/${project}/files`
    return {
      a({ href, children }) {
        const target = href ? docLinkTarget(base, path, href) : null
        // Anything not a repo path — an absolute URL, a `#section` anchor —
        // renders exactly as the default renderer would.
        if (!target) return <a href={href}>{children}</a>
        return <Link to={target}>{children}</Link>
      },
    }
  }, [owner, project, path])

  return (
    <Markdown
      text={text}
      className={`md-doc ${className}`.trim()}
      components={components}
      rehypePlugins={REHYPE_PLUGINS}
    />
  )
}
