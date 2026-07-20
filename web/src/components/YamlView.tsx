import type { ReactNode } from 'react'

/**
 * Dependency-free YAML display highlighting (same spirit as DiffView):
 * line-based tokenization good enough for job type files — comments, keys,
 * list dashes, quoted strings, and bare literals. Not a parser; a display
 * heuristic.
 */
export function YamlView({ yaml, full = false }: { yaml: string; full?: boolean }) {
  return (
    <pre className={full ? 'prompt yaml-full' : 'prompt'}>
      {yaml.split('\n').map((line, i) => (
        <div key={i}>{highlightLine(line)}</div>
      ))}
    </pre>
  )
}

function highlightLine(line: string): ReactNode {
  if (/^\s*#/.test(line)) return <span className="y-comment">{line || ' '}</span>
  if (!line) return ' '

  const m = line.match(/^(\s*)(- )?(?:([^\s:#][^:]*?)(:)( |$))?(.*)$/)
  if (!m) return line
  const [, indent, dash, key, colon, sep, rest] = m
  return (
    <>
      {indent}
      {dash && <span className="y-dash">{dash}</span>}
      {key !== undefined && (
        <>
          <span className="y-key">{key}</span>
          {colon}
          {sep}
        </>
      )}
      {rest !== undefined && highlightValue(rest)}
    </>
  )
}

function highlightValue(value: string): ReactNode {
  // Trailing comment, only when the value carries no quotes (heuristic).
  if (!value.includes('"') && !value.includes("'")) {
    const hash = value.search(/(^| )#/)
    if (hash >= 0) {
      const cut = value[hash] === '#' ? hash : hash + 1
      return (
        <>
          {classifyScalar(value.slice(0, cut))}
          <span className="y-comment">{value.slice(cut)}</span>
        </>
      )
    }
  }
  return classifyScalar(value)
}

function classifyScalar(v: string): ReactNode {
  const trimmed = v.trim()
  if (/^(['"]).*\1$/.test(trimmed)) return <span className="y-str">{v}</span>
  if (/^(true|false|null|~|-?\d+(\.\d+)?)$/.test(trimmed)) return <span className="y-lit">{v}</span>
  return v
}
