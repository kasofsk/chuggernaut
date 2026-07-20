import { useCallback, useEffect, useState } from 'react'
import { ApiError, api, type OriginStatus } from '../api'

/**
 * Linked-origin panel: origin URL, unreleased backlog, release PR state, and
 * the release/sync actions. Renders nothing on classic (unlinked) projects —
 * the origin endpoint 404s and the panel stays hidden.
 */
export function OriginPanel({ owner, project }: { owner: string; project: string }) {
  const [status, setStatus] = useState<OriginStatus | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const refresh = useCallback(() => {
    api
      .origin(owner, project)
      .then((s) => {
        setStatus(s)
        setError(null)
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 404) setStatus(null)
        else setError(e instanceof Error ? e.message : 'origin load failed')
      })
  }, [owner, project])

  useEffect(refresh, [refresh])

  if (!status?.origin) return null
  const { origin, release } = status
  const releaseOpen = release?.status === 'open'

  async function act(fn: () => Promise<unknown>) {
    setBusy(true)
    try {
      await fn()
      refresh()
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'action failed')
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="card origin">
      <div className="row-head">
        <h2>
          Origin
          {status.held && (
            <span className="badge held" title="merge queue held while the release PR is open">
              held for release {release?.number}
            </span>
          )}
        </h2>
        <div className="actions">
          <button disabled={busy} onClick={() => act(() => api.originSync(owner, project))}>
            sync
          </button>
          {!releaseOpen && (
            <button
              disabled={busy || status.ahead_by === 0}
              title={
                status.ahead_by === 0
                  ? 'integration has nothing beyond origin main'
                  : 'push integration as a release branch and open a PR'
              }
              onClick={() => act(() => api.originRelease(owner, project))}
            >
              open release PR
            </button>
          )}
        </div>
      </div>
      {error && <div className="error banner">{error}</div>}
      <dl className="origin-facts">
        <dt>remote</dt>
        <dd>
          <code>{origin.url}</code>
        </dd>
        <dt>origin {origin.main_branch}</dt>
        <dd>
          <code>{status.origin_main_sha?.slice(0, 10) ?? '—'}</code>
        </dd>
        <dt>integration</dt>
        <dd>
          <code>{status.integration_sha?.slice(0, 10) ?? '—'}</code>
          <span className="dim">
            {' '}
            · {status.ahead_by} unreleased commit{status.ahead_by === 1 ? '' : 's'}
          </span>
        </dd>
        {release && (
          <>
            <dt>release {release.number}</dt>
            <dd>
              {release.pr_url ? (
                <a href={release.pr_url} target="_blank" rel="noreferrer">
                  PR #{release.pr_number}
                </a>
              ) : (
                <span className="dim">no PR (non-GitHub origin)</span>
              )}{' '}
              · {release.status}
            </dd>
          </>
        )}
      </dl>
    </section>
  )
}
