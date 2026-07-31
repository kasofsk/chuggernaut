import { useEffect, useState } from 'react'
import { ApiError, api, type Input, type InputKind, type JobTypeDetail } from '../api'
import { RichSelect } from './RichSelect'

/**
 * Declared job inputs on a form (spec §1.1 `inputs:`, design #311 slice B).
 *
 * One place, because the create form and the Draft editor render the same
 * declaration against the same rules and the same 422 vocabulary — and because
 * an input is the one job field whose *shape* comes from the project's repo, so
 * a second copy of these rules would drift from the server's.
 *
 * What lives here: the declaration fetch, the field rendering, the client-side
 * pre-validation that mirrors the creation-time 422, and the mapping of an
 * `inputs.{name}` error back onto its field. What deliberately does NOT: any
 * release-time semantic (required-presence, undeclared names) — the server is
 * the authority, and a client check must never block a submit it would accept.
 */

/** The default charset every value clears whatever its declaration
 *  (`types::inputs::INPUT_VALUE_PATTERN`): alphanumerics plus seven punctuation
 *  characters, at most {@link INPUT_VALUE_LEN_MAX} of them. An input value is an
 *  identifier, not prose — it can reach a `run:` script that crosses further
 *  shells. Kept as one regex so the length bound and the charset are the one
 *  check the server runs. */
const VALUE_CHARSET = /^[A-Za-z0-9._:/@+-]{1,256}$/

/** `types::inputs::INPUT_VALUE_LEN_MAX` — characters, which is also bytes. */
const INPUT_VALUE_LEN_MAX = 256

/** Prefix on the `field` of a release-time validation error for one input. */
const INPUT_FIELD_PREFIX = 'inputs.'

/**
 * The job type's definition at default-branch HEAD, refetched when the selected
 * type changes; null while in flight and on failure (the form still works —
 * only the declared-input fields are missing). Shared by the create form and
 * the Draft editor, which both let the operator change `type` mid-compose.
 */
export function useJobTypeDetail(owner: string, project: string, type: string): JobTypeDetail | null {
  const [detail, setDetail] = useState<JobTypeDetail | null>(null)
  useEffect(() => {
    if (!type) {
      setDetail(null)
      return
    }
    let live = true
    api.jobType(owner, project, type).then(
      (d) => live && setDetail(d),
      () => live && setDetail(null),
    )
    return () => {
      live = false
    }
  }, [owner, project, type])
  return detail
}

/** The kinds this build renders natively. An unknown kind from a newer
 *  dispatcher (spec §14 N+1 tolerance) renders as a text field and says so
 *  rather than crashing or hiding the input. */
function isKnownKind(kind: string): kind is InputKind {
  return kind === 'string' || kind === 'enum'
}

/** A declared `pattern` as a whole-value matcher. Null when JavaScript cannot
 *  compile it: the server's dialect is Rust's `regex`, not JS's, so an
 *  uncompilable pattern is a check this client skips — never a verdict it
 *  invents. */
function wholeValueMatcher(pattern: string): RegExp | null {
  try {
    return new RegExp(`^(?:${pattern})$`)
  } catch {
    return null
  }
}

/**
 * Why this value would be refused, or null if it passes what the client can
 * check: the charset floor and the length bound (the creation-time 422), then
 * the declaration's own narrowing (`values` for an `enum`, `pattern` for a
 * `string`).
 *
 * An **empty** value is never an error here. A missing `required` input is a
 * release-time rejection, not a creation one — the server accepts a Draft or
 * Frozen job without it, so refusing the submit would block something it would
 * accept. The `(required)` marker on the label is what carries that.
 */
export function inputValueError(input: Input, value: string): string | null {
  if (!value) return null
  if (!VALUE_CHARSET.test(value))
    return value.length > INPUT_VALUE_LEN_MAX
      ? `${value.length} characters, over the ${INPUT_VALUE_LEN_MAX}-character limit`
      : 'inputs are identifiers: letters, digits and . _ : / @ + - only'
  switch (input.type) {
    case 'enum':
      return (input.values ?? []).includes(value)
        ? null
        : `not one of ${(input.values ?? []).join(', ')}`
    case 'string': {
      const re = input.pattern ? wholeValueMatcher(input.pattern) : null
      return re && !re.test(value) ? `does not match ${input.pattern}` : null
    }
    default:
      return null
  }
}

/** Every supplied value's client-side verdict, keyed by input name. Empty means
 *  the form is submittable as far as the client can tell. */
export function inputValueErrors(
  declared: Input[],
  values: Record<string, string>,
): Record<string, string> {
  const errors: Record<string, string> = {}
  for (const input of declared) {
    const message = inputValueError(input, values[input.name] ?? '')
    if (message) errors[input.name] = message
  }
  return errors
}

/**
 * A value map as a request field: trimmed, blanks dropped, and `undefined` when
 * nothing is left. Absent rather than empty on both counts — a job type
 * declaring no inputs must produce the request body it produces today, byte for
 * byte, and an optional input left blank must stay *absent* rather than arrive
 * as an empty string the server refuses.
 */
export function inputsOrUndefined(
  values: Record<string, string>,
): Record<string, string> | undefined {
  const supplied: Record<string, string> = {}
  for (const [name, value] of Object.entries(values)) {
    const trimmed = value.trim()
    if (trimmed) supplied[name] = trimmed
  }
  return Object.keys(supplied).length ? supplied : undefined
}

