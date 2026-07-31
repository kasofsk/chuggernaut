import { useCallback, useEffect, useRef } from 'react'

export interface JobEvent {
  job_seq: number
  project: string
  ts: string
  event_type: string
  [key: string]: unknown
}

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
      }
    }
    return () => source.close()
  }, [owner, project, jobSeq])
}

export function useDebouncedCallback(fn: () => void, delayMs: number) {
  const cb = useRef(fn)
  cb.current = fn
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])
  return useCallback(() => {
    clearTimeout(timer.current)
    timer.current = setTimeout(() => cb.current(), delayMs)
  }, [delayMs])
}
