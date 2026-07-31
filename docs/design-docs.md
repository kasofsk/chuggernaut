# Design documents: the header contract

**Audience:** contributors writing or amending a document under
[`docs/design/`](./design/) — most often the agent running a `design` job, and
the reviewer checking its output. If you only want to *read* the designs, the
operator UI's **Designs** view is the friendlier surface; this page explains
where the two lines at the top of every document end up.

The design tree is prose. Nothing in a design document is enforced by the
platform, and the platform parses exactly **two** things out of one: the title
and the `Status:` line. Both are read from the first
[`DOC_HEAD_LINES_MAX`](../crates/types/src/rollup.rs) (32) lines, and both are
surfaced **verbatim** — no vocabulary is defined, nothing is validated, nothing
is inferred (design [#321](./design/321-job-groups.md) Decision 8).

That is the whole contract, and it is why the two conventions below matter:
they are the only leverage an author has over how the document appears
everywhere else.

## The opening of a design document

```markdown
# Design #321 — Job groups (tying a job to the thing it belongs to)

Status: IMPLEMENTED — shipped in jobs #324, #330, #331 and #332.

Written against the tree at `00dd0dc`. Every claim about current behavior below
was read out of `spec.md` and the source in this repo; where the brief and the
tree disagree, the tree wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree).
```

1. **Line 1 is an `# ` heading** — the document's title. It is what the Designs
   view labels the row with; a document without one falls back to its slug.
2. **Line 3 is `Status:` and carries the status and nothing else.** One short,
   complete phrase, ending in a period.
3. **Everything else starts a new paragraph** — the provenance preamble ("written
   against the tree at `<sha>`, every claim verified against the source"), what
   shipped and in which jobs, what an amendment changed, what a downstream
   reader should read first.

### Why the status line has to be short

`design_doc_head` in [`crates/types/src/rollup.rs`](../crates/types/src/rollup.rs)
takes the remainder of the **first** line beginning `Status:` and truncates it
to [`DOC_STATUS_LEN_MAX`](../crates/types/src/rollup.rs) — 120 characters. That
string is what `GET /api/v1/projects/{owner}/{project}/designs` serves and what
`web/src/pages/Designs.tsx` renders. A `Status:` line that runs on into a
paragraph is therefore served, and shown, cut mid-sentence:

```text
Status: PROPOSED. Written against the tree at `470cc0c` (2026-07-30). Every
```

The truncation is deliberate — the status is display text the platform compares
to nothing, so an over-long line is trimmed rather than refused. Keeping the
line inside the bound is the author's job, not the platform's.

Two corollaries:

- **No markdown in the status line.** It is surfaced unparsed, so `**amended**`
  renders as literal asterisks.
- **The first `Status:` line wins**, matched case-sensitively at the start of a
  line. Don't indent it, don't bold it, and don't quote a `Status:` line inside
  the opening 32 lines of prose.

### The vocabulary

There is no schema and no enforcement, by design — a machine-checked status
would need a vocabulary the platform does not own. The values in use today:

| Value | Means |
| --- | --- |
| `PROPOSED` | Argued, not built. The default for a new design. |
| `IMPLEMENTED` | Built and merged. Name the jobs that did it. |
| `IMPLEMENTED IN PART` | Some slices shipped; say which, and what is still open. |
| `DRAFT` | Notes or an audit, not yet an argued proposal. |
| `FINDING` | A conclusion — often "don't do this yet" — rather than a proposal. |

Amending a status is an ordinary `design` (or `docs`) job against the document.
Nothing else writes it: **the repo stays the source of truth for a design's
status**, and the platform reports discrepancies without resolving them.

## How a document reaches the Designs view

Two independent joins, both derived at read time — nothing is stored:

- **Path → slug → group name.** `docs/design/321-job-groups.md` has slug
  `321-job-groups`, and the group name a job carries to say it belongs to that
  design is `design/321-job-groups`
  ([`crates/types/src/groups.rs`](../crates/types/src/groups.rs),
  `DESIGN_GROUP_PREFIX`). The leading `<seq>-` is what the view sorts and labels
  by; it is a convention, not an identity — the path is the identity.
- **Group name → jobs.** Every job whose `groups` list names that group is a
  member, and the roll-up (`counts`, `open`) is one pass over the project's job
  records. A design nobody has filed a job against is still a row, with an empty
  member list.

Two consequences worth knowing before you add a file:

- **Every `docs/design/*.md` becomes a row.** There is no opt-out and no index
  file — a `README.md` dropped in that directory would show up as a design
  called "README". Pages *about* design documents (like this one) live one level
  up, under `docs/`.
- **The listing is bounded** at `DESIGNS_MAX` (128) documents; anything past it
  is dropped from the reply and logged, never silently truncated.

### `status_stale`

The view flags a row `status stale` when the design **has** members, **none** is
open, and the status line still says something. It is a reported discrepancy,
never an action: the platform will not edit your document. (The derivation is
deliberately broad — a design with one closed job and nine unwritten slices
trips it too.)

### Hiding finished designs

The index has a **hide implemented** toggle beside the lens and sort controls.
It composes with the lens rather than replacing it — "hide finished" is
orthogonal to "show stale" — and it is client-side only: the API sends every
row, and the choice is not remembered across reloads.

What it hides is the row whose status line *leads* with `IMPLEMENTED`, read
from the same leading token the status badge shows. `IMPLEMENTED IN PART` is
never hidden: the vocabulary above defines it as live work with open slices,
and it shares that leading token. A design with no `Status:` line is never
hidden either.

This is why a finished design can carry `status stale` and still be filtered
away: the flag and the toggle read different things — the flag reads the jobs
(the rule above never looks at the status token), the toggle reads the word the
document wrote about itself.

## Related

- [`spec.md`](../spec.md) §9.4 — documentation jobs and the docs tree; §1.1
  (`groups`), §6.2 (`GET .../groups`, `GET .../designs`).
- [`.chug/prompts/work/design.md`](../.chug/prompts/work/design.md) — the work
  prompt a `design` job runs under.
- [design #321](./design/321-job-groups.md) — why groups are derived rather than
  stored, and Decision 8 on status hygiene without a second writer.
- [`STYLE.md`](../STYLE.md), [`NORTH-STAR.md`](../NORTH-STAR.md) — the blessed
  practices and the structural direction a design is held to.
