import { useLayoutEffect, useRef, useState } from 'react'

const DAY_LABEL_WIDTH_PX = 54

/**
 * Indexes of the day columns that get a tick label, thinned until consecutive
 * labels are at least `labelWidth` apart so none can collide at any width.
 */
export function dayLabelIndexes(n: number, plotWidth: number, labelWidth: number): number[] {
  if (n <= 0) return []
  const every = Math.max(1, Math.ceil(labelWidth / Math.max(1, plotWidth / n)))
  const out = [0]
  for (let i = every; i < n; i += every) out.push(i)
  const tail = out[out.length - 1]
  if (tail !== n - 1) {
    if (n - 1 - tail >= every) out.push(n - 1)
    else if (out.length > 1) out[out.length - 1] = n - 1
  }
  return out
}

/**
 * The stats page's daily-activity chart: grouped bars of jobs created vs
 * completed per day. Hand-rolled SVG like Sparkline — no chart library — but
 * with the full anatomy: y gridlines, day tick labels, a legend, and a
 * per-day hover tooltip (the hit target is the whole day column, not the
 * bars). Series colors ride the theme tokens via CSS classes; all text wears
 * text tokens. Width tracks the container (ResizeObserver) so the SVG never
 * scales its type.
 */
export function ActivityChart({
  starts,
  created,
  completed,
  height = 180,
}: {
  starts: number[]
  created: number[]
  completed: number[]
  height?: number
}) {
  const wrapRef = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(600)
  const [hover, setHover] = useState<number | null>(null)

  useLayoutEffect(() => {
    const el = wrapRef.current
    if (!el) return
    const ro = new ResizeObserver(([e]) => setWidth(Math.max(200, e.contentRect.width)))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const n = starts.length
  const max = Math.max(1, ...created, ...completed)
  const pad = { top: 10, right: 8, bottom: 24, left: 10 + String(max).length * 8 }
  const plotW = width - pad.left - pad.right
  const plotH = height - pad.top - pad.bottom
  const step = max <= 4 ? 1 : Math.ceil(max / 4)
  const ticks: number[] = []
  for (let v = step; v <= max; v += step) ticks.push(v)

  const colW = plotW / n
  const barGap = 2
  const barW = Math.max(3, Math.min(14, (colW - 3 * barGap) / 2))
  const x0 = (i: number) => pad.left + i * colW + colW / 2 - barW - barGap / 2
  const y = (v: number) => pad.top + plotH - (v / max) * plotH
  const baseline = pad.top + plotH

  const fmtDay = (t: number) =>
    new Date(t).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
  const labeled = new Set(dayLabelIndexes(n, plotW, DAY_LABEL_WIDTH_PX))
  const labelX = (i: number) => {
    const inset = DAY_LABEL_WIDTH_PX / 2
    return Math.min(width - inset, Math.max(inset, pad.left + i * colW + colW / 2))
  }

  const bar = (bx: number, v: number) => {
    const ty = y(v)
    const h = baseline - ty
    const r = Math.min(3, barW / 2, h)
    if (h <= 0) return `M ${bx} ${baseline} h ${barW}`
    return [
      `M ${bx} ${baseline}`,
      `V ${ty + r}`,
      `Q ${bx} ${ty} ${bx + r} ${ty}`,
      `H ${bx + barW - r}`,
      `Q ${bx + barW} ${ty} ${bx + barW} ${ty + r}`,
      `V ${baseline}`,
      'Z',
    ].join(' ')
  }

  const hoverDay = hover !== null ? starts[hover] : null

  return (
    <div className="activity-chart" ref={wrapRef}>
      <div className="ac-legend">
        <span className="ac-key">
          <span className="ac-dot ac-created" /> Created
        </span>
        <span className="ac-key">
          <span className="ac-dot ac-completed" /> Completed
        </span>
      </div>
      <svg width={width} height={height} role="img" aria-label="jobs created and completed per day">
        {ticks.map((v) => (
          <g key={v}>
            <line className="ac-grid" x1={pad.left} x2={width - pad.right} y1={y(v)} y2={y(v)} />
            <text className="ac-label" x={pad.left - 5} y={y(v) + 4} textAnchor="end">
              {v}
            </text>
          </g>
        ))}
        <line className="ac-axis" x1={pad.left} x2={width - pad.right} y1={baseline} y2={baseline} />
        {starts.map((t, i) => (
          <g key={t}>
            {hover === i && (
              <rect
                className="ac-col-hi"
                x={pad.left + i * colW}
                y={pad.top}
                width={colW}
                height={plotH}
              />
            )}
            <path className="ac-bar ac-created" d={bar(x0(i), created[i])} />
            <path className="ac-bar ac-completed" d={bar(x0(i) + barW + barGap, completed[i])} />
            {labeled.has(i) && (
              <text
                className="ac-label"
                x={labelX(i)}
                y={height - 6}
                textAnchor="middle"
              >
                {fmtDay(t)}
              </text>
            )}
            <rect
              className="ac-hit"
              x={pad.left + i * colW}
              y={pad.top}
              width={colW}
              height={plotH}
              onMouseEnter={() => setHover(i)}
              onMouseLeave={() => setHover(null)}
            />
          </g>
        ))}
      </svg>
      {hover !== null && hoverDay !== null && (
        <div
          className="ac-tooltip"
          style={{
            left: `${Math.min(width - 150, Math.max(0, pad.left + hover * colW + colW / 2 - 70))}px`,
          }}
        >
          <div className="ac-tt-day">{fmtDay(hoverDay)}</div>
          <div>
            <span className="ac-dot ac-created" /> {created[hover]} created
          </div>
          <div>
            <span className="ac-dot ac-completed" /> {completed[hover]} completed
          </div>
        </div>
      )}
    </div>
  )
}
