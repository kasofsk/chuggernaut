import type { CSSProperties } from 'react'

/**
 * Shimmering placeholders for a page's initial load (styles: `.skel` at the
 * end of styles.css). Only the first fetch skeletons — polling/SSE refreshes
 * keep the previous content on screen.
 */

export function Skeleton({
  width = '100%',
  height = '1em',
  style,
}: {
  width?: string | number
  height?: string | number
  style?: CSSProperties
}) {
  return <span className="skel" style={{ width, height, ...style }} aria-hidden="true" />
}

// Varied line widths so stacked lines read as prose, not bars.
const LINE_WIDTHS = ['100%', '92%', '74%', '86%', '58%']

/** n stacked text-like lines of varied widths. */
export function SkeletonLines({ n = 3 }: { n?: number }) {
  return (
    <div className="skel-lines" aria-hidden="true">
      {Array.from({ length: n }, (_, i) => (
        <Skeleton key={i} width={LINE_WIDTHS[i % LINE_WIDTHS.length]} height="0.9em" />
      ))}
    </div>
  )
}

/** n placeholder cards, each a title bar over `lines` text lines — the shape a
 *  page of stacked content cards (tags, prompts) loads into. */
export function SkeletonCards({
  n = 2,
  titleWidth = '8rem',
  lines = 4,
}: {
  n?: number
  titleWidth?: string
  lines?: number
}) {
  return (
    <>
      {Array.from({ length: n }, (_, i) => (
        <section className="card" key={i}>
          <Skeleton width={titleWidth} height="1.2em" />
          <SkeletonLines n={lines} />
        </section>
      ))}
    </>
  )
}

/** n rows of cells sized by `widths`, mimicking a table's shape. */
export function SkeletonTable({ rows = 5, widths }: { rows?: number; widths: string[] }) {
  return (
    <div className="skel-table" aria-hidden="true">
      {Array.from({ length: rows }, (_, i) => (
        <div className="skel-table-row" key={i}>
          {widths.map((w, j) => (
            <Skeleton key={j} width={w} height="0.9em" />
          ))}
        </div>
      ))}
    </div>
  )
}
