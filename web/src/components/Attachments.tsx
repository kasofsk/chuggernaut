import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type DragEvent,
} from 'react'
import { ApiError, MAX_ATTACHMENT_BYTES, api, type Attachment } from '../api'

// ── Shared helpers ──────────────────────────────────────────────────────────

/** Attachments the UI previews inline as thumbnails; everything else renders as
 *  a name + size row with a generic file glyph. */
export function isImage(contentType: string): boolean {
  return contentType.startsWith('image/')
}

/** Humane byte size: '812 B', '46 KB', '3.2 MB'. */
export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`
  return `${(n / (1024 * 1024)).toFixed(n < 10 * 1024 * 1024 ? 1 : 0)} MB`
}

// The API rejects '/', '\', control chars, empty, '.' and '..' (path-traversal
// guard, routes.rs valid_attachment_name). Sanitize a picked/pasted/shot file's
// name to something it will accept, giving pastes (which arrive as "image.png"
// or blank) a unique-ish stem so several in a row don't clobber each other.
let nameCounter = 0
export function safeAttachmentName(raw: string | undefined, contentType: string): string {
  let name = (raw ?? '').replace(/[/\\]/g, '-').replace(/[\x00-\x1f]/g, '').trim()
  if (!name || name === '.' || name === '..') {
    const ext = contentType.split('/')[1]?.split('+')[0] || 'bin'
    // Date.now + a counter keeps a burst of pasted screenshots distinct.
    name = `pasted-${Date.now()}-${nameCounter++}.${ext}`
  }
  return name.slice(0, 255)
}

/** Pull image files off a paste/drop event's DataTransfer (screenshots land as
 *  image/* items with no real filename). */
function filesFromTransfer(dt: DataTransfer | null): File[] {
  if (!dt) return []
  const out: File[] = []
  if (dt.files && dt.files.length) out.push(...Array.from(dt.files))
  else if (dt.items) {
    for (const it of Array.from(dt.items)) {
      if (it.kind === 'file') {
        const f = it.getAsFile()
        if (f) out.push(f)
      }
    }
  }
  return out
}

/** Map an ApiError to a human line, calling out the size cap specially. */
function uploadErrorMessage(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.status === 413) return `too large — the ${fmtBytes(MAX_ATTACHMENT_BYTES)} limit`
    return e.message
  }
  return e instanceof Error ? e.message : 'upload failed'
}

// ── Image lightbox (reuses the cover pop-out overlay pattern) ────────────────

function ImageLightbox({ src, alt, onClose }: { src: string; alt: string; onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKey)
    const prev = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => {
      document.removeEventListener('keydown', onKey)
      document.body.style.overflow = prev
    }
  }, [onClose])
  return (
    <div className="cover-lightbox" onClick={onClose} role="dialog" aria-modal="true" aria-label={alt}>
      <div className="attach-lightbox-inner" onClick={(e) => e.stopPropagation()}>
        <button type="button" className="cover-close" aria-label="close" onClick={onClose}>
          ✕
        </button>
        <img className="attach-lightbox-img" src={src} alt={alt} />
      </div>
    </div>
  )
}

// ── Picker controls (add / camera), shared by both flows ─────────────────────

function AttachControls({
  onFiles,
  busy,
}: {
  onFiles: (files: File[]) => void
  busy?: boolean
}) {
  const addRef = useRef<HTMLInputElement>(null)
  const camRef = useRef<HTMLInputElement>(null)
  const pick = (input: HTMLInputElement | null) => input?.click()
  const onChange = (e: ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files ? Array.from(e.target.files) : []
    if (files.length) onFiles(files)
    e.target.value = '' // allow re-picking the same file
  }
  return (
    <div className="attach-controls">
      {/* No accept filter: docs and other files are allowed too (they render as
          name+size rows). Multiple so a whole camera-roll selection lands at once. */}
      <input ref={addRef} type="file" multiple hidden onChange={onChange} />
      {/* accept=image/* + capture: on a phone this opens the camera directly for
          a fresh screenshot/photo; on desktop it falls back to a file picker. */}
      <input
        ref={camRef}
        type="file"
        accept="image/*"
        capture="environment"
        hidden
        onChange={onChange}
      />
      <button type="button" onClick={() => pick(addRef.current)} disabled={busy}>
        ＋ Attach
      </button>
      <button
        type="button"
        className="attach-cam"
        onClick={() => pick(camRef.current)}
        disabled={busy}
        title="take a photo / screenshot with the camera"
      >
        📷 Camera
      </button>
    </div>
  )
}

// ── JobAttachments: the card on an existing job (JobDetail, Draft editor) ─────

type Upload = { id: number; name: string; file: File; progress: number; error: string | null }

/**
 * Attachments card for an existing job (spec §1.6). Lists current attachments —
 * images as thumbnails that pop into a lightbox, other files as name+size rows —
 * and, when `canEdit`, uploads via a file input (with camera capture on mobile),
 * drag-and-drop, and paste-from-clipboard, with per-file progress, a size-cap
 * error, and retry. Delete confirms first.
 */
export function JobAttachments({
  owner,
  project,
  seq,
  canEdit = true,
}: {
  owner: string
  project: string
  seq: number
  canEdit?: boolean
}) {
  const [list, setList] = useState<Attachment[]>([])
  const [loading, setLoading] = useState(true)
  const [listError, setListError] = useState<string | null>(null)
  const [uploads, setUploads] = useState<Upload[]>([])
  const [dragOver, setDragOver] = useState(false)
  const [lightbox, setLightbox] = useState<string | null>(null)
  const idRef = useRef(0)

  const refresh = useCallback(() => {
    api.attachments(owner, project, seq).then(
      (a) => {
        setList(a)
        setListError(null)
      },
      (e) => setListError(e instanceof Error ? e.message : 'failed to load attachments'),
    ).finally(() => setLoading(false))
  }, [owner, project, seq])

  useEffect(() => {
    setLoading(true)
    refresh()
  }, [refresh])

  // Run one upload entry to completion, driving its progress/error. On success
  // it drops from the in-flight list and the job's attachment list refetches.
  const runUpload = useCallback(
    (up: Upload) => {
      const setEntry = (patch: Partial<Upload>) =>
        setUploads((us) => us.map((u) => (u.id === up.id ? { ...u, ...patch } : u)))
      if (up.file.size > MAX_ATTACHMENT_BYTES) {
        setEntry({ error: `too large — the ${fmtBytes(MAX_ATTACHMENT_BYTES)} limit` })
        return
      }
      api
        .putAttachment(owner, project, seq, up.name, up.file, (f) => setEntry({ progress: f }))
        .then(
          () => {
            setUploads((us) => us.filter((u) => u.id !== up.id))
            refresh()
          },
          (e) => setEntry({ error: uploadErrorMessage(e) }),
        )
    },
    [owner, project, seq, refresh],
  )

  const startUploads = useCallback(
    (files: File[]) => {
      if (!canEdit || files.length === 0) return
      const entries: Upload[] = files.map((file) => ({
        id: idRef.current++,
        name: safeAttachmentName(file.name, file.type),
        file,
        progress: 0,
        error: null,
      }))
      setUploads((us) => [...us, ...entries])
      entries.forEach(runUpload)
    },
    [canEdit, runUpload],
  )

  const retry = (id: number) => {
    const up = uploads.find((u) => u.id === id)
    if (!up) return
    const reset = { ...up, progress: 0, error: null }
    setUploads((us) => us.map((u) => (u.id === id ? reset : u)))
    runUpload(reset)
  }

  const del = (name: string) => {
    if (!confirm(`Delete attachment “${name}”?`)) return
    api.deleteAttachment(owner, project, seq, name).then(refresh, (e) =>
      setListError(e instanceof Error ? e.message : 'delete failed'),
    )
  }

  // Paste-from-clipboard: a screenshot pasted anywhere on the job page attaches
  // here. Ignore pastes into a text field so typing isn't hijacked.
  useEffect(() => {
    if (!canEdit) return
    const onPaste = (e: ClipboardEvent) => {
      const el = e.target as HTMLElement | null
      const tag = el?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || el?.isContentEditable) return
      const files = filesFromTransfer(e.clipboardData)
      if (files.length) {
        e.preventDefault()
        startUploads(files)
      }
    }
    document.addEventListener('paste', onPaste)
    return () => document.removeEventListener('paste', onPaste)
  }, [canEdit, startUploads])

  const onDrop = (e: DragEvent) => {
    e.preventDefault()
    setDragOver(false)
    if (canEdit) startUploads(filesFromTransfer(e.dataTransfer))
  }
  const onDragOver = (e: DragEvent) => {
    if (!canEdit) return
    e.preventDefault()
    setDragOver(true)
  }

  const count = list.length + uploads.length
  const lightboxImg = lightbox && list.find((a) => a.name === lightbox && isImage(a.content_type))

  return (
    <section
      className={`card attachments${dragOver ? ' attach-dragover' : ''}`}
      onDrop={onDrop}
      onDragOver={onDragOver}
      onDragLeave={() => setDragOver(false)}
    >
      <div className="row-head">
        <h2>
          Attachments {count > 0 && <span className="dim">{count}</span>}
        </h2>
        {canEdit && <AttachControls onFiles={startUploads} />}
      </div>

      {listError && <div className="error banner">{listError}</div>}

      {count === 0 ? (
        <p className="dim attach-empty">
          {loading
            ? 'loading…'
            : canEdit
              ? 'No attachments yet — attach, drag a file here, or paste a screenshot.'
              : 'No attachments.'}
        </p>
      ) : (
        <div className="attach-grid">
          {list.map((a) => {
            const img = isImage(a.content_type)
            const url = api.attachmentUrl(owner, project, seq, a.name)
            return (
              <div className="attach-tile" key={a.name}>
                {img ? (
                  <button
                    type="button"
                    className="attach-thumb"
                    title={a.name}
                    onClick={() => setLightbox(a.name)}
                  >
                    <img src={url} alt={a.name} loading="lazy" />
                  </button>
                ) : (
                  <a className="attach-file" href={url} target="_blank" rel="noreferrer" title={a.name}>
                    <span className="attach-glyph" aria-hidden="true">
                      📄
                    </span>
                  </a>
                )}
                <div className="attach-meta">
                  <span className="attach-name" title={a.name}>
                    {a.name}
                  </span>
                  <span className="dim attach-size">{fmtBytes(a.size)}</span>
                </div>
                {canEdit && (
                  <button
                    type="button"
                    className="attach-del"
                    aria-label={`delete ${a.name}`}
                    title="delete"
                    onClick={() => del(a.name)}
                  >
                    ✕
                  </button>
                )}
              </div>
            )
          })}

          {uploads.map((u) => (
            <div className="attach-tile attach-uploading" key={`up-${u.id}`}>
              <div className="attach-thumb attach-thumb-pending">
                {u.error ? <span aria-hidden="true">⚠</span> : `${Math.round(u.progress * 100)}%`}
              </div>
              <div className="attach-meta">
                <span className="attach-name" title={u.name}>
                  {u.name}
                </span>
                {u.error ? (
                  <span className="attach-err" title={u.error}>
                    {u.error}
                  </span>
                ) : (
                  <span className="dim attach-size">uploading…</span>
                )}
              </div>
              {u.error ? (
                <button type="button" className="attach-del attach-retry" title="retry" onClick={() => retry(u.id)}>
                  ↻
                </button>
              ) : (
                <div className="attach-progress" aria-hidden="true">
                  <div className="attach-progress-bar" style={{ width: `${Math.round(u.progress * 100)}%` }} />
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {lightboxImg && (
        <ImageLightbox
          src={api.attachmentUrl(owner, project, seq, lightboxImg.name)}
          alt={lightboxImg.name}
          onClose={() => setLightbox(null)}
        />
      )}
    </section>
  )
}

// ── AttachmentComposer: pick/paste/shoot files while composing a new job ──────
//
// Used before the job exists (New Job form, share-to-job screen). It owns
// nothing durable — the parent holds the File[] and PUTs them once the job is
// created (see uploadFiles). Images preview from object URLs, revoked on change.

export function AttachmentComposer({
  files,
  onChange,
  label = 'Attachments',
  hint = 'optional — added to the job on create; paste or drag a screenshot in',
}: {
  files: File[]
  onChange: (files: File[]) => void
  label?: string
  hint?: string
}) {
  const [dragOver, setDragOver] = useState(false)
  const [lightbox, setLightbox] = useState<number | null>(null)

  // One object URL per file, rebuilt when the set changes and revoked on cleanup.
  const urls = useMemo(() => files.map((f) => (isImage(f.type) ? URL.createObjectURL(f) : null)), [files])
  useEffect(() => () => urls.forEach((u) => u && URL.revokeObjectURL(u)), [urls])

  const add = (incoming: File[]) => {
    if (incoming.length) onChange([...files, ...incoming])
  }
  const removeAt = (i: number) => onChange(files.filter((_, j) => j !== i))

  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const el = e.target as HTMLElement | null
      const tag = el?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || el?.isContentEditable) return
      const f = filesFromTransfer(e.clipboardData)
      if (f.length) {
        e.preventDefault()
        add(f)
      }
    }
    document.addEventListener('paste', onPaste)
    return () => document.removeEventListener('paste', onPaste)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [files])

  const over = files[lightbox ?? -1]
  const overUrl = lightbox != null ? urls[lightbox] : null

  return (
    <div
      className={`field attach-composer${dragOver ? ' attach-dragover' : ''}`}
      onDrop={(e) => {
        e.preventDefault()
        setDragOver(false)
        add(filesFromTransfer(e.dataTransfer))
      }}
      onDragOver={(e) => {
        e.preventDefault()
        setDragOver(true)
      }}
      onDragLeave={() => setDragOver(false)}
    >
      <span>
        {label} <span className="dim">({hint})</span>
      </span>
      <AttachControls onFiles={add} />
      {files.length > 0 && (
        <div className="attach-grid">
          {files.map((f, i) => {
            const url = urls[i]
            return (
              <div className="attach-tile" key={`${f.name}-${i}`}>
                {url ? (
                  <button type="button" className="attach-thumb" title={f.name} onClick={() => setLightbox(i)}>
                    <img src={url} alt={f.name} />
                  </button>
                ) : (
                  <div className="attach-file" title={f.name}>
                    <span className="attach-glyph" aria-hidden="true">
                      📄
                    </span>
                  </div>
                )}
                <div className="attach-meta">
                  <span className="attach-name" title={f.name}>
                    {f.name || 'screenshot'}
                  </span>
                  <span className="dim attach-size">{fmtBytes(f.size)}</span>
                </div>
                <button
                  type="button"
                  className="attach-del"
                  aria-label={`remove ${f.name}`}
                  title="remove"
                  onClick={() => removeAt(i)}
                >
                  ✕
                </button>
              </div>
            )
          })}
        </div>
      )}
      {over && overUrl && (
        <ImageLightbox src={overUrl} alt={over.name} onClose={() => setLightbox(null)} />
      )}
    </div>
  )
}

/**
 * PUT a composed File[] onto a just-created job's attachments. Best-effort per
 * file; returns the names that failed so the caller can surface a warning
 * without blocking navigation to the job (where they can be retried).
 */
export async function uploadFiles(
  owner: string,
  project: string,
  seq: number,
  files: File[],
): Promise<string[]> {
  const failed: string[] = []
  await Promise.all(
    files.map(async (f) => {
      const name = safeAttachmentName(f.name, f.type)
      try {
        await api.putAttachment(owner, project, seq, name, f)
      } catch {
        failed.push(name)
      }
    }),
  )
  return failed
}
