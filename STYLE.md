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
file**: `.chug/jobs/_defaults.yaml` appends the `ci` command evaluator to every job
type, and `.chug/tasks/ci.sh` runs fmt, clippy `-D warnings`, and the full workspace
test suite on every merge (see CLAUDE.md "CI — the evaluation gates ARE the
CI"). Each item below is tagged **live** (the gate already enforces it) or
**pending** (needs config or a script the gate doesn't run yet; until then
reviewer-enforced by hand). "Non-negotiable" is about the rule, not the
machine: a violation blocks the change whether or not a linter caught it, and
several of these guard state that today's code still violates (noted inline) —
that debt is what the pending checks will pin down.

- **Dependency-graph invariants.** *(enforced:
  `crates/test-utils/tests/boundary_guard.rs` over `cargo metadata`, riding
  the workspace test run in `.chug/tasks/ci.sh` — refactor-plan A3.)* Only `store`
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
  already had wear a site-specific `#[allow]` whose `reason = "TODO(…)"` names
  the Track C ticket that dissolves them, so existing debt is greppable
  (`git grep 'clippy::too_many_lines'`, `git grep 'TODO(style)'`) while new code
  cannot add a violation without an explicit, reviewable allow. The marker rides
  the attribute rather than a comment above it — job #342 deleted every non-doc
  comment in the tree. Crate-level `#![allow]` of these
  three lints defeats the ratchet and is rejected on sight.

- **Formatting is `rustfmt` / `prettier` defaults.** *(live for Rust:
  `.chug/tasks/ci.sh` runs `cargo fmt --all -- --check` on every merge, and
  `.githooks/pre-commit` formats staged files and re-stages them before the
  commit — formatting is fixed, never reported. Pending for
  web: `web/` has `prettier` and an `npm run format:check` script as of A4,
  and the hook runs it on staged `web/` files when an install is present, but
  no gate enforces it — the wiring and the one-time reformat of the 55
  files that fail it belong to refactor-plan E1.)* *Why:* zero decisions, zero
  diffs about decisions.

- **No comments except doc comments; a doc comment is at most 2 sentences.**
  *(live: `.chug/tasks/check-comments.sh` runs from `.chug/tasks/ci.sh`,
  unconditionally and before the Rust early-exit, over the Rust and TypeScript
  sources in the diff — and from `.githooks/pre-commit` in `--staged` mode, so a
  comment is caught at the commit rather than a rework cycle later.)* `//`, `/* */` and every trailing-on-a-code-line form
  are rejected; `///`, `//!`, `/** */`, `/*! */` are the only prose a source
  file may carry, and each block stays inside two sentences. Longer than that
  is a doc: write it under `docs/` (or in the module's `MODULES.md` row) and
  leave a pointer. *Why:* comments are scattered by construction — nobody
  reviews them as a body of knowledge, they drift out of step with the code
  they annotate, and an agent reading the tree cannot tell a current one from a
  stale one. Docs are intentional and organized: one place to look, one place
  to update, and a job type that maintains them. Every comment this rule
  rejects is a sentence that belongs in a doc.

  Two carve-outs, both narrow. **Inner doc comments** (`//!`, `/*! */`, and a
  TypeScript file's first doc block) are exempt from the sentence cap: the
  module header — accepts / emits / guarantees / spec § — is the in-tree doc
  surface NORTH-STAR §4 asks for, registered in `MODULES.md` and structurally
  unable to scatter. **Machine-read directives** are not prose and are allowed:
  `jscpd:ignore-start`/`-end`, `SAFETY:`, and the eslint/ts/prettier pragmas —
  put the justification on the directive line itself. The allowlist matches the
  text right after the opener, so a directive is **one line**: a wrapped second
  line is an ordinary comment and the gate rejects it. Write the justification
  so it fits, however long that line runs.

  **Rule 1 is absolute; rule 2 is still a ratchet.** Job #342 deleted every
  non-doc comment in the tree — the rationale worth keeping was hoisted into
  [`docs/implementation-notes.md`](docs/implementation-notes.md) — so the gate
  lints every tracked Rust/TypeScript source and one non-doc comment anywhere
  fails it, changed file or not. The two-sentence cap still has pre-existing
  debt (~500 over-long doc comments), so it judges only blocks a diff adds a
  line inside: edit a doc comment and you trim it.

- **No duplicated code: zero clones.** *(live: `.chug/tasks/check-duplication.sh`
  runs `jscpd@5.0.5` — pinned exactly — over the whole repo from `.chug/tasks/ci.sh`,
  unconditionally and before the Rust early-exit, at `threshold: 0`. Config:
  `.jscpd.json`, 10 lines / 80 tokens; integration tests, vendored and
  generated trees excluded — ticket A5.)* Any clone fails the gate; extract the
  shared body into a helper named after its caller (Tier 2 rule 4) rather than
  raise the bar. A deliberate exception is a `jscpd:ignore-start` /
  `jscpd:ignore-end` bracket **whose directive line says why** (the comment
  rule above allows the directive, not a paragraph beside it) — never a
  threshold change. *Why:* duplicated logic drifts apart, and the copy that didn't get
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

5. **Commit messages carry the why; docs carry the knowledge.** The commit
   message explains why the change is shaped the way it is — PR descriptions
   and chat transcripts don't persist, `git blame` does. A constraint the code
   cannot express goes in a doc comment (two sentences, Tier 1) or the doc that
   doc comment points at — never in a comment, and never as narration of the
   next line. *Why:* six months out, the rationale is the only part that can't
   be re-derived from the diff, and the knowledge is only findable if it lives
   somewhere a reader thinks to look.

   The other half of the same rule is the **doc-update task**
   (`.chug/tasks/docs-update.md`, referenced from the `code` and `web` work
   prompts): a change updates the docs it makes stale, in the same commit.
   Its evaluation-phase counterpart (`.chug/tasks/review-docs-updated.md`) is
   wired but deliberately inert until the project decides how docs are managed.

   **A doc that says a gate, tier, fixture or command exists is making a factual
   claim about the tree — check it, or mark it as intent.** Present-tense prose
   about machinery is trusted and acted on, so a stale claim is worse than
   silence: it sends the next author to build against something that is not
   there, and lets a reviewer accept it as an answer. Write what the tree does,
   date the measurement, and mark anything unbuilt in the heading rather than
   describing it as if it ran.

   **Marking is a syntax, and it is checked.** A backticked path in a `.md` is
   resolved against `git ls-files` by `.chug/tasks/check-doc-facts.sh` — every
   job, every tracked `*.md`, and an **error** since design
   [#415](docs/design/415-knowledge-architecture.md) S1b. The same script checks
   a backticked constant asserted with a value against the tree's `pub const`.
   Three HTML comments suppress both on the line that carries them, and they are
   not interchangeable:

   | Marker | Means | Use for |
   | --- | --- | --- |
   | `<!-- intent -->` | designed, not built | a path a decision proposes and no commit has created |
   | `<!-- runtime -->` | correctly absent from git | build output, operator-owned files, anything a `.gitignore` excludes on purpose |
   | `<!-- absent -->` | named *because* it does not exist | a stale-path measurement, a rejected alternative, a recorded deletion — the line's own point is the absence |

   The three are ordered by tense: `intent` is a path that should exist later,
   `runtime` is one that exists on a real machine but not in git, and `absent`
   is one that exists nowhere and the sentence says so. `absent` is the
   narrowest: it is honest only when a reader who deleted the marker would still
   read the line as asserting the path is gone. Writing it on "see
   `crates/foo/bar.rs`" is self-evidently false, which is the property that <!-- absent -->
   keeps it from becoming a general silencer.

   A path that resolves in **another repo** takes no marker: qualify it instead
   (`kasofsk/beacon:infra/gcp-workload-id/`), or write the per-project config
   slot in its generic form (`.chug/tags/{tag}.md`). A bare path implies this
   tree, so the rewrite fixes the prose for a human reader and not just for the
   checker.

   The **restated constant** is judged the same way: a backticked
   `SCREAMING_SNAKE_CASE` name that resolves to a `pub const` in the tree and is
   asserted with a value on the same line must match it. A mention carrying no
   value is not a claim and is not checked, and `<!-- intent -->` marks the value
   a slice will bump it *to*. Inside an append-only body the cheapest honest fix
   is the tense — a past-tense or dated statement (`was 2 when this landed`) is
   not a claim about today's tree and is not checked.

   A marker covers **the line that carries it**, so put it at the end of the
   line making the claim; a claim on the next line is judged on its own. No
   marker is a way to silence a path that is simply stale — that is an edit, not
   a marker. An append-only design body is no exemption: it cannot be rewritten,
   but it can be annotated, so the sentence keeps the path and says what happened
   to it. *Why:* one week produced five — a `.github/`
   workflow mirror that did not exist and a `tier-2 ENABLED` announcement over a
   tier that self-skipped (#375, #378/#382), 17 shell suites nothing executed
   (#385), a duplication gate analysing no `.nix` files (#383), `check-modules.sh`
   verifying row presence but never content (#382), and `testing.md`'s tier 3,
   whose fixtures went out with the v1 tree and whose `chuggernaut seed` command
   v2 never had (#394).

6. **New behavior lands with a regression test at the lowest tier that can
   express it** (`testing.md`); the correctness core (`chuggernaut_domain::state`,
   release validation) stays at near-total branch coverage. *Why:* the lower
   the tier, the more often the test actually runs.

7. **Re-derive every host fact inside the namespace that will use it.** The
   worker daemon is itself a container (`deploy/prod/build-worker.sh`), so a
   path, device or socket the host has is absent to `chug-worker` unless it is
   mounted in — and **existence, identity and provenance are three separate
   questions**: a check that answers one does not answer the others. Ask all
   three of the view that will actually run the code: *is it there, is it the
   thing it claims to be, and did it get there by a route that survives?* A
   `create_dir_all` or a `stat` on the daemon's side is a statement about the
   container, never about the node — provision host state from the deploy
   script, and check it from the daemon in the daemon's own view. *Why:* this
   one root cause produced a rework cycle in job #374 (a boot-time `/dev/kvm`
   check that reads the daemon container's view, so enabling KVM also means
   passing the device into `chug-worker`), in #379/#380 (a `create_dir_all` of
   `WORKER_CACHE_DIR` that lands in the daemon's writable layer and never on the
   host, which is why `worker-refresh.sh` deliberately does not `mkdir` it), and
   in all three of job #384's (a realise target mounted nowhere — *existence*; a
   leaf bind that resolved the operator's symlink away, so the path existed but
   was not a store path — *identity*; fixed by binding the parent and asserting
   the canonicalized target lands under the store — *provenance*).
   Design [#373](docs/design/373-project-toolchains.md) is the long record.

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
