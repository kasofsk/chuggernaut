import type { DeployReport, Task } from '../api'

export function DeployLegCard({ report }: { report: DeployReport }) {
  const legs = report.legs ?? []
  const glyph = (s: string) => (s === 'ok' ? '✓' : s === 'failed' ? '✕' : '·')
  return (
    <div className="deploy-card">
      <div className="deploy-shas">
        {report.from_sha && <code>{report.from_sha.slice(0, 12)}</code>}
        <span aria-hidden="true" className="dim">
          →
        </span>
        {report.to_sha ? <code>{report.to_sha.slice(0, 12)}</code> : <span className="dim">?</span>}
        {report.rollback && (
          <span
            className="badge badge-red"
            title="restart-verify's health check failed — prod was restored to the previous binary"
          >
            rolled back
          </span>
        )}
        {report.health && (
          <span className={`badge ${report.health === 'ok' ? 'badge-green' : 'badge-orange'}`}>
            health: {report.health}
          </span>
        )}
      </div>
      {legs.length > 0 && (
        <ul className="deploy-legs">
          {legs.map((leg, i) => (
            <li key={i} className={`deploy-leg deploy-leg-${leg.status}`}>
              <span className="deploy-leg-icon" aria-hidden="true">
                {glyph(leg.status)}
              </span>
              <span className="deploy-leg-name">{leg.name}</span>
              {leg.error && <span className="deploy-leg-err">{leg.error}</span>}
              {typeof leg.secs === 'number' && (
                <span className="deploy-leg-secs dim">{leg.secs}s</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

export function deployReportOf(structured: unknown): DeployReport | null {
  let value: Record<string, unknown> | null = null
  if (structured == null) return null
  if (typeof structured === 'string') {
    try {
      const parsed: unknown = JSON.parse(structured)
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        value = parsed as Record<string, unknown>
      }
    } catch {
      return null
    }
  } else if (typeof structured === 'object' && !Array.isArray(structured)) {
    value = structured as Record<string, unknown>
  }
  if (!value) return null
  return Array.isArray(value.legs) ? (value as unknown as DeployReport) : null
}

export function deployReportOfTasks(tasks: Task[]): DeployReport | null {
  for (let i = tasks.length - 1; i >= 0; i--) {
    const r = tasks[i].result
    if (!r || !('structured' in r)) continue
    const report = deployReportOf(r.structured)
    if (report) return report
  }
  return null
}
