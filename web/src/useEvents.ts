import { useEffect, useRef } from 'react'

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
export function useProjectEvents(
  owner: string,
  project: string,
  onEvent: (e: JobEvent) => void,
  jobSeq?: number,
) {
  const handler = useRef(onEvent)
  handler.current = onEvent
  useEffect(() => {
    const base = `/api/v1/projects/${owner}/${project}`
    const url = jobSeq !== undefined ? `${base}/jobs/${jobSeq}/events` : `${base}/events`
    const source = new EventSource(url)
    source.onmessage = (msg) => {
      try {
        handler.current(JSON.parse(msg.data))
      } catch {
        // ignore unparseable frames
      }
    }
    return () => source.close()
  }, [owner, project, jobSeq])
}
