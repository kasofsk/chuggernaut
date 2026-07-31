import { useMemo, useState } from 'react'
import type { DiffResponse } from '../api'

type FileStat = DiffResponse['files'][number]

/** The new-side path a `git diff --numstat` entry names, expanding git's
 *  `{old => new}` rename notation to the path the diff section carries. */
export function statPathOf(path: string): string {
  const brace = path.indexOf('{')
  const close = path.indexOf('}', brace + 1)
  if (brace >= 0 && close > brace) {
    const inner = path.slice(brace + 1, close)
    const arrow = inner.indexOf(' => ')
    if (arrow >= 0) return path.slice(0, brace) + inner.slice(arrow + 4) + path.slice(close + 1)
  }
  const arrow = path.indexOf(' => ')
  return arrow >= 0 ? path.slice(arrow + 4) : path
}

function diffPathStrip(raw: string): string {
  const path = raw.startsWith('"') && raw.endsWith('"') ? raw.slice(1, -1) : raw
  if (path === '/dev/null') return ''
  return path.startsWith('a/') || path.startsWith('b/') ? path.slice(2) : path
}

function sectionPathOf(section: string): string {
  let from = ''
  let to = ''
  for (const line of section.split('\n')) {
    if (line.startsWith('@@')) break
    if (!from && line.startsWith('--- ')) from = diffPathStrip(line.slice(4))
    if (!to && line.startsWith('+++ ')) to = diffPathStrip(line.slice(4))
  }
  if (to || from) return to || from
  const header = section.split('\n')[0]
  const rest = header.startsWith('diff --git ') ? header.slice(11).trimEnd() : ''
  const half = rest.lastIndexOf(' b/')
  return half >= 0 ? diffPathStrip(rest.slice(half + 1)) : rest
}

/** Splits a unified diff into its per-file sections, keyed by the new-side
 *  path (the old one for a deletion). */
export function diffSectionsByPath(diff: string): Map<string, string> {
  const sections = new Map<string, string>()
  if (!diff) return sections
  const lines = diff.split('\n')
  let start = -1
  for (let i = 0; i <= lines.length; i++) {
    if (i < lines.length && !lines[i].startsWith('diff --git ')) continue
    if (start >= 0) {
      const text = lines.slice(start, i).join('\n')
      sections.set(sectionPathOf(text), text)
    }
    start = i
  }
  return sections
}

/** Plain unified-diff render with line coloring. */
export function DiffView({ diff }: { diff: string }) {
  return (
    <pre className="diff">
      {diff.split('\n').map((line, i) => (
        <div
          key={i}
          className={
            line.startsWith('+') && !line.startsWith('+++')
              ? 'diff-add'
              : line.startsWith('-') && !line.startsWith('---')
                ? 'diff-del'
                : line.startsWith('@@')
                  ? 'diff-hunk'
                  : line.startsWith('diff ') || line.startsWith('+++') || line.startsWith('---')
                    ? 'diff-file'
                    : undefined
          }
        >
          {line || ' '}
        </div>
      ))}
    </pre>
  )
}

function DiffFileRow({
  file,
  section,
  done,
  open,
  onToggle,
}: {
  file: FileStat
  section: string | undefined
  done: boolean
  open: boolean
  onToggle: () => void
}) {
  return (
    <li>
      <button type="button" className="diff-file-row" aria-expanded={open} onClick={onToggle}>
        <span className="diff-file-caret" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
        <span className="diff-file-name">{file.path}</span>
        <span className="diff-file-stat">
          <span className="diff-stat-add">+{file.additions}</span>{' '}
          <span className="diff-stat-del">−{file.deletions}</span>
        </span>
      </button>
      {open &&
        (section ? (
          <DiffView diff={section} />
        ) : (
          <p className="diff-file-pending dim">
            {done ? 'no hunks — binary or mode change' : 'hunks still loading…'}
          </p>
        ))}
    </li>
  )
}

/**
 * A job's diff as a manifest of changed files, each row collapsing its hunks.
 * `done` is false while later pages of the diff are still arriving, so a file
 * whose hunks have not landed says so rather than reading as unchanged.
 */
export function DiffFiles({ files, diff, done }: { files: FileStat[]; diff: string; done: boolean }) {
  const sections = useMemo(() => diffSectionsByPath(diff), [diff])
  const [open, setOpen] = useState<Record<string, boolean>>({})
  const single = files.length === 1 ? files[0].path : null
  const setAll = (value: boolean) =>
    setOpen(Object.fromEntries(files.map((f) => [f.path, value])))

  if (files.length === 0) return <DiffView diff={diff} />

  return (
    <div className="diff-files">
      <div className="diff-files-actions">
        <button className="linklike" onClick={() => setAll(true)}>
          expand all
        </button>
        <button className="linklike" onClick={() => setAll(false)}>
          collapse all
        </button>
      </div>
      <ul className="diff-file-list">
        {files.map((f) => (
          <DiffFileRow
            key={f.path}
            file={f}
            section={sections.get(statPathOf(f.path))}
            done={done}
            open={open[f.path] ?? f.path === single}
            onToggle={() => setOpen((o) => ({ ...o, [f.path]: !(o[f.path] ?? f.path === single) }))}
          />
        ))}
      </ul>
    </div>
  )
}
