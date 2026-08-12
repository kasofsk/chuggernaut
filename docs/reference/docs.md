# The doc policy — how documentation works in this repo

**Audience:** whoever is about to write or change a document here — most often
the agent running a `docs`, `design`, `molt`, `code` or `web` job, and the
reviewer holding its output to these rules. This page is the rules, in the
present tense.
The argument for them, with the measurements that produced each one, is design
[#415](../design/415-knowledge-architecture.md); read it when you want to know
*why*, or before proposing a change to a rule.

Knowledge in this repo lives in docs, not comments — `.chug/tasks/check-comments.sh`
rejects every non-doc comment in the tree, so the explanation a comment would
have carried has exactly one place to go. That makes the docs load-bearing, and
these rules are what keep them worth loading.

## The two kinds, and their opposite update rules

There are two kinds of document and no third. Which one you are editing decides
everything else on this page.

| | **Reference** | **Design** |
| --- | --- | --- |
| Where | everything under `docs/` except `docs/design/`, plus `CLAUDE.md`, `web/CLAUDE.md` and the prose under `.chug/` | `docs/design/*.md`, and nothing else |
| What it holds | the system as it is now | one decision, and why it was taken |
| Tense | present, always | present in the head; dated past in the body |
| History | none — no `Status:` line, no changelog, no "we decided" | the whole point |
| How you edit it | rewrite the sentence that is now wrong, in place | rewrite the **head** freely; extend the **body** only by appending |

A **plan** is not a third kind: it is a design doc with a slice table, which is
why `docs/design/210-ts-rewrite-plan.md` and `docs/design/215-refactor-plan.md`
carry numbers and live beside the rest. A page that describes a future belongs
to one of the two update rules, or it belongs to neither and decays unobserved.

Two consequences worth stating outright, because they are the two mistakes that
recur:

- **A reference doc never narrates a change.** "As of job #412 the gate also
  checks…" is a sentence that is wrong the moment a later job changes it again.
  Write what the tree does; the why goes in the commit message, and what your
  change did goes in your work summary.
- **A design body is never edited in place.** A dated statement was true when it
  was written. If it is now wrong, append a correction — the shape already in the
  tree is `## Correction — YYYY-MM-DD, job #N (what it corrects)` — and link it
  from the head, so the head stays the one thing a reader has to read.

`.chug/tasks/docs-update.md` is the work-phase checklist that applies this table
during a `code` or `web` job, and `.chug/tasks/review-docs-updated.md` is the
evaluator that can fail the job for skipping it.

## A design doc: mutable head, append-only body

The **head** is the title, the `Status:` line, the decision table, the slice
table and any current-state section — everything before the argument begins. It
is rewritten to current truth whenever anything below it changes. Everything
after it is the record.

The head exists to bound the reading cost of *knowing where things stand*: the
design corpus is long enough that a doc nobody finishes is stale in the way that
matters, and reconstructing the present from an original plus N corrections is
not something a reader should have to do. There is no syntactic boundary to look
for — `docs/design/415-knowledge-architecture.md` marks it with a rule and a
`## The record` heading, most docs do not, and a `---` in a design doc is usually
just a section separator.

**One job type may delete a design doc outright, and none may rewrite a body.**
A `molt` job (`docs/design/533-molt.md`) sheds the corpus at a milestone, and its
licence is *deletion of a whole design whose `Status:` leads with `IMPLEMENTED`
and is not `IMPLEMENTED IN PART`* — never compaction of a body. That is the point
of the shape rather than a technicality: removing a file does not edit an
append-only body, so the exception stays a licensed **deletion** and append-only
needs no exception at all. Heads are already mutable, so compacting one needs no
licence. The jobs remain in git history and in the job records, so what a molt
destroys is the narration, never the record. `.chug/tasks/check-molt.sh` holds
the eligibility test mechanically, and `.chug/molt-ledger` records the named
class that licensed each shed.

### The header contract

The first two non-blank lines are the only part of a design document the
platform parses. Everything else in the tree is prose, enforced by reviewers and
by the gates below.

```markdown
# Design #321 — Job groups (tying a job to the thing it belongs to)

Status: IMPLEMENTED — shipped in jobs #324, #330, #331 and #332.

Written against the tree at `00dd0dc`. Every claim about current behavior below
was read out of `docs/spec.md` and the source in this repo; where the brief and the
tree disagree, the tree wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree).
```

1. **Line 1 is an `# ` heading** — the document's title, and what the operator
   UI's Designs view labels the row with. A document without one falls back to
   its slug.
2. **Line 3 is `Status:` and carries the status and nothing else.** One short,
   complete phrase, ending in a period.
3. **Everything else starts a new paragraph** — the provenance preamble
   ("written against the tree at `<sha>`, every claim verified against the
   source"), what shipped and in which jobs, what an amendment changed.

Both lines are read from the head of the file — `DOC_HEAD_LINES_MAX` is 32 —
and surfaced **verbatim** by `design_doc_head` in
[`crates/types/src/rollup.rs`](../../crates/types/src/rollup.rs): no vocabulary
is defined, nothing is validated, nothing is inferred. The status is truncated,
`DOC_STATUS_LEN_MAX` is 120 characters — deliberately, since it is display text
the platform compares to nothing — so a line that runs on into a paragraph is
served, and shown, cut mid-sentence. Keeping it inside the bound is the author's
job, not the platform's.

Two corollaries: **no markdown in the status line**, because it is surfaced
unparsed and `**amended**` renders as literal asterisks; and **the first
`Status:` line wins**, matched case-sensitively at the start of a line, so don't
indent it, don't bold it, and don't quote one inside the opening 32 lines.

The vocabulary in use — there is no schema, because a machine-checked status
would need a vocabulary the platform does not own:

| Value | Means |
| --- | --- |
| `PROPOSED` | Argued, not built. The default for a new design. |
| `IMPLEMENTED` | Built and merged. Name the jobs that did it. |
| `IMPLEMENTED IN PART` | Some slices shipped; say which, and what is still open. |
| `DRAFT` | Notes or an audit, not yet an argued proposal. |
| `FINDING` | A conclusion — often "don't do this yet" — rather than a proposal. |

Amending a status is an ordinary `design` or `docs` job against the document.
Nothing else writes it: the repo stays the source of truth for a design's
status, and the platform reports discrepancies without resolving them.

### How a document reaches the Designs view

Two joins, both derived at read time — nothing is stored:

- **Path → slug → group name.** `docs/design/321-job-groups.md` has slug
  `321-job-groups`, and the group a job carries to say it belongs to that design
  is `design/321-job-groups` (`DESIGN_GROUP_PREFIX` in
  [`crates/types/src/groups.rs`](../../crates/types/src/groups.rs)). The leading
  `<seq>-` is what the view sorts and labels by; it is a convention, not an
  identity — the path is the identity. `.chug/tasks/doc-lint.sh` rejects a new
  `docs/design/*.md` whose basename is not `{seq}-{slug}.md`.
- **Group name → jobs.** Every job whose `groups` list names that group is a
  member, and the roll-up is one pass over the project's job records. A design
  nobody has filed a job against is still a row, with an empty member list.

So **every `docs/design/*.md` becomes a row** — there is no opt-out and no index
file, and a `README.md` dropped in that directory would appear as a design
called "README". Pages *about* design documents (like this one) live elsewhere
under `docs/`. The listing is bounded — `DESIGNS_MAX` is 128 documents — and
anything past it is dropped from the reply and logged, never silently truncated.

The view flags a row **status stale** when the design has a member other than
its own authoring job (the job whose number *is* the design's), none of the
members is open, and the status line still says something. It is a reported
discrepancy, never an action — the platform will not edit your document — and
the derivation is deliberately broad, so a design with one closed job and nine
unwritten slices trips it too.

The separate **hide implemented** toggle hides the row whose status line *leads*
with `IMPLEMENTED`; `IMPLEMENTED IN PART` shares that leading token and is never
hidden, because the vocabulary above defines it as live work. That is why a
finished design can carry `status stale` and still be filtered away: the flag
reads the jobs, the toggle reads the word the document wrote about itself.

## One definition per concept; a mention is free

A concept is defined once, in the doc where it is argued, and
[`docs/concepts.md`](../concepts.md) is the routing table that says where. It
holds no definitions itself — a definition divorced from its argument is worth
less — and it names its own criterion for adding a row.

**A mention is free**, in any doc, as often as an argument needs one. What is
banned is a second *definition*, because a concept explained in two places
drifts in one of them and a reader cannot tell which. The enforced half is
syntactic and covers registered terms only, in two shapes: `**Term.**` opening a
list item, and `**Term** is|are|means|refers to` where the term opens a
sentence. An unregistered term is invisible however it is written.

When you need a registered term in a doc that does not own it, write the
mention and link the owner. When you believe this doc is now the right owner,
move the registry row rather than writing a second definition.

## `CLAUDE.md` and the prompts gloss; they never define

`CLAUDE.md`, `web/CLAUDE.md` and the prose under `.chug/prompts/` restate other
docs on purpose: they are read first, or injected into a context that may hold
nothing else, and a file of bare links would force a dozen doc-opens before work
starts.

> One line of gloss plus a link to the owner. Never a second definition.

They are held to the rule rather than exempted from it. A drifting gloss is far
less harmful than a competing definition, and the link gives a reader somewhere
to check — while the single most damaging stale directive this tree ever carried
(a normative instruction to protect a module that had been deleted) was in
`CLAUDE.md` itself, so a file-level exemption would exempt exactly the wrong
file.

## Every doc under `docs/` is catalogued

[`docs/README.md`](../README.md) carries a `## The catalogue` section with one
row per tracked `docs/**/*.md` — a link and a one-line summary — and it
catalogues itself, so there is no exception to remember.

**Adding a doc is two acts: the file, and its row.** `.chug/tasks/check-doc-facts.sh`
check 5 compares the catalogue against the tree in both directions and fails on
either mismatch, which is what keeps the second act from being the one everyone
forgets. Write the row in its directory's group — root docs, then `reference/`,
then `reference/runbooks/`, then `design/` by number — and write a summary a
stranger could route on:

```markdown
| [`docs/reference/testing.md`](reference/testing.md) | The test tiers, what each costs, and where a given test belongs |
```

Completeness of an index is not the same as being read: a catalogued doc can
still be cited by nothing. What the row buys is the moment of authorship — a doc
nobody can summarise in one line is a doc worth reconsidering before it merges.
Whether anything *else* names it is the ledger's second question, below, and the
catalogue is deliberately no answer to it — a row exists for every doc by
construction, so counting one would make that question constant.

## When you land a design slice, you update its design doc

In the same commit as the implementation, not in a follow-up job. You are the
only party who knows what *actually* landed versus what was designed; a later
`docs` job would be re-deriving that from your diff.

1. **Flip the slice row** to `**Landed** (job #N)` — `N` is your job — and say in
   the same cell what actually landed, not what the row proposed.
2. **Adjust the `Status:` line** and any "what is landed" sentence in the head.
   `Status: IMPLEMENTED` is a claim that *every* slice landed; if slices remain,
   it stays `IMPLEMENTED IN PART`.
3. **If what you built differs from what the body argues**, append a dated
   correction saying so and point the row at it.

The job doing the landing is exempt from the gate that checks these rows,
because its own `job/N` commit cannot exist yet when the commit is gated.

## What checks this, and what only reports it

Six gates read the docs. Five of them fail a job, and knowing which is which is
the difference between a rework cycle and a warning you can read at your
leisure.

| Gate | Runs | Judges | Verdict |
| --- | --- | --- | --- |
| `.chug/tasks/check-doc-facts.sh` | pre-stage of **every** job, whole-tree; `--staged` in the pre-commit hook | paths, restated constants, landed-slice rows, owned definitions, the catalogue, heading anchors | **error** |
| `.chug/tasks/check-comments.sh` | pre-stage of every job | non-doc comments, and the two-sentence cap on doc comments | **error** |
| `.chug/tasks/doc-lint.sh` | stage 1 of `docs`, `design` and `molt` jobs | markdown well-formedness, relative links, the `{seq}-{slug}.md` filename shape | **error** for those three job types |
| `.chug/tasks/review-docs-updated.md` | evaluation of every `code` and `web` job | cross-doc state claims, behavioural claims about symbols the diff touched, a design slice landed without its head updated | **error** — an agent evaluator, so it reads and never runs |
| `.chug/tasks/check-molt.sh` | stage 0 of `molt` jobs | the accounting of a shedding: a vanished landed-slice claim, a doc that lost its last referrer, a deletion that was not eligible, a deleted path still cited from a non-doc file with no stub, a shed with no ledger line | **error** for that one job type |
| `.chug/tasks/doc-staleness.sh` | every job, and the pre-commit hook | whether a file a doc names has moved since the doc did, and whether anything under `docs/` is named by nothing | **advisory** — *suspect*, not wrong |

Three things about that table are decisions rather than accidents:

- **No gate can ask whether a molt lost something.** Every other row catches a doc
  saying something *wrong*; shedding produces docs that say something *less*, and
  a gate that failed a diff for removing a true sentence would fail every molt.
  So `check-molt.sh` asks accounting instead, and the judgement — was the
  shedding well-aimed, and did a load-bearing fact die — belongs to two agent
  evaluators, one of which is instructed to refute rather than approve, and then
  to a **human**: `molt` is the only job type here that ends in an approval a
  person gives. That is not ceremony. The failure this most likely dies of is a
  shed rejected alternative, which names no path, constant or link, has no
  signature in a diff of legitimate deletions, and surfaces only months later
  when someone re-proposes the rejected thing with no argument to hand — so the
  last reader is the one who knows what the project is about to do next. A gate
  that cannot run is worse here than anywhere else, so an unresolvable base exits
  as a linter error: "nothing lost" and "never looked" must not print the same.
- **The fatal ones are all mechanical.** A gate that errors in the pre-stage of
  every job stops the fleet when it is wrong, so each of those checks refuses to
  judge what it cannot parse: an unresolvable token, an unrecognised assertion
  shape, a slice row in prose and a malformed catalogue row are skipped in
  silence rather than guessed at.
- **The ledger reports and does not block.** A doc is suspect because the code it
  names moved after it did, which is very often fine. A ledger that fails builds
  for history nobody in the commit caused is a ledger people disable — so the
  one case it blocks is a diff that edits a doc and then changes a non-doc file
  that doc names, which the author clears by re-reading the doc and *saying so*:
  a `Doc-reread: <path>` assertion, one line per doc, either as a trailer in a
  commit message on the branch or as a line the branch's diff adds to
  `.chug/doc-reread`. The assertion is the point — a timestamp records that a
  doc was edited, not that anyone read it. Only the second form survives a
  rebase, and a job branch is rebased on every merge-conflict rework, so a
  squashed or re-authored commit takes a trailer with it (job #482); the file is
  read from the diff and never from its contents, so a line already on the base
  branch asserts nothing.
- **The ledger's second half asks reach rather than truth.** Per tracked
  `docs/**/*.md`, how many other tracked `*.md` name it — by a backticked path
  claim or a relative link, prompts included, `docs/README.md` excluded. Zero is
  a finding and anything else is silent. It is advisory for the same reason and
  one of its own: a `PROPOSED` design doc is uncited until the work starts. It
  runs whole-tree only, so the pre-commit hook never prints it: reach is a
  property of the corpus, and a staged subset cannot answer it.

A claim that is *correctly* unresolvable is marked on its line rather than
rewritten: `<!-- intent -->`, `<!-- runtime -->`, `<!-- absent -->`. The three
are not interchangeable and
[`docs/reference/style.md`](style.md#tier-2--mechanical-rules-a-reviewer-checks-by-name)'s
doc-claim rule is where each one's meaning is stated.

## Related

- [design #415](../design/415-knowledge-architecture.md) — the argument for
  every rule on this page, and the measurements behind it.
- [design #533](../design/533-molt.md) — the argument behind the licensed
  deletion above, and the shedding it exists for. Its machinery is landed (S2,
  job #548); no molt has run yet.
- [`docs/reference/style.md`](style.md) — the blessed practices, including the
  doc-claim rule and the marker syntax.
- [`docs/concepts.md`](../concepts.md) — the concept registry, and the criterion
  for adding a row.
- [`docs/README.md`](../README.md) — the docs index and the catalogue.
- [`.chug/tasks/docs-update.md`](../../.chug/tasks/docs-update.md) — the
  work-phase task a `code` or `web` job follows to apply this policy.
- [`docs/spec.md`](../spec.md) §9.4 — documentation jobs and the docs tree.
