import { Component, type ReactNode } from 'react'
import ReactMarkdown from 'react-markdown'

// Agent/operator prose (channel posts, work/review/human summaries) is written
// markdown-ish — lists, code spans, the occasional heading. Render it safely:
// react-markdown escapes raw HTML by default (no rehype-raw here, so no HTML
// passthrough), and we drop images so nothing auto-loads a remote URL. Headings
// are scaled down in styles.css so an h1 inside a card doesn't read as a page
// title, and code blocks scroll inside their own container.

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

export function Markdown({ text, className = '' }: { text: string; className?: string }) {
  return (
    <MarkdownBoundary source={text}>
      <div className={`md ${className}`.trim()}>
        <ReactMarkdown disallowedElements={['img']} unwrapDisallowed>
          {text}
        </ReactMarkdown>
      </div>
    </MarkdownBoundary>
  )
}
