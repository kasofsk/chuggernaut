import { useEffect, useState } from 'react'

/**
 * A sanitized `cover_html` widget (spec §4.3, jobs #125/#143): untrusted
 * operator/agent HTML rendered in a fully-sandboxed iframe. Inline it is a
 * bounded preview (a max-height card with a bottom fade) so a long cover can't
 * dominate its host; an expand affordance pops it out into a full-viewport
 * lightbox rendering the same content at full size through the *same* sandbox.
 * Esc, a click on the backdrop, or the close button dismiss the pop-out.
 *
 * Containment is at render, not ingest: cover HTML is stored verbatim (only
 * size-capped) and neutralized here, the single shared choke point. `sandbox=""`
 * blocks scripts, forms, and same-origin access; the injected CSP
 * (`default-src 'none'`, only inline styles + `data:` images/fonts) additionally
 * blocks the passive external fetches a bare sandbox still allows — an external
 * `<img>`, a CSS `url()`/`@import` — so a hostile cover can neither execute nor
 * phone home. Content rides in via `srcDoc`, so nothing else is fetched.
 *
 * One shared component so job covers and agent-authored covers on task
 * summaries/reports behave identically everywhere they render.
 */
export function CoverWidget({ html, title = 'cover' }: { html: string; title?: string }) {
  const [open, setOpen] = useState(false)
  // Presentational-only CSP (job #143): no network of any kind, inline styles
  // and data: images/fonts only. Prepended so it is parsed before any resource.
  const sandboxed = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; font-src data:; style-src 'unsafe-inline'">${html}`

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
             same-origin, no forms. The injected CSP blocks all network fetches
             (job #143). Content rides in via srcDoc. Same policy popped out. */
          sandbox=""
          srcDoc={sandboxed}
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
            <iframe className="cover-frame-full" title={title} sandbox="" srcDoc={sandboxed} />
          </div>
        </div>
      )}
    </>
  )
}
