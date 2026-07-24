# Chuggernaut — style & blessed practices

The rules that make agent-driven work on this codebase reliably high-quality.
Companion to `NORTH-STAR.md` (target factoring) and `crates.md` (the map);
adapted from TigerBeetle's TIGER_STYLE where its guidance transfers to a
Rust/TypeScript orchestrator. The performance-oriented rules there (static
allocation, no recursion, hot-loop extraction) deliberately do **not**
transfer: Chuggernaut is I/O-bound orchestration that chooses simplicity over
performance.

The doc is three tiers, strictest first. Rules are numeric and mechanical
wherever possible, because vague rules erode and numbers don't — and each
rule carries its why inline, because the why is what lets a worker or
reviewer generalize correctly to cases the rule didn't anticipate. Keep this
document short: it is written to be injectable into Work and Evaluation
prompts, and reviewers should reject violations of Tier 1 and Tier 2 by
naming the rule.

## Tier 1 — machine-checkable invariants, non-negotiable

These are the invariants that must never be violated and that NORTH-STAR §4
targets for machine enforcement, so that a worker cannot drift on them even
without reading this file. **Status today:** the repo has no CI wired yet — no
`.github/`, no `clippy.toml`/lints table, no ESLint config — so every item
below is currently **reviewer-enforced by hand**, and each notes the check that
will make it automatic. "Non-negotiable" is about the rule, not the machine: a
violation blocks the change whether or not a linter caught it, and several of
these guard state that today's code still violates (noted inline) — that debt
is what the checks will pin down.

- **Dependency-graph invariants.** *(pending: `cargo metadata` check; today
  reviewer-checked — the tree currently satisfies it.)* Only `store` depends
  on `async-nats`; `api` never depends on `dispatcher` outside dev-deps;
  `types` has no async runtime or I/O dependencies. *Why:* one NATS
  integration point and a pure data crate are what keep every other boundary
  in the workspace meaningful.

  The check itself is a ~20-line script over `cargo metadata`: run
  `cargo metadata --format-version 1`, walk `resolve.nodes` (the resolved
  workspace graph), and fail the build if (a) any workspace crate other than
  `store` lists `async-nats` among its dependencies, (b) `dispatcher` appears
  in `api`'s non-dev dependency edges, or (c) `tokio`/`async-nats` appear
  anywhere in `types`' subtree. No new tooling — jq or a tiny Rust/Python
  script; it would run in the normal CI job before tests.

- **Web import boundaries (ESLint).** *(pending: presupposes the `ui/`/`data/`
  split, which is NORTH-STAR §3 target work — `web/src/` is still flat, so
  there is nothing to lint yet.)* Once the split lands, `ui/` never imports
  from `data/` or `api/`, and only `data/` imports `api/`. *Why:* this is what
  makes `ui/` presentational-by-construction and keeps fetching in one
  auditable layer (NORTH-STAR §3).

- **Function length: 70 lines.** *(pending: `clippy.toml` with
  `too-many-lines-threshold = 70` plus a deny of the allow-by-default
  `clippy::too_many_lines` lint; today reviewer-checked, and much of `eval.rs`
  exceeds it.)* *Why:* a function that fits on one screen can be reviewed as a
  unit; `eval.rs` reached ~2,900 lines precisely because no numeric limit
  existed.

- **No `.unwrap()` / `.expect()` outside tests.** *(pending: deny
  `clippy::unwrap_used`/`expect_used`; today reviewer-checked, and non-test
  domain code still contains such calls to be cleaned up.)* *Why:* the large
  majority of catastrophic distributed-system failures trace to mishandled
  errors; in the dispatcher a panic stalls every job in the DAG.

- **Formatting is `rustfmt` / `prettier` defaults.** *(pending: `cargo fmt
  --check` / `prettier --check` in CI; run locally today.)* *Why:* zero
  decisions, zero diffs about decisions.

## Tier 2 — mechanical rules a reviewer checks by name

Not (yet) machine-checked, but each is concrete enough that a reviewer can
verify it in seconds and must name it when rejecting.

1. **Deciders return effects; they never perform them.** `state.rs` is
   already pure — **zero `.await`s**. As lifecycle decision logic is carved out
   of `eval.rs`/`exec.rs` into `domain::decide::*` (the NORTH-STAR §1 target),
   each decider stays pure and returns `Vec<Effect>` for one interpreter to
   execute — it never performs an effect. *Why:* pure decision logic is
   exhaustively testable at tier 1 of `testing.md`, and the decider/effects
   seam is the whole point of the north-star factoring. (`Effect` and
   `domain::decide` do not exist yet — this is the shape new decision code must
   take, not a description of the tree today.)

2. **Assert liberally in domain code — arguments, postconditions, and
   invariants.** Aim for TigerStyle's density (on average, two assertions per
   function) in `state.rs`, release validation, and the deciders. Two
   specific patterns: **pair assertions across the NATS boundary** (assert
   the invariant before the dispatcher writes a job record and re-assert it
   on read-back), and **assert negative space** — what must never happen,
   e.g. no transition out of `Done`, never a second writer of job state.
   *Why:* assertions catch the bug at the moment of corruption instead of
   three subsystems later, and negative-space checks are what catch the
   transitions nobody meant to add.

3. **Everything is bounded.** Every loop has an iteration cap, every queue a
   depth limit, every wait a timeout; on hitting a bound, fail fast and
   surface it — never spin, never grow silently. *Why:* a stuck dispatcher
   loop stalls the entire job DAG; bounded-and-loud beats
   unbounded-and-quiet in an orchestrator more than anywhere else.

4. **Naming.** Units and qualifiers are suffixes in descending significance —
   `timeout_secs_max`, not `max_timeout_secs` (related names then align and
   sort together). No abbreviations in identifiers. A helper is prefixed with
   its caller's name (`evaluate_release` → `evaluate_release_criteria`), so
   the call tree reads from the names alone. *Why:* agent-written code is
   navigated by grep; predictable names are the index.

5. **Commit messages carry the why; comments are prose.** The commit message
   explains why the change is shaped the way it is — PR descriptions and chat
   transcripts don't persist, `git blame` does. Comments state constraints
   the code cannot express ("why"), written as sentences; never narration of
   the next line or where a change came from. *Why:* six months out, the
   rationale is the only part that can't be re-derived from the diff.

6. **New behavior lands with a regression test at the lowest tier that can
   express it** (`testing.md`); the correctness core (`dispatcher::state`,
   release validation) stays at near-total branch coverage. *Why:* the lower
   the tier, the more often the test actually runs.

## Tier 3 — principles

- **Single writer.** The dispatcher is the only writer of job records,
  single-threaded by design. A change that seems to need a second writer or
  a lock is the wrong shape — simplify instead.
- **Simplicity over performance.** Simplicity is achieved through revision,
  not first drafts; "a simpler shape would do" is a legitimate review
  rejection, not gold-plating.
- **Zero technical debt.** Fix it at design time; a problem deferred into a
  running orchestrator costs an order of magnitude more than one caught in
  the ticket.
- **Dependencies need a stated justification.** Not zero-deps absolutism —
  but every new crate or npm package added must say in its commit message
  what it buys and why vendoring or writing it is worse.
