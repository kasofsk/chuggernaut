/**
 * Group names, client-side (design #321 Decision 2): what a name may look like,
 * and where a chip carrying one goes.
 *
 * The server is the authority — `types::groups::check_groups` is the one
 * validator all three write paths share — so the checks here are the same
 * courtesy the declared inputs do: they say no before the round trip, never
 * instead of it. A group is a **label**, not a reference: a `design/` name whose
 * document does not exist is a working group, so nothing here resolves anything.
 */

/** Most groups one job may carry (`GROUPS_COUNT_MAX`). */
export const GROUPS_COUNT_MAX = 8
/** Longest accepted name in characters (`GROUP_NAME_LEN_MAX`); ASCII-only. */
export const GROUP_NAME_LEN_MAX = 128
/** Mirrors `GROUP_NAME_PATTERN`: lowercase, and never opening with `/` or `.`. */
const GROUP_NAME_PATTERN = /^[a-z0-9][a-z0-9._/-]*$/
/** The namespace whose members conventionally name a `docs/design/` document. */
export const DESIGN_GROUP_PREFIX = 'design/'

/**
 * Why this name can't join `existing`, or null when it can — one message per
 * rule, in the same order the dispatcher checks them so the wording an operator
 * sees before the round trip matches the wording after one.
 */
export function groupNameError(name: string, existing: string[]): string | null {
  if (!name) return null
  if (existing.includes(name)) return 'already on this job'
  if (existing.length >= GROUPS_COUNT_MAX) return `at most ${GROUPS_COUNT_MAX} groups per job`
  if (name.length > GROUP_NAME_LEN_MAX) return `over ${GROUP_NAME_LEN_MAX} characters`
  if (!GROUP_NAME_PATTERN.test(name))
    return 'lowercase letters, digits and . _ / - only, starting with a letter or digit'
  return null
}

/**
 * Where a group chip leads. A `design/{slug}` name is a document, so it opens
 * that design; anything else has no page of its own, so it opens the jobs list
 * filtered to it — which is the group view for an unnamespaced group. A slug
 * carrying its own `/` is not a design route, so it falls back to the filter.
 */
export function groupHref(owner: string, project: string, name: string): string {
  const slug = name.startsWith(DESIGN_GROUP_PREFIX)
    ? name.slice(DESIGN_GROUP_PREFIX.length)
    : ''
  if (slug && !slug.includes('/')) return `/p/${owner}/${project}/designs/${encodeURIComponent(slug)}`
  return `/p/${owner}/${project}?group=${encodeURIComponent(name)}`
}
