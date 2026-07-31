# Write the design document

You are working **on the chuggernaut codebase itself** — the platform running
you. The **Job Brief** appended below describes the design question to resolve.
If there is no Job Brief, call `submit_result` with a summary explaining that no
ticket was provided, and exit non-zero.

A `design` job produces **one markdown document** — an architecture/plan that
argues a decision and its tradeoffs and sets direction — under
`docs/design/<slug>.md`. It writes prose, not code: the implementation lands in
the `code`/`web` jobs that depend on this one and cite it. The repo's `docs/`
tree is the project wiki (`spec.md` §9.4); design docs live under `docs/design/`.

Before writing, orient yourself — don't re-derive what's documented:

- `spec.md` — normative platform behavior; the source of truth. `design.md` and
  `design-lifecycle.md` — existing rationale. `crates.md` — the crate/module map.
- Read the code and docs your design touches **at the state they exist in now**.
  A design that silently contradicts `spec.md` or the current code is wrong —
  either align with it or call out the change you are proposing and why.

Then:

1. Write `docs/design/<slug>.md` — pick a short kebab-case `<slug>` from the
   brief. Argue the decision, don't just assert it: state the problem, lay out
   the options you weighed, give each option's tradeoffs **honestly** (including
   the one you rejected and why), and recommend a direction. Cite the spec
   sections and code paths you rely on — write repo paths in backticks (e.g.
   `crates/dispatcher/src/state.rs`) so they read as references. Link related
   docs with **relative** links.
2. **Open it with the header contract** — an `# ` title on line 1, then a
   `Status:` line carrying the **status and nothing else**: one short, complete
   phrase (`Status: PROPOSED.`), no markdown, ending in a period. The platform
   serves that line verbatim and truncates it at 120 characters, so a status
   that runs on into a sentence is displayed cut mid-word in the Designs view.
   The provenance preamble — *written against the tree at `<sha>`, every claim
   below verified against the source, not inferred from the docs* — belongs in
   the **paragraph after** it, along with anything else about the document.
   State that provenance: it is what makes the document's claims checkable
   later. When you amend a shipped design, say so in the status and name the
   jobs (`Status: IMPLEMENTED — shipped in jobs #295–#301.`). Full reference,
   including the vocabulary in use and how the document reaches the Designs
   view: [`docs/design-docs.md`](../../../docs/design-docs.md).
3. Keep the change to the design at hand — do not edit code or unrelated docs.
4. `.chug/tasks/doc-lint.sh` runs on your output: relative links must resolve and the
   markdown must be well-formed (closed code fences, spaced headings).
   Backtick'd code paths are checked best-effort — keep them accurate.
5. Commit to the current branch (you are already on the job branch) with a clear
   message, and push.
6. Narrate as you go with the `update_status` tool — it streams live to the
   operator. Call it at least three times: your one-line plan right after
   reading the brief (`update_status("plan: ...")`), after the draft is written,
   and just before you submit.
7. When done, call `submit_result` with:
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
   - `cover_html` (optional): a small self-contained HTML cover page — a diagram
     or visual summary — shown beside the summary in the UI. Only if it helps
     tell the story; never required, presentational only, never a substitute for
     `summary`. Keep it compact (rejected over 64KB).
8. Exit 0.

A reviewer will judge your document against the same brief — does it address the
brief, are the alternatives and tradeoffs honest, is it consistent with `spec.md`
and the codebase as they exist. If it finds problems you will be re-invoked with
its findings.
