import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { ApiError, api, type Task } from '../api'
import { Transcript } from './Transcript'

// Poll cadence while a container runs, and the ceiling a network-error backoff
// climbs to. The buffer is capped so a multi-hundred-megabyte cargo build log
// can't balloon the tab — we keep the tail and flag the truncation.
const POLL_MS = 2000
const MAX_BACKOFF_MS = 15000
const BUFFER_CAP = 2_000_000 // ~2MB of tail

// cargo/agent output carries ANSI colour + cursor escapes. Strip the common
// CSI/OSC sequences (rendering colour is a non-goal) rather than pull in a
// terminal library. Lone carriage returns (progress-bar redraws) collapse to
// newlines so a redrawn line doesn't run on forever.
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b[@-Z\\-_]/g
function stripAnsi(s: string): string {
  return s.replace(ANSI, '').replace(/\r\n/g, '\n').replace(/\r/g, '\n')
}

type LogStatus =
  | 'loading'
  | 'live'
  | 'ended'
  | 'retrying'
  | 'unreachable'
  | 'nocontainer'

const STATUS_TEXT: Record<LogStatus, string> = {
  loading: 'connecting…',
  live: '● live',
  ended: 'ended',
  retrying: 'reconnecting…',
  unreachable: 'node unreachable — retrying',
  nocontainer: 'no container',
}

/**
 * A task's stdout, tailed live and then seamlessly continued from the harvested
 * artifact at the same byte offsets (see {@link api.taskOutput}). Renders as a
 * self-contained pane (mount it in an expandable table row): monospace, dark
 * via CSS variables, scrolling inside its own bounded box so it never widens the
 * page. Auto-scrolls to the bottom while pinned; scrolling up pauses that and a
 * jump-to-bottom control resumes it.
 *
 * Polling runs only while mounted (the pane is open) and the tab is visible, and
 * tears down on unmount — it never touches the page's SSE refresh. A 404 (no
 * container yet — a parked human task or Pending launch) shows a muted note, not
 * an error; network/502 errors show a subtle retrying state and back off.
 */
export function TaskLogPane({
  owner,
  project,
  seq,
  task,
  onClose,
}: {
  owner: string
  project: string
  seq: number
  task: Task
  onClose: () => void
}) {
  const [text, setText] = useState('')
  const [status, setStatus] = useState<LogStatus>('loading')
  const [truncated, setTruncated] = useState(false)
  const [pinned, setPinned] = useState(true)
  // Default to the formatted claude transcript; `raw` is the debugging escape
  // hatch that shows the untouched byte stream.
  const [view, setView] = useState<'transcript' | 'raw'>('transcript')

  const bodyRef = useRef<HTMLDivElement>(null)
  const pinnedRef = useRef(true)
  useEffect(() => {
    pinnedRef.current = pinned
  }, [pinned])

  // The tail loop. Keyed on the task identity only: a Running→Done transition
  // arrives through the endpoint's `running` flag, not a prop change, so the
  // loop doesn't need to restart when the parent re-renders the task.
  useEffect(() => {
    const cancelled = { current: false }
    let timer: number | null = null
    let offset = 0
    let buffer = ''
    let wasRunning = false
    let errorStreak = 0

    const append = (chunk: string) => {
      buffer += stripAnsi(chunk)
      if (buffer.length > BUFFER_CAP) {
        buffer = buffer.slice(buffer.length - BUFFER_CAP)
        const nl = buffer.indexOf('\n') // trim the partial leading line
        if (nl >= 0) buffer = buffer.slice(nl + 1)
        setTruncated(true)
      }
      setText(buffer)
    }

    const clear = () => {
      if (timer != null) {
        clearTimeout(timer)
        timer = null
      }
    }
    const schedule = (ms: number, final = false) => {
      clear()
      timer = window.setTimeout(() => void tick(final), ms)
    }

    const tick = async (final: boolean) => {
      if (cancelled.current) return
      // Pause while the tab is backgrounded — re-check shortly rather than poll.
      if (document.hidden) return schedule(POLL_MS, final)
      try {
        const r = await api.taskOutput(owner, project, seq, task.id, offset)
        if (cancelled.current) return
        errorStreak = 0
        offset = r.offset
        if (r.data) append(r.data)
        if (r.running) {
          wasRunning = true
          setStatus('live')
          schedule(POLL_MS)
        } else if (wasRunning && !final) {
          // Observed the container exit — one final sweep of the harvested tail,
          // then stop. The endpoint keeps the same offsets across the switch.
          setStatus('live')
          schedule(POLL_MS, true)
        } else {
          setStatus('ended')
        }
      } catch (e) {
        if (cancelled.current) return
        if (e instanceof ApiError && e.status === 404) {
          // No container yet (parked human / Pending launch). Muted, and keep a
          // slow watch only while one might still spawn.
          setStatus('nocontainer')
          if (task.state === 'Pending' || task.state === 'Running') schedule(POLL_MS, final)
          return
        }
        errorStreak += 1
        setStatus(e instanceof ApiError && e.status === 502 ? 'unreachable' : 'retrying')
        schedule(Math.min(POLL_MS * 2 ** errorStreak, MAX_BACKOFF_MS), final)
      }
    }

    void tick(false)
    const onVisible = () => {
      // Resume promptly when the tab comes back and the loop is idle-waiting.
      if (!document.hidden && timer == null && !cancelled.current) void tick(false)
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      cancelled.current = true
      clear()
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [owner, project, seq, task.id, task.state])

  // Stick to the bottom on append while pinned (and when the view mode flips,
  // since the two renderings have different heights).
  useLayoutEffect(() => {
    if (pinnedRef.current && bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight
    }
  }, [text, view])

  const onScroll = () => {
    const el = bodyRef.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24
    if (atBottom !== pinnedRef.current) setPinned(atBottom)
  }
  const jump = () => {
    const el = bodyRef.current
    if (el) el.scrollTop = el.scrollHeight
    setPinned(true)
  }

  const warn = status === 'retrying' || status === 'unreachable'
  const placeholder =
    status === 'nocontainer'
      ? 'no container output yet — nothing has run for this task'
      : status === 'loading'
        ? 'connecting…'
        : '(no output)'

  return (
    <div className="log-pane">
      <div className="log-head">
        <strong>
          task {task.id} · logs
        </strong>
        <span className={`log-status${status === 'live' ? ' live' : warn ? ' warn' : ' dim'}`}>
          {STATUS_TEXT[status]}
        </span>
        {status === 'ended' && (
          <a href={api.artifactUrl(owner, project, seq, task.id, 'stdout.log')} download>
            download
          </a>
        )}
        <button
          className="linklike"
          onClick={() => setView((v) => (v === 'transcript' ? 'raw' : 'transcript'))}
          title={view === 'transcript' ? 'show the raw log stream' : 'show the formatted transcript'}
        >
          {view === 'transcript' ? 'raw' : 'transcript'}
        </button>
        <button className="linklike" onClick={onClose}>
          close
        </button>
      </div>
      {truncated && <div className="log-truncated dim">… earlier output truncated</div>}
      <div
        className={`log-body${view === 'transcript' ? ' log-transcript' : ''}`}
        ref={bodyRef}
        onScroll={onScroll}
      >
        {text ? (
          view === 'transcript' ? (
            <Transcript text={text} />
          ) : (
            text
          )
        ) : (
          <span className="dim">{placeholder}</span>
        )}
      </div>
      {!pinned && (
        <button className="log-jump" onClick={jump}>
          jump to bottom ↓
        </button>
      )}
    </div>
  )
}
