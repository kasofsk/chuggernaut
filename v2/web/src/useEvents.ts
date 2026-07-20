import { useEffect, useRef } from 'react'
import { markSse } from './connection'

export interface JobEvent {
  job_seq: number
  project: string
  ts: string
  event_type: string
  [key: string]: unknown
}

// SSE subscription (§6.4). EventSource reconnects itself and replays from
// Last-Event-ID, so the handler just consumes. `onEvent` sees every event;
// use it to refetch or merge.
//
// Mobile addendum (Part 11): backgrounded tabs get their sockets suspended;
// on some mobile browsers the EventSource comes back CLOSED, where its own
// retry never fires. Recreate it when the app returns to the foreground.
// Stream health is reported to the connection store for the banner.
export function useProjectEvents(
  owner: string,
  project: string,
  onEvent: (e: JobEvent) => void,
  jobSeq?: number,
) {
  const handler = useRef(onEvent)
  handler.current = onEvent
  useEffect(() => {
    const id = Symbol('sse')
    const base = `/api/v1/projects/${owner}/${project}`
    const url = jobSeq !== undefined ? `${base}/jobs/${jobSeq}/events` : `${base}/events`
    let source: EventSource | null = null

    const connect = () => {
      source?.close()
      source = new EventSource(url)
      source.onopen = () => markSse(id, false)
      source.onerror = () => markSse(id, true)
      source.onmessage = (msg) => {
        try {
          handler.current(JSON.parse(msg.data))
        } catch {
          // ignore unparseable frames
        }
      }
    }
    connect()

    const onVisible = () => {
      if (document.visibilityState === 'visible' && source?.readyState === EventSource.CLOSED) {
        connect()
      }
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      document.removeEventListener('visibilitychange', onVisible)
      markSse(id, false)
      source?.close()
    }
  }, [owner, project, jobSeq])
}
