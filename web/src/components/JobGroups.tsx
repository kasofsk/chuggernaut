import { useEffect, useId, useState, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { api } from '../api'
import { GROUPS_COUNT_MAX, groupHref, groupNameError } from '../groups'

/**
 * Job groups in the UI (design #321): what a job is part of. Two components and
 * one hook, shared by every surface that shows or edits them — the jobs table's
 * chips, the job page's editor, the create form and the draft editor — so the
 * chip, the picker and the vocabulary cannot drift into three versions.
 *
 * A group is inert to execution and mutable in **every** state, terminal
 * included: annotating a job that already finished is the case the feature
 * exists for, so nothing here gates on state.
 */

/** Read-only chips for a jobs-table row: compact, wrapping, each a link to what
 *  the label refers to (a design document, or the list filtered to the group).
 *  Renders nothing at all for an ungrouped job, which is most of them. */
export function GroupChips({
  owner,
  project,
  groups,
}: {
  owner: string
  project: string
  groups: string[] | undefined
}) {
  if (!groups?.length) return null
  return (
    <div className="group-chips">
      {groups.map((g) => (
        <Link key={g} className="group-chip" to={groupHref(owner, project, g)} title={g}>
          {g}
        </Link>
      ))}
    </div>
  )
}

/**
 * The add/remove editor: a chip with an × per group, plus an input that offers
 * the names already in use. A **picker over known names, not referential
 * integrity** — a name nobody has used yet is a valid group, and a `design/`
 * name whose document is missing still works, so the input takes free text and
 * the vocabulary is only a suggestion list.
 *
 * The owner decides what an add means: the job page PUTs it, the create form and
 * the draft editor hold it in the payload they are composing.
 */
export function GroupPicker({
  value,
  options,
  onAdd,
  onRemove,
  fieldClassName = 'field',
  labelExtra,
  disabled = false,
}: {
  value: string[]
  /** names to suggest — the project's groups plus its design documents */
  options: string[]
  onAdd: (name: string) => void
  onRemove: (name: string) => void
  /** lets the draft editor add its flash/pulse classes to the field */
  fieldClassName?: string
  /** the draft editor's remote-edit wand, rendered after the label */
  labelExtra?: ReactNode
  /** true while a write is in flight, so a second tap can't race it */
  disabled?: boolean
}) {
  const listId = useId()
  const [draft, setDraft] = useState('')
  const [error, setError] = useState<string | null>(null)

  function add() {
    const name = draft.trim()
    if (!name) return
    const bad = groupNameError(name, value)
    if (bad) {
      setError(`${name}: ${bad}`)
      return
    }
    setError(null)
    setDraft('')
    onAdd(name)
  }

  return (
    <div className={fieldClassName}>
      <span>
        Groups{' '}
        <span className="dim">
          (what this job is part of — e.g. <code>design/321-job-groups</code>)
        </span>
        {labelExtra}
      </span>
      {value.length > 0 && (
        <div className="tag-row">
          {value.map((g) => (
            <button
              type="button"
              key={g}
              className="tag tag-on"
              title={`remove ${g}`}
              disabled={disabled}
              onClick={() => onRemove(g)}
            >
              {g} ×
            </button>
          ))}
        </div>
      )}
      <div className="group-add">
        <input
          list={listId}
          value={draft}
          placeholder={value.length >= GROUPS_COUNT_MAX ? 'at the 8-group limit' : 'design/…, or any label'}
          aria-label="Add a group"
          disabled={disabled}
          onChange={(e) => {
            setDraft(e.target.value)
            setError(null)
          }}
          onKeyDown={(e) => {
            if (e.key !== 'Enter') return
            e.preventDefault()
            add()
          }}
        />
        <button type="button" className="link" disabled={disabled || !draft.trim()} onClick={add}>
          + add
        </button>
      </div>
      <datalist id={listId}>
        {options
          .filter((o) => !value.includes(o))
          .map((o) => (
            <option key={o} value={o} />
          ))}
      </datalist>
      {error && <div className="error">{error}</div>}
    </div>
  )
}

/**
 * The name vocabulary the picker suggests: the groups the project's jobs already
 * carry, plus a `design/{slug}` for every document under `docs/design/` —
 * including the designs nobody has filed a job against yet, which is exactly the
 * row the groups read cannot represent. Both reads are best-effort: a picker
 * with no suggestions still takes free text.
 */
export function useGroupOptions(owner: string, project: string): string[] {
  const [options, setOptions] = useState<string[]>([])
  useEffect(() => {
    let live = true
    Promise.all([
      api.groups(owner, project).catch(() => []),
      api.designs(owner, project).catch(() => []),
    ]).then(([gs, ds]) => {
      if (!live) return
      const names = [...gs.map((g) => g.name), ...ds.map((d) => d.name)]
      setOptions([...new Set(names)].sort((a, b) => a.localeCompare(b)))
    })
    return () => {
      live = false
    }
  }, [owner, project])
  return options
}
