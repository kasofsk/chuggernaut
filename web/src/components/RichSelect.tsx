import { useEffect, useRef, useState, type KeyboardEvent } from 'react'

export type RichOption = {
  value: string
  label: string
  /** longer explanation, shown under the label in the menu */
  description?: string
  /** short monospace detail (e.g. the slug), shown next to the label */
  detail?: string
}

/**
 * A select whose menu items are rich rows (label + description + detail) —
 * native <option> can't be styled. Closes on outside click or Escape;
 * arrow keys + Enter work like a native select.
 */
export function RichSelect({
  options,
  value,
  onChange,
  placeholder,
}: {
  options: RichOption[]
  value: string
  onChange: (value: string) => void
  placeholder?: string
}) {
  const [open, setOpen] = useState(false)
  const [hi, setHi] = useState(-1)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onDoc)
    return () => document.removeEventListener('mousedown', onDoc)
  }, [open])

  const selected = options.find((o) => o.value === value)

  function openMenu() {
    setOpen(true)
    setHi(Math.max(0, options.findIndex((o) => o.value === value)))
  }

  function pick(i: number) {
    onChange(options[i].value)
    setOpen(false)
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      setOpen(false)
      return
    }
    if (!open) {
      if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
        e.preventDefault()
        openMenu()
      }
      return
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setHi((h) => Math.min(options.length - 1, h + 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setHi((h) => Math.max(0, h - 1))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      if (hi >= 0 && hi < options.length) pick(hi)
    }
  }

  return (
    <div className="rich-select" ref={ref} onKeyDown={onKey}>
      <button
        type="button"
        className="rich-select-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => (open ? setOpen(false) : openMenu())}
      >
        {selected ? (
          <span className="rich-select-value">
            <b>{selected.label}</b>
            {selected.description && <span className="dim"> — {selected.description}</span>}
          </span>
        ) : (
          <span className="rich-select-value dim">{placeholder ?? 'select…'}</span>
        )}
        <span className="rich-select-caret">▾</span>
      </button>
      {open && (
        <div className="rich-select-menu" role="listbox">
          {options.map((o, i) => (
            <button
              type="button"
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              className={
                'rich-option' +
                (o.value === value ? ' rich-option-selected' : '') +
                (i === hi ? ' rich-option-hi' : '')
              }
              onMouseEnter={() => setHi(i)}
              onClick={() => pick(i)}
            >
              <span className="rich-option-title">
                {o.label}
                {o.detail && <span className="dim type-slug"> · {o.detail}</span>}
              </span>
              {o.description && <span className="rich-option-desc dim">{o.description}</span>}
            </button>
          ))}
          {options.length === 0 && <div className="rich-option-empty dim">no options</div>}
        </div>
      )}
    </div>
  )
}
