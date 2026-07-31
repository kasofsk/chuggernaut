import { Component, type ReactNode } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import type { PluggableList } from 'unified'
import remarkGfm from 'remark-gfm'

const REMARK_PLUGINS = [remarkGfm]

class MarkdownBoundary extends Component<
  { source: string; children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false }
  static getDerivedStateFromError() {
    return { failed: true }
  }
  render() {
    if (this.state.failed) {
      return <div className="md md-fallback">{this.props.source}</div>
    }
    return this.props.children
  }
}

/** `components` and `rehypePlugins` exist for rendering whole repo documents,
 *  where a relative link has to resolve against the document rather than the
 *  SPA route it happens to be shown under, and a `#section` link needs a
 *  heading to land on (see `DocMarkdown`). Prose panels pass neither and keep
 *  the plain renderer. */
export function Markdown({
  text,
  className = '',
  components,
  rehypePlugins,
}: {
  text: string
  className?: string
  components?: Components
  rehypePlugins?: PluggableList
}) {
  return (
    <MarkdownBoundary source={text}>
      <div className={`md ${className}`.trim()}>
        <ReactMarkdown
          remarkPlugins={REMARK_PLUGINS}
          rehypePlugins={rehypePlugins}
          disallowedElements={['img']}
          unwrapDisallowed
          components={components}
        >
          {text}
        </ReactMarkdown>
      </div>
    </MarkdownBoundary>
  )
}