/** {@link inputsOrUndefined} narrowed to what the type declares, in declaration
 *  order: a value for an input the selected type does not declare is one the
 *  server would reject as undeclared at release, so it is never sent. */
export function suppliedInputs(
  declared: Input[],
  values: Record<string, string>,
): Record<string, string> | undefined {
  const picked: Record<string, string> = {}
  for (const input of declared)
    if (values[input.name] !== undefined) picked[input.name] = values[input.name]
  return inputsOrUndefined(picked)
}

/**
 * Per-input messages carried by a rejected create/patch/release, keyed by input
 * name. Both §6.5 error shapes are read because inputs are checked in two
 * passes (spec §2.2): the creation pass answers one `inputs: input 'sha': …`
 * message, and release validation answers the `{errors: [{field, message}]}`
 * envelope with `field: "inputs.{name}"`. Anything else maps to nothing and the
 * caller's banner still shows it.
 */
export function inputFieldErrors(err: unknown): Record<string, string> {
  if (!(err instanceof ApiError) || typeof err.body !== 'object' || !err.body) return {}
  const body = err.body as { error?: unknown; errors?: unknown }
  const found: Record<string, string> = {}
  if (Array.isArray(body.errors))
    for (const entry of body.errors) {
      const { field, message } = (entry ?? {}) as { field?: unknown; message?: unknown }
      if (typeof field === 'string' && field.startsWith(INPUT_FIELD_PREFIX) && typeof message === 'string')
        found[field.slice(INPUT_FIELD_PREFIX.length)] = message
    }
  if (typeof body.error === 'string') {
    const one = /^inputs: input '([a-z][a-z0-9_]*)': ([\s\S]*)$/.exec(body.error)
    if (one) found[one[1]] = one[2]
  }
  return found
}

/** The parenthetical hint beside an input's name: whether it must be supplied,
 *  its description, and last — for a kind this build doesn't know — that it is
 *  being rendered as text, which is a caveat about the control rather than
 *  something the operator needs before reading what the input is for. */
function inputHint(input: Input): string {
  const parts = [input.required ? 'required' : 'optional']
  if (input.description) parts.push(input.description)
  if (!isKnownKind(input.type))
    parts.push(`unrecognized type "${input.type}", rendered as text`)
  return parts.join(' — ')
}

/** A declared `default` is shown as a *placeholder*, never as a pre-filled
 *  value: the platform materializes it onto the job record at the Ready
 *  transition (#311 "when a default becomes a value"), so pre-filling would
 *  submit as an operator-supplied value and make the audit trail lie about who
 *  chose it. Mirrors how the timeout/model fields show their type default. */
function inputPlaceholder(input: Input): string | undefined {
  return input.default ? `${input.default} (default)` : undefined
}

/** An `enum`'s menu: its declared values, preceded for an optional input by the
 *  choice that leaves it unsupplied — otherwise a picked value could not be
 *  taken back, and for an input with a `default` that means losing the default
 *  the platform would have filled in. Its label names the default rather than
 *  repeating it, so it can't be mistaken for the declared value of the same
 *  name sitting under it. */
function enumOptions(input: Input) {
  const unsupplied = {
    value: '',
    label: input.default ? `unset — default: ${input.default}` : 'unset',
  }
  const declared = (input.values ?? []).map((v) => ({ value: v, label: v }))
  return input.required ? declared : [unsupplied, ...declared]
}

/**
 * One field per declared input, in declaration order. Renders **nothing** —
 * not an empty section, not a heading — for a type that declares none, which is
 * every job type that predates the feature.
 */
export function JobInputFields({
  declared,
  values,
  onChange,
  serverErrors = {},
  fieldClassName = 'field',
  onFieldFocus,
  onFieldBlur,
}: {
  declared: Input[]
  /** current form values by input name; a missing key is an unsupplied input */
  values: Record<string, string>
  onChange: (name: string, value: string) => void
  /** messages from a rejected request ({@link inputFieldErrors}); they win over
   *  the live client verdict, being the authority's word on the same value */
  serverErrors?: Record<string, string>
  /** wrapper class, so the Draft editor can flash a remotely-edited field */
  fieldClassName?: string
  onFieldFocus?: () => void
  onFieldBlur?: () => void
}) {
  if (!declared.length) return null
  return (
    <>
      {declared.map((input) => {
        const value = values[input.name] ?? ''
        const message = serverErrors[input.name] ?? inputValueError(input, value)
        const contents = (
          <>
            <span>
              {input.name} <span className="dim">({inputHint(input)})</span>
            </span>
            {input.type === 'enum' ? (
              <RichSelect
                value={value}
                onChange={(v) => onChange(input.name, v)}
                placeholder="pick a value…"
                options={enumOptions(input)}
              />
            ) : (
              <input
                value={value}
                placeholder={inputPlaceholder(input)}
                onFocus={onFieldFocus}
                onBlur={onFieldBlur}
                onChange={(e) => onChange(input.name, e.target.value)}
              />
            )}
            {message && <span className="input-error">{message}</span>}
          </>
        )
        return input.type === 'enum' ? (
          <div className={fieldClassName} key={input.name}>
            {contents}
          </div>
        ) : (
          <label className={fieldClassName} key={input.name}>
            {contents}
          </label>
        )
      })}
    </>
  )
}
