/**
 * A hand-rolled SVG sparkline (#162/#163): a filled polyline over a series of
 * counts. No chart library — a viewBox-normalised polyline the CSS colours via
 * currentColor. Flat/empty series render a baseline rather than a spike.
 */
export function Sparkline({
  data,
  width = 132,
  height = 34,
  className,
}: {
  data: number[]
  width?: number
  height?: number
  className?: string
}) {
  const n = data.length
  const max = Math.max(1, ...data)
  const pad = 2
  const x = (i: number) => (n <= 1 ? pad : pad + (i * (width - 2 * pad)) / (n - 1))
  const y = (v: number) => height - pad - (v / max) * (height - 2 * pad)
  const pts = data.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`)
  const line = pts.join(' ')
  const area = `${x(0).toFixed(1)},${height} ${line} ${x(n - 1).toFixed(1)},${height}`
  return (
    <svg
      className={`sparkline${className ? ` ${className}` : ''}`}
      viewBox={`0 0 ${width} ${height}`}
      width={width}
      height={height}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      <polygon className="sparkline-area" points={area} />
      <polyline className="sparkline-line" points={line} />
    </svg>
  )
}
