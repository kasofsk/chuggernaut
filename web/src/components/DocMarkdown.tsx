import { useMemo } from 'react'
import { Link } from 'react-router-dom'
import type { Components } from 'react-markdown'
import rehypeSlug from 'rehype-slug'
import { Markdown } from './Markdown'

const ABSOLUTE = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i

const REHYPE_PLUGINS = [rehypeSlug]

/**
 * Where a link inside the document at `docPath` points, or null when the href
 * is not a repo path — an absolute URL or an in-page anchor is left to the
 * browser. `base` is the project's file-browser route.
 */
export function docLinkTarget(base: string, docPath: string, href: string): string | null {
  if (!href || href.startsWith('#') || ABSOLUTE.test(href)) return null
  const hash = href.indexOf('#')
  const rel = hash < 0 ? href : href.slice(0, hash)
  const frag = hash < 0 ? '' : href.slice(hash)
  if (!rel) return null
  const dir = docPath.slice(0, docPath.lastIndexOf('/') + 1)
  const parts = rel.startsWith('/') ? [] : dir.split('/').filter(Boolean)
  for (const seg of rel.split('/')) {
    if (!seg || seg === '.') continue
    if (seg === '..') parts.pop()
    else parts.push(seg)
  }
  if (parts.length === 0) return null
  const last = parts[parts.length - 1]
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
