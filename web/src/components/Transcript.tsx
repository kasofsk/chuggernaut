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

// Character count for a thinking run's summary — '812 chars', '~2.1k chars'.
function fmtChars(n: number): string {
  if (n < 1000) return `${n} chars`
  return `~${(n / 1000).toFixed(1)}k chars`
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

function SystemBlock({ event, raw }: { event: Json; raw: string }) {
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

// One member of a thinking run: a `thinking` content-block segment, or a
// thinking-flavored `system` event (a `thinking_tokens` heartbeat). Both are
// "thinking noise" that shouldn't drown the readable rows, so they bundle
// together; the raw event is kept so a heartbeat stays individually inspectable.
type ThinkMember =
  | { kind: 'seg'; text: string }
  | { kind: 'sys'; event: Json; raw: string }

// A `system` event is thinking-flavored when its subtype names thinking (e.g.
// `thinking_tokens` heartbeats emitted while an agent reasons). Matching on the
// substring tolerates variants without enumerating them.
function isThinkingSystem(event: Json): boolean {
  return event.type === 'system' && typeof event.subtype === 'string' && event.subtype.includes('thinking')
}

function memberChars(m: ThinkMember): number {
  return m.kind === 'seg' ? m.text.length : m.raw.length
}

// A run of one or more consecutive thinking-flavored members, rendered as a
// single collapsed entry so a long stream of thinking (content blocks *and*
// `thinking_tokens` heartbeats alike) doesn't drown the readable rows. The
// summary carries the member count and total size; expanding reveals the members
// in order, each still individually expandable. A lone plain segment renders like
// an ordinary thinking block (no bundle chrome).
function ThinkingRun({ members }: { members: ThinkMember[] }) {
  if (members.length === 1 && members[0].kind === 'seg') {
    return (
      <details className="tx-block tx-thinking">
        <summary>
          <span className="tx-kind">thinking</span>
        </summary>
        <TruncatedPre text={members[0].text} />
      </details>
    )
  }
  const total = members.reduce((n, m) => n + memberChars(m), 0)
  const label = `thinking (${members.length} segments, ${fmtChars(total)})`
  return (
    <details className="tx-block tx-thinking">
      <summary>
        <span className="tx-kind">{label}</span>
      </summary>
      <div className="tx-think-segs">
        {members.map((m, i) =>
          m.kind === 'seg' ? (
            <details key={i} className="tx-block tx-thinking tx-think-seg">
              <summary>
                <span className="tx-kind">thinking</span>
              </summary>
              <TruncatedPre text={m.text} />
            </details>
          ) : (
            <SystemBlock key={i} event={m.event} raw={m.raw} />
          ),
        )}
      </div>
    </details>
  )
}

// A flat, ordered render item. Flattening events into this list is what lets a
// thinking run span content-block *and* event boundaries: consecutive thinking
// segments coalesce into one item regardless of how the stream chunked them.
type Item =
  | { kind: 'raw'; text: string }
  | { kind: 'thinking'; members: ThinkMember[] }
  | { kind: 'content'; block: Json }
  | { kind: 'result'; event: Json }
  | { kind: 'system'; event: Json; raw: string }
  | { kind: 'unknown'; raw: string; label: string }

// Flatten blocks into render items, coalescing runs of consecutive thinking-
// flavored members — `thinking` content blocks and `thinking_tokens` system
// heartbeats alike — into a single item. Any non-thinking item (assistant text,
// tool_use, tool_result, result, ordinary system, shell output) breaks the run;
// a later thinking member opens a fresh bundle.
function flattenBlocks(blocks: Block[]): Item[] {
  const items: Item[] = []
  const pushThink = (member: ThinkMember) => {
    const last = items[items.length - 1]
    if (last && last.kind === 'thinking') last.members.push(member)
    else items.push({ kind: 'thinking', members: [member] })
  }
  for (const b of blocks) {
    if (b.kind === 'raw') {
      items.push({ kind: 'raw', text: b.text })
      continue
    }
    const event = b.event
    switch (event.type) {
      case 'assistant':
      case 'user': {
        const cblocks = messageBlocks(event)
        if (!cblocks) {
          items.push({ kind: 'unknown', raw: b.raw, label: String(event.type) })
          break
        }
        for (const cb of cblocks) {
          if (cb.type === 'thinking' || cb.type === 'redacted_thinking') {
            pushThink({ kind: 'seg', text: typeof cb.thinking === 'string' ? cb.thinking : '[redacted]' })
          } else {
            items.push({ kind: 'content', block: cb })
          }
        }
        break
      }
      case 'result':
        items.push({ kind: 'result', event })
        break
      case 'system':
        // A thinking-flavored heartbeat joins the current thinking bundle;
        // ordinary system events render on their own and break the run.
        if (isThinkingSystem(event)) pushThink({ kind: 'sys', event, raw: b.raw })
        else items.push({ kind: 'system', event, raw: b.raw })
        break
      default:
        // Unknown / future event type — collapsed raw JSON, never a crash.
        items.push({ kind: 'unknown', raw: b.raw, label: String(event.type) })
    }
  }
  return items
}

export function Transcript({ text }: { text: string }) {
  const items = useMemo(() => flattenBlocks(parseTranscript(text)), [text])
  return (
    <div className="transcript">
      {items.map((it, i) => {
        switch (it.kind) {
          case 'raw':
            return (
              <pre key={i} className="tx-shell">
                {it.text}
              </pre>
            )
          case 'thinking':
            return <ThinkingRun key={i} members={it.members} />
          case 'content':
            return <ContentBlock key={i} block={it.block} />
          case 'result':
            return <ResultBlock key={i} event={it.event} />
          case 'system':
            return <SystemBlock key={i} event={it.event} raw={it.raw} />
          case 'unknown':
            return <RawJson key={i} raw={it.raw} label={it.label} />
        }
      })}
    </div>
  )
}
