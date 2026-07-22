import { useEffect, useState } from 'react'

/**
 * A sanitized `cover_html` widget (spec §4.3, job #125): untrusted operator/agent
 * HTML rendered in a fully-sandboxed iframe. Inline it is a bounded preview (a
 * max-height card with a bottom fade) so a long cover can't dominate its host;
 * an expand affordance pops it out into a full-viewport lightbox rendering the
 * same content at full size through the *same* sandbox (no scripts, no
 * same-origin, no network — content rides in via `srcDoc`). Esc, a click on the
 * backdrop, or the close button dismiss the pop-out.
 *
 * One shared component so job covers and (once they land) agent-authored covers
 * on task summaries/reports behave identically everywhere they render.
 */
export function CoverWidget({ html, title = 'cover' }: { html: string; title?: string }) {
  const [open, setOpen] = useState(false)

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    // Lock background scroll while the lightbox owns the viewport.
    const prev = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => {
      document.removeEventListener('keydown', onKey)
      document.body.style.overflow = prev
    }
  }, [open])

  return (
    <>
      <div className="cover-preview">
        <iframe
          className="cover-frame"
          title={title}
          /* Presentational only (spec §4.3). Fully sandboxed: no scripts, no
             same-origin, no forms. Content rides in via srcDoc so nothing is
             fetched over the network. Same policy inline and popped out. */
          sandbox=""
          srcDoc={html}
        />
        {/* Transparent full-preview hit target: click anywhere on the collapsed
            preview to expand. The iframe below never receives the click. */}
        <button
          type="button"
          className="cover-hit"
          aria-label="expand cover to full view"
          onClick={() => setOpen(true)}
        />
        {/* Unobtrusive corner affordance, revealed on hover. */}
        <button
          type="button"
          className="cover-expand"
          title="expand"
          aria-label="expand cover to full view"
          onClick={() => setOpen(true)}
        >
          ⤢
        </button>
      </div>

      {open && (
        <div className="cover-lightbox" onClick={() => setOpen(false)} role="dialog" aria-modal="true" aria-label={title}>
          <div className="cover-lightbox-inner" onClick={(e) => e.stopPropagation()}>
            <button type="button" className="cover-close" aria-label="close" onClick={() => setOpen(false)}>
              ✕
            </button>
            <iframe className="cover-frame-full" title={title} sandbox="" srcDoc={html} />
          </div>
        </div>
      )}
    </>
  )
}
