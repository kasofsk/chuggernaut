import type { DeployReport, Task } from '../api'

// A deploy job's structured leg report (ticket #187) as a checklist card: the
// from→to SHA header with a rollback/health badge, then one row per leg — green
// ✓ ok, red ✕ failed (with the short reason), grey · skipped. A deploy is a
// checklist, not a conversation, so it reads as one. Shared by JobDetail (in a
// deploy job's command task) and the per-project Deploys page — one renderer, no
// duplication.
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

// A deploy job's structured payload carries a `legs` array (the envelope fields
// are optional). Detect that shape on a task result's `structured` — tolerant of
// either an object or a JSON string — so a deploy renders as a checklist rather
// than a raw JSON block. Returns null for anything else.
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

// The deploy leg report for a whole job, harvested from its tasks: the command
// work task carries the structured DeployReport. Scans newest-first so a
// re-deploy's latest attempt wins.
export function deployReportOfTasks(tasks: Task[]): DeployReport | null {
  for (let i = tasks.length - 1; i >= 0; i--) {
    const r = tasks[i].result
    if (r && r.kind === 'Command') {
      const report = deployReportOf(r.structured)
      if (report) return report
    }
  }
  return null
}
