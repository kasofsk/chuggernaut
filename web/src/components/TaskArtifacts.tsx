import { useCallback, useEffect, useState } from 'react'
import { api, type ArtifactKind, type Task } from '../api'
import { SkeletonLines } from './Skeleton'

const LABELS: Record<ArtifactKind, string> = {
  'session.jsonl': 'transcript',
  'stdout.log': 'logs',
}

/**
 * Links to whatever a task left behind. Transcripts and container logs are
 * fetched as bytes, not JSON: a transcript is JSONL (one object per line, so
 * not a valid document) and both can be large.
 *
 * Availability is per-task, so it is queried rather than inferred: human tasks
 * have nothing, command tasks have logs only, and an agent that died before
 * starting has no transcript.
 */
export function TaskArtifacts({
  owner,
  project,
  seq,
  task,
}: {
  owner: string
  project: string
  seq: number
  task: Task
}) {
  const [kinds, setKinds] = useState<ArtifactKind[]>([])
  const [open, setOpen] = useState<ArtifactKind | null>(null)
  const [text, setText] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const terminal = task.state === 'Done' || task.state === 'Failed'
  useEffect(() => {
    // Artifacts are harvested after the container exits; nothing to list until
    // the task settles.
    if (!terminal) return
    api
      .artifacts(owner, project, seq, task.id)
      .then((r) => setKinds(r.artifacts))
      .catch(() => setKinds([]))
  }, [owner, project, seq, task.id, terminal])

  const view = useCallback(
    (kind: ArtifactKind) => {
      setOpen(kind)
      setText(null)
      setError(null)
      api
        .artifactText(owner, project, seq, task.id, kind)
        .then(setText)
        .catch((e) => setError(String(e)))
    },
    [owner, project, seq, task.id],
  )

  if (kinds.length === 0) return <span className="dim">—</span>

  return (
    <>
      <span className="artifact-links">
        {kinds.map((k) => (
          <button key={k} className="linklike" onClick={() => view(k)}>
            {LABELS[k]}
          </button>
        ))}
      </span>
      {open && (
        <div className="artifact-viewer">
          <div className="artifact-head">
            <strong>
              task {task.id} · {LABELS[open]}
            </strong>
            <a href={api.artifactUrl(owner, project, seq, task.id, open)} download>
              download
            </a>
            <button className="linklike" onClick={() => setOpen(null)}>
              close
            </button>
          </div>
          {error && <p className="error">{error}</p>}
          {!text && !error && <SkeletonLines n={4} />}
          {text && <pre className="artifact-body">{text}</pre>}
        </div>
      )}
    </>
  )
}
