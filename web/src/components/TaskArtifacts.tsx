import { useEffect, useRef, useState } from 'react'
import { api, type ArtifactKind, type Task } from '../api'
import { SkeletonLines } from './Skeleton'

const LABELS: Record<ArtifactKind, string> = {
  'session.jsonl': 'transcript',
  'stdout.log': 'logs',
  'output.tar.gz': 'output',
}

/** Kinds that are opaque binary: offered as a download, never as text. */
const BINARY: ArtifactKind[] = ['output.tar.gz']

/**
 * Links to whatever a task left behind. Availability is per-task, so it is
 * queried rather than inferred: human tasks have nothing, command tasks have
 * logs only, and an agent that died before starting has no transcript.
 */
export function TaskArtifacts({
  owner,
  project,
  seq,
  task,
  open,
  onOpen,
}: {
  owner: string
  project: string
  seq: number
  task: Task
  open: ArtifactKind | null
  onOpen: (kind: ArtifactKind | null) => void
}) {
  const [kinds, setKinds] = useState<ArtifactKind[]>([])

  const terminal = task.state === 'Done' || task.state === 'Failed'
  useEffect(() => {
    if (!terminal) return
    api
      .artifacts(owner, project, seq, task.id)
      .then((r) => setKinds(r.artifacts))
      .catch(() => setKinds([]))
  }, [owner, project, seq, task.id, terminal])

  if (kinds.length === 0) return <span className="dim">—</span>

  return (
    <span className="artifact-links">
      {kinds.map((k) =>
        BINARY.includes(k) ? (
          <a
            key={k}
            className="linklike"
            href={api.artifactUrl(owner, project, seq, task.id, k)}
            download
          >
            {LABELS[k]}
          </a>
        ) : (
          <button key={k} className="linklike" onClick={() => onOpen(open === k ? null : k)}>
            {LABELS[k]}
          </button>
        ),
      )}
    </span>
  )
}

/**
 * One artifact rendered as text — fetched as bytes, not JSON, since a transcript
 * is JSONL (one object per line, so not a valid document) and both kinds can be
 * large. Mount it in a width-bounded container — never a table cell, which sizes
 * to its widest line (docs/implementation-notes.md).
 */
export function ArtifactViewer({
  owner,
  project,
  seq,
  task,
  kind,
  onClose,
}: {
  owner: string
  project: string
  seq: number
  task: Task
  kind: ArtifactKind
  onClose: () => void
}) {
  const [text, setText] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const paneRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    paneRef.current?.scrollIntoView({ block: 'nearest' })
  }, [])

  useEffect(() => {
    let cancelled = false
    setText(null)
    setError(null)
    api
      .artifactText(owner, project, seq, task.id, kind)
      .then((t) => {
        if (!cancelled) setText(t)
      })
      .catch((e) => {
        if (!cancelled) setError(String(e))
      })
    return () => {
      cancelled = true
    }
  }, [owner, project, seq, task.id, kind])

  return (
    <div className="artifact-viewer" ref={paneRef}>
      <div className="artifact-head">
        <strong>
          task {task.id} · {LABELS[kind]}
        </strong>
        <a href={api.artifactUrl(owner, project, seq, task.id, kind)} download>
          download
        </a>
        <button className="linklike" onClick={onClose}>
          close
        </button>
      </div>
      {error && <p className="error">{error}</p>}
      {!text && !error && <SkeletonLines n={4} />}
      {text && <pre className="artifact-body">{text}</pre>}
    </div>
  )
}
