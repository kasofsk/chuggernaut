import { Component, type ReactNode } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import type { PluggableList } from 'unified'
import remarkGfm from 'remark-gfm'

// Agent/operator prose (channel posts, work/review/human summaries) is written
// markdown-ish — lists, code spans, the occasional heading. Render it safely:
// react-markdown escapes raw HTML by default (no rehype-raw here, so no HTML
// passthrough), and we drop images so nothing auto-loads a remote URL. Headings
// are scaled down in styles.css so an h1 inside a card doesn't read as a page
// title, and code blocks scroll inside their own container.

// GFM is not on by default (react-markdown dropped it in v5), and everything we
// render is written as GitHub-flavored markdown — every document under
// `docs/design/` has a pipe table, which CommonMark parses as one reflowed
// paragraph. The plugin also restores task lists, strikethrough and autolink
// literals, which agent prose uses freely.
const REMARK_PLUGINS = [remarkGfm]

// Malformed input never blanks a panel: on any parse/render throw we fall back
// to the raw source as plain (pre-wrapped) text.
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
