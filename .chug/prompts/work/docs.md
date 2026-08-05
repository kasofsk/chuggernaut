# Write the documentation page

You are working **on the chuggernaut codebase itself** — the platform running
you. The **Job Brief** appended below describes the reference/wiki page to write
or update. If there is no Job Brief, call `submit_result` with a summary
explaining that no ticket was provided, and exit non-zero.

A `docs` job maintains the **project wiki**: reference pages that *teach* —
how something works, how to use it, where to look. The repo's `docs/` tree is
the wiki root (`docs/spec.md` §9.4). Design/plan documents are a different job type
(`design`, under `docs/design/`); a `docs` page is reference material, not an
argument for a decision.

Before writing, orient yourself — don't re-derive what's documented:

- `docs/spec.md` — normative platform behavior (the source of truth); `docs/reference/crates.md` —
  the crate/module map; `docs/design/000-rationale.md` / `docs/reference/design-lifecycle.md` — rationale.
- **Read the code you are documenting, as it exists now.** Accuracy against the
  current code is the whole job — do not describe intended or remembered
  behavior. Spot-check every claim you make against the source.

Then:

1. Write or update the page under `docs/` — choose a location and filename that
   fit where a reader would look for it, and wire it into navigation (link it
   from a relevant index/page, and link out to related docs) with **relative**
   links. Pitch it at its audience: an operator guide, a contributor reference,
   and an API note are not written the same way — say who it is for.
2. Keep the change to the page(s) at hand — do not edit code or unrelated docs.
3. `.chug/tasks/doc-lint.sh` runs on your output: relative links must resolve and the
   markdown must be well-formed (closed code fences, spaced headings).
   Backtick'd code paths (e.g. `crates/api/src/routes.rs`) and restated
   constants are gated harder still — `.chug/tasks/check-doc-facts.sh` resolves
   them against git over the whole tree and **fails** the job, so keep them
   accurate or mark the line (docs/reference/style.md's doc-claim rule).
4. Commit to the current branch (you are already on the job branch) with a clear
   message, and push. `.githooks/pre-commit` runs the same doc lint on your
   staged markdown as an advisory pass — it prints what a `docs` job's stage-1
   gate will fail on, without blocking the commit. Act on what it reports.
5. Narrate as you go with the `update_status` tool — it streams live to the
   operator. Call it at least three times: your one-line plan right after
   reading the brief (`update_status("plan: ...")`), after the page is written,
   and just before you submit.
6. When done, call `submit_result` with:
   - `summary`: a short markdown report of what you wrote (this becomes the
     merge commit body). Structure it:
     - Open with **one plain sentence** stating the outcome — markdown-free; it
       is the readable first line of the squash commit body.
     - Then short `###` sections — `### What changed` (bulleted, per file or
       concern, inline code for paths/symbols), `### How verified` (commands run
       and their outcomes), `### Notes` (caveats, follow-ups, surprises). Omit
       any section that would be empty.
     - Prefer bullets to prose; no multi-sentence run-on paragraphs. Keep it
       brief — structure replaces neither brevity nor substance.
   - `structured`: `{ "files_changed": [...], "notes": "..." }`.
   - `cover_html` (optional): a small self-contained HTML cover page shown beside
     the summary in the UI. Only if it helps tell the story; never required,
     presentational only, never a substitute for `summary`. Keep it compact
     (rejected over 64KB).
7. Exit 0.

A reviewer will judge your page against the same brief — accuracy against the
current code (it will spot-check your claims), placement/navigation, and whether
it fits its audience. If it finds problems you will be re-invoked with its
findings.
