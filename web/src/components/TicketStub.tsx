/**
 * The train-ticket / boarding-pass live preview (#171): renders a job as it is
 * composed — title as the headline, type as a punch-corner chip, deps as
 * 'connections', extra evaluators as 'inspection stamps', tags as 'stickers',
 * and validation problems as amber stamps. Shared with the Draft live editor
 * (#129), where remote PATCHes fill the same fields — so the chat-as-wizard demo
 * shows a ticket materialising rather than a form.
 */
export interface TicketData {
  owner: string
  project: string
  title: string
  /** the selected type's display name; absent renders a MISSING TYPE stamp */
  typeLabel?: string
  deps: number[]
  /** extra evaluator names */
  evals: string[]
  tags: string[]
  /** the destination label ('draft' vs 'release') for the route line */
  destination?: string
}

export function TicketStub({
  data,
  anim = 'none',
}: {
  data: TicketData
  /** create-time flourish: hole-punch + depart (release) or slide to the siding (draft) */
  anim?: 'none' | 'punched' | 'depart' | 'siding'
}) {
  const errors: string[] = []
  if (!data.typeLabel) errors.push('Missing type')

  const cls =
    'ticket' +
    (anim === 'punched' || anim === 'depart' ? ' ticket-punched' : '') +
    (anim === 'depart' ? ' ticket-depart' : '') +
    (anim === 'siding' ? ' ticket-siding' : '')

  return (
    <div className={cls}>
      <div className="ticket-eyebrow">
        <span>Boarding pass</span>
        <span>🚂 Chuggernaut</span>
      </div>
      <div className={`ticket-headline${data.title ? '' : ' dim'}`}>{data.title || 'Untitled run'}</div>
      <div>
        {data.typeLabel ? (
          <span className="ticket-type-chip">◗ {data.typeLabel}</span>
        ) : (
          <span className="ticket-error">Missing type</span>
        )}
      </div>
      <div className="ticket-route">
        <b>
          {data.owner}/{data.project}
        </b>
        <span>→</span>
        <b>{data.destination ?? 'departure'}</b>
      </div>

      <hr className="ticket-perf" />

      {data.deps.length > 0 && (
        <>
          <div className="ticket-section-label">Connections</div>
          <div className="ticket-conns">
            {data.deps.map((d) => (
              <span key={d} className="ticket-conn">
                #{d}
              </span>
            ))}
          </div>
        </>
      )}
      {data.evals.length > 0 && (
        <>
          <div className="ticket-section-label">Inspection stamps</div>
          <div className="ticket-stamps">
            {data.evals.map((e, i) => (
              <span key={`${e}-${i}`} className="ticket-stamp">
                {e}
              </span>
            ))}
          </div>
        </>
      )}
      {data.tags.length > 0 && (
        <>
          <div className="ticket-section-label">Tags</div>
          <div className="ticket-stickers">
            {data.tags.map((t) => (
              <span key={t} className="ticket-sticker">
                {t}
              </span>
            ))}
          </div>
        </>
      )}
      {errors.filter((e) => e !== 'Missing type').map((e) => (
        <span key={e} className="ticket-error">
          {e}
        </span>
      ))}
      <div className="ticket-barcode" />
    </div>
  )
}
