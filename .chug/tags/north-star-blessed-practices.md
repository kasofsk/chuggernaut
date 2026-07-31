# North-star blessed practices

Injected guidance for work on this repo. Full texts: `STYLE.md` (the rules
and their whys), `NORTH-STAR.md` (target factoring), `contracts.md`
(dispatcher interfaces), `testing.md` (test tiers). When this summary and
those docs disagree, the docs win — read them before large changes.

## Non-negotiable (Tier 1 — reviewers reject by rule name)

- Only `store` depends on `async-nats`; `api` never depends on `dispatcher`
  outside dev-deps; `types` stays sync (no async runtime, no I/O). Pure
  domain code (`state.rs`, deciders, the domain crate) has **zero `.await`**.
- Functions ≤ 70 lines. No `.unwrap()`/`.expect()` outside tests.
- `rustfmt`/`prettier` defaults; `cargo clippy -D warnings` clean.
- **No comments except doc comments, and a doc comment is ≤ 2 sentences**
  (`.chug/tasks/check-comments.sh`; the no-comment half covers the whole tree,
  the sentence cap only the blocks a diff touches).
  Module headers (`//!`, `/*! */`, a TS file's first block) are exempt from
  the cap. Knowledge goes in a doc, rationale in the commit message; update
  the docs your change stales — `.chug/tasks/docs-update.md`.
- `.githooks/pre-commit` enforces the fast half at **your** commit: it formats
  and re-stages your staged files, and rejects a non-doc comment, a `MODULES.md`
  registry gap or a duplicated block. Fix what it names rather than working
  around it; `git commit --no-verify` is the escape hatch when the alternative
  is leaving work uncommitted, and it belongs in your summary when used.

## Mechanical rules (Tier 2)

1. **Deciders return effects; they never perform them.** New decision logic
   is a pure function `(view of state, event) → (transitions, Vec<Effect>)`;
   one interpreter executes effects through the ports. Never add I/O to
   decision code.
2. **Assert liberally in domain code** — arguments, postconditions,
   invariants; ~two per function. Assert negative space (no transition out
   of terminal states; never a second writer of job state).
3. **Everything is bounded** — every loop an iteration cap, every queue a
   depth limit, every wait a timeout; on hitting a bound fail fast and loud.
4. **Naming:** units/qualifiers as suffixes in descending significance
   (`timeout_secs_max`); no abbreviations; helpers prefixed with their
   caller's name so the call tree reads from names alone.
5. **Commit messages carry the why; docs carry the knowledge.** A constraint
   the code can't express goes in a doc comment (≤ 2 sentences) or the doc it
   points at — never a comment, never narration of the next line.
6. **New behavior lands with a regression test at the lowest tier that can
   express it**; `dispatcher::state` and release validation stay at
   near-total branch coverage.

## Principles (Tier 3)

- **Single writer.** The dispatcher is the only writer of job records,
  single-threaded by design. Needing a lock or second writer means the
  design is the wrong shape — simplify.
- **Simplicity over performance.** "A simpler shape would do" is a
  legitimate review rejection.
- **Zero deferred debt.** Fix it at design time.
- **New dependencies state their justification** in the commit message.

## Contract-first change rule (dispatcher jobs)

Any change to the dispatcher names the contract it changes — a `Msg`
pre/postcondition, an `Effect`, an invariant, a golden trace. If it can't be
expressed that way, the missing contract is the first commit of the job.

## Direction of travel

Every touch moves code toward the north star, never sideways: decision logic
carved out of `eval.rs`/`exec.rs` goes into pure deciders, not new `impl
Core` methods; new modules get a doc header (accepts / emits / guarantees /
spec §) and a `MODULES.md` registry row; web fetching goes through the
`data/` layer once it exists. Do not add to the blobs the north star is
shrinking.
