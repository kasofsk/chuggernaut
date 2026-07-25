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
without reading this file. **CI here is the evaluation gate, not a workflow
file**: `jobs/_defaults.yaml` appends the `ci` command evaluator to every job
type, and `tasks/ci.sh` runs fmt, clippy `-D warnings`, and the full workspace
test suite on every merge (see CLAUDE.md "CI — the evaluation gates ARE the
CI"). Each item below is tagged **live** (the gate already enforces it) or
**pending** (needs config or a script the gate doesn't run yet; until then
reviewer-enforced by hand). "Non-negotiable" is about the rule, not the
machine: a violation blocks the change whether or not a linter caught it, and
several of these guard state that today's code still violates (noted inline) —
that debt is what the pending checks will pin down.

- **Dependency-graph invariants.** *(enforced:
  `crates/test-utils/tests/boundary_guard.rs` over `cargo metadata`, riding
  the workspace test run in `tasks/ci.sh` — refactor-plan A3.)* Only `store`
  depends on `async-nats`; `api` never depends on `dispatcher` outside
  dev-deps; `types` has no async runtime or I/O dependencies; and
  `chuggernaut-domain` — the pure core (refactor-plan C1) — resolves neither
  `tokio` nor `async-nats` (nor `store`/`vcs`/`auth`) anywhere in its
  subtree, with a companion zero-`.await` sweep over its sources. *Why:* one
  NATS integration point and pure data/domain crates are what keep every
  other boundary in the workspace meaningful — and a crate that cannot reach
  a runtime cannot drift into I/O.

- **Web import boundaries (ESLint).** *(pending: presupposes the `ui/`/`data/`
  split, which is NORTH-STAR §3 target work — `web/src/` is still flat, so
  there is nothing to lint yet.)* Once the split lands, `ui/` never imports
  from `data/` or `api/`, and only `data/` imports `api/`. *Why:* this is what
  makes `ui/` presentational-by-construction and keeps fetching in one
  auditable layer (NORTH-STAR §3).

- **Function length: 70 lines.** *(live: `clippy.toml` sets
  `too-many-lines-threshold = 70` and the root `Cargo.toml`'s
  `[workspace.lints.clippy]` denies `too_many_lines`, so the gate's clippy
  `-D warnings` fails on a 71-line function — refactor-plan A4.)* *Why:* a
  function that fits on one screen can be reviewed as a unit; `eval.rs`
  reached ~2,900 lines precisely because no numeric limit existed.

- **No `.unwrap()` / `.expect()` outside tests.** *(live: the same workspace
  lint table denies `unwrap_used` and `expect_used`; `#[cfg(test)]` modules
  and `tests/` targets carry the exemption as an explicit top-of-scope
  `#![allow]` — refactor-plan A4.)* *Why:* the large majority of catastrophic
  distributed-system failures trace to mishandled errors; in the dispatcher a
  panic stalls every job in the DAG.

  Both denies landed as a **ratchet, not a cleanup**: the violations the tree
  already had wear a site-specific `#[allow]` with a `TODO` naming the Track C
  ticket that dissolves them, so existing debt is greppable
  (`git grep 'clippy::too_many_lines'`) while new code cannot add a violation
  without an explicit, reviewable allow. Crate-level `#![allow]` of these
  three lints defeats the ratchet and is rejected on sight.

- **Formatting is `rustfmt` / `prettier` defaults.** *(live for Rust:
  `tasks/ci.sh` runs `cargo fmt --all -- --check` on every merge. Pending for
  web: `web/` has `prettier` and an `npm run format:check` script as of A4,
  but nothing runs it — the gate wiring and the one-time reformat of the 55
  files that fail it belong to refactor-plan E1.)* *Why:* zero decisions, zero
  diffs about decisions.

- **No duplicated code: zero clones.** *(live: `tasks/check-duplication.sh`
  runs `jscpd@5.0.5` — pinned exactly — over the whole repo from `tasks/ci.sh`,
  unconditionally and before the Rust early-exit, at `threshold: 0`. Config:
  `.jscpd.json`, 10 lines / 80 tokens; integration tests, vendored and
  generated trees excluded — ticket A5.)* Any clone fails the gate; extract the
  shared body into a helper named after its caller (Tier 2 rule 4) rather than
  raise the bar. A deliberate exception is a `jscpd:ignore-start` /
  `jscpd:ignore-end` bracket **with a comment saying why** — never a threshold
  change. *Why:* duplicated logic drifts apart, and the copy that didn't get
  the fix is where the next bug lives. Agent-written code duplicates far more
  readily than human-written code — an agent that cannot find the existing
  helper writes a second one — so a threshold set anywhere above zero would
  ratchet the wrong way.

## Tier 2 — mechanical rules a reviewer checks by name

Not (yet) machine-checked, but each is concrete enough that a reviewer can
verify it in seconds and must name it when rejecting.

1. **Deciders return effects; they never perform them.** `state.rs` is
   already pure — **zero `.await`s**. As lifecycle decision logic is carved out
   of `eval.rs`/`exec.rs` into `domain::decide::*` (the NORTH-STAR §1 target),
   each decider stays pure and returns `Vec<Effect>` for one interpreter to
   execute — it never performs an effect. *Why:* pure decision logic is
   exhaustively testable at tier 1 of `testing.md`, and the decider/effects
   seam is the whole point of the north-star factoring.
   (`chuggernaut_domain::{effects, decide}` exist as of B2/C1;
   `decide::escalation` is the worked template a new decider copies.)

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
