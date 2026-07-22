import { useMemo } from 'react'
import { Markdown } from './Markdown'

// Renders a claude `--output-format stream-json` transcript the way watching
// claude in a terminal reads: assistant prose as markdown, tool calls as compact
// expandable headers, tool results collapsed with a size hint, the final result
// as a highlighted completion block. Bootstrap/shell output (git clone chatter,
// cargo lines) isn't JSON — it stays inline as raw monospace, in sequence.
//
// Everything is drift-tolerant: an unknown event shape collapses to raw JSON, a
// line that doesn't parse falls back to raw text, and a JSON line split across
// tail chunks (the trailing, newline-less line) is left raw until it completes.

type Json = Record<string, unknown>

type Block =
  | { kind: 'raw'; text: string }
  | { kind: 'event'; event: Json; raw: string }

function isObject(v: unknown): v is Json {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

// A claude event is a JSON object carrying a string `type`. Fast-path out of the
// common case (a plain shell line) before spending a JSON.parse.
function tryParseEvent(line: string): Json | null {
  const t = line.trimStart()
  if (t[0] !== '{') return null
  try {
    const v = JSON.parse(t)
    if (isObject(v) && typeof v.type === 'string') return v
  } catch {
    /* not JSON — fall through to raw */
  }
  return null
}

/**
 * Split the tail buffer into an ordered list of blocks. Only complete lines
 * (those followed by a newline) are parsed as events; a trailing line without a
 * newline is a partial JSON chunk mid-transfer, so it renders raw until the next
 * append completes it. Consecutive non-event lines coalesce into one raw block
 * so shell output keeps its original inline sequence.
 */
function parseTranscript(text: string): Block[] {
  const blocks: Block[] = []
  const lines = text.split('\n')
  let rawBuf: string[] = []
  const flushRaw = () => {
    if (rawBuf.length) {
      blocks.push({ kind: 'raw', text: rawBuf.join('\n') })
      rawBuf = []
    }
  }
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    // The final element sits after the last '\n': it is either '' (buffer ended
    // cleanly) or a partial, still-arriving line. Never parse it.
    if (i === lines.length - 1) {
      if (line !== '') rawBuf.push(line)
      break
    }
    const ev = tryParseEvent(line)
    if (ev) {
      flushRaw()
      blocks.push({ kind: 'event', event: ev, raw: line })
    } else {
      rawBuf.push(line)
    }
  }
  flushRaw()
  return blocks
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / (1024 * 1024)).toFixed(1)} MB`
}

// Huge tool results would bloat the DOM; render a bounded head with a note.
const RESULT_CAP = 20_000
function capText(s: string): { text: string; capped: boolean } {
  if (s.length <= RESULT_CAP) return { text: s, capped: false }
  return { text: s.slice(0, RESULT_CAP), capped: true }
}

// One-line gist of a tool call's input — the command for Bash, the path for a
// file tool, etc. Falls back to compact JSON. Newlines collapse so it stays a
// single header line (CSS ellipsizes the overflow).
function toolSummary(input: unknown): string {
  if (!isObject(input)) return ''
  const first = (...keys: string[]) => {
    for (const k of keys) if (typeof input[k] === 'string') return input[k] as string
    return null
  }
  const s =
    first('command', 'file_path', 'path', 'pattern', 'url', 'query', 'prompt') ??
    JSON.stringify(input)
  return s.replace(/\s+/g, ' ').trim()
}

// Tool results arrive as a string, an array of content blocks, or (rarely) some
// other shape. Flatten to text, marking non-text parts rather than dropping them.
function resultText(content: unknown): string {
  if (typeof content === 'string') return content
  if (Array.isArray(content)) {
    return content
      .map((c) => {
        if (typeof c === 'string') return c
        if (isObject(c) && c.type === 'text' && typeof c.text === 'string') return c.text
        if (isObject(c) && c.type === 'image') return '[image]'
        return JSON.stringify(c)
      })
      .join('\n')
  }
  if (content == null) return ''
  return JSON.stringify(content, null, 2)
}

function TruncatedPre({ text, className }: { text: string; className?: string }) {
  const { text: shown, capped } = capText(text)
  return (
    <pre className={`tx-pre${className ? ' ' + className : ''}`}>
      {shown}
      {capped && <span className="tx-note dim">{`\n… ${fmtBytes(text.length)} total, truncated`}</span>}
    </pre>
  )
}

function RawJson({ raw, label }: { raw: string; label: string }) {
  return (
    <details className="tx-block tx-unknown">
      <summary>
        <span className="tx-kind">{label}</span>
      </summary>
      <TruncatedPre text={raw} />
    </details>
  )
}

function ContentBlock({ block }: { block: Json }) {
  const type = block.type
  if (type === 'text' && typeof block.text === 'string') {
    return <Markdown className="tx-assistant" text={block.text} />
  }
  if (type === 'thinking' || type === 'redacted_thinking') {
    const thinking = typeof block.thinking === 'string' ? block.thinking : '[redacted]'
    return (
      <details className="tx-block tx-thinking">
        <summary>
          <span className="tx-kind">thinking</span>
        </summary>
        <TruncatedPre text={thinking} />
      </details>
    )
  }
  if (type === 'tool_use') {
    const name = typeof block.name === 'string' ? block.name : 'tool'
    const summary = toolSummary(block.input)
    return (
      <details className="tx-block tx-tool">
        <summary>
          <span className="tx-kind">{name}</span>
          {summary && <span className="tx-arg">{summary}</span>}
        </summary>
        <TruncatedPre text={JSON.stringify(block.input ?? {}, null, 2)} />
      </details>
    )
  }
  if (type === 'tool_result') {
    const text = resultText(block.content)
    const isError = block.is_error === true
    return (
      <details className={`tx-block tx-result${isError ? ' tx-error' : ''}`}>
        <summary>
          <span className="tx-kind">{isError ? 'result · error' : 'result'}</span>
          <span className="tx-arg dim">{fmtBytes(text.length)}</span>
        </summary>
        <TruncatedPre text={text} />
      </details>
    )
  }
  // Unknown content block — show it rather than swallow it.
  return <RawJson raw={JSON.stringify(block, null, 2)} label={String(type ?? 'block')} />
}

function messageBlocks(event: Json): Json[] | null {
  const msg = event.message
  if (isObject(msg) && Array.isArray(msg.content)) return msg.content.filter(isObject) as Json[]
  return null
}

function ResultBlock({ event }: { event: Json }) {
  const isError = event.is_error === true || event.subtype === 'error_max_turns'
  const usage = isObject(event.usage) ? event.usage : {}
  const bits: string[] = []
  if (typeof event.duration_ms === 'number') bits.push(`${(event.duration_ms / 1000).toFixed(1)}s`)
  if (typeof event.num_turns === 'number') bits.push(`${event.num_turns} turns`)
  if (typeof event.total_cost_usd === 'number') bits.push(`$${event.total_cost_usd.toFixed(4)}`)
  const inTok = usage.input_tokens
  const outTok = usage.output_tokens
  if (typeof inTok === 'number' || typeof outTok === 'number') {
    bits.push(`${inTok ?? 0} in / ${outTok ?? 0} out tok`)
  }
  const result = typeof event.result === 'string' ? event.result : null
  return (
    <div className={`tx-final${isError ? ' tx-error' : ''}`}>
      <div className="tx-final-head">
        <span className="tx-kind">{isError ? 'ended · error' : 'completed'}</span>
        {bits.length > 0 && <span className="tx-final-meta dim">{bits.join(' · ')}</span>}
      </div>
      {result && <Markdown className="tx-assistant" text={result} />}
    </div>
  )
}

function EventBlock({ event, raw }: { event: Json; raw: string }) {
  switch (event.type) {
    case 'assistant':
    case 'user': {
      const blocks = messageBlocks(event)
      if (!blocks) return <RawJson raw={raw} label={String(event.type)} />
      return (
        <>
          {blocks.map((b, i) => (
            <ContentBlock key={i} block={b} />
          ))}
        </>
      )
    }
    case 'result':
      return <ResultBlock event={event} />
    case 'system': {
      const subtype = typeof event.subtype === 'string' ? event.subtype : ''
      const model = typeof event.model === 'string' ? ` · ${event.model}` : ''
      return (
        <details className="tx-block tx-system">
          <summary>
            <span className="tx-kind">{`system${subtype ? ' · ' + subtype : ''}`}</span>
            {model && <span className="tx-arg dim">{model.slice(3)}</span>}
          </summary>
          <TruncatedPre text={raw} />
        </details>
      )
    }
    default:
      // Unknown / future event type — collapsed raw JSON, never a crash.
      return <RawJson raw={raw} label={String(event.type)} />
  }
}

export function Transcript({ text }: { text: string }) {
  const blocks = useMemo(() => parseTranscript(text), [text])
  return (
    <div className="transcript">
      {blocks.map((b, i) =>
        b.kind === 'raw' ? (
          <pre key={i} className="tx-shell">
            {b.text}
          </pre>
        ) : (
          <EventBlock key={i} event={b.event} raw={b.raw} />
        ),
      )}
    </div>
  )
}
