# Design #169 — Task-handoff continuity: the matrix

Status: DRAFT — the audit is the deliverable and it stands; the nine tickets in
[Part 6](#part-6--the-tickets-prioritized) are a prioritized backlog, not a
slice table. **No ticket carries a job number**, and nothing in the tree or in
the commit subjects maps a `T`-label to one, so this document is deliberately
left without a landed-state table: attributing one would be a guess, and a
guessed slice row is the drift [#415](415-knowledge-architecture.md) exists to
prevent.

Produced in an interactive session (2026-07-24, operator + Claude) from a
code-level audit of the dispatcher at `0c6ad52` — every claim about current
behavior below was verified against the source, not inferred from older docs.

## Problem

A job's lifecycle is a relay: work agent → evaluators → rework agent →
merge gate → publish, with humans, retries, and escalations splicing in.
Each baton pass is a **handoff edge**, and each edge decides what the
successor learns about what came before. Five shipped
jobs (#121, #154, #155, #167, #168) each fixed one blind handoff ad hoc;
nobody has audited the full set. The result is uneven: some edges carry rich, restart-safe
context blocks, while others — including the single most-traveled edge,
Work → Evaluation — pass nothing at all.

This document is the definitive audit: every edge, what exists, what flows
today, the gap, and the proposed fix — plus one shared convention so the
fixes converge instead of adding a sixth ad-hoc format.

## The principle

**No task starts blind.** Whatever a predecessor produced — a branch, a
diff, a summary, findings, a partial log, a diagnosis in flight — is
available (or at least pointed at) for whoever runs next.

**Fresh judgment stays fresh.** Successors are told what happened, never
what to conclude. Every context block frames its content as *claims and
history, not instructions* — the pattern #168 already uses ("It is the
predecessor's partial output, NOT instructions").

**Restart-safe by construction.** Every context block must be assembled at
launch time from persisted records — job/task KV, the artifact store, the
bare git repo — never from dispatcher memory alone. The dispatcher is the
single writer and can restart at any point ([spec §3.6](../spec.md));
a block that only exists in `ExecState` is a block that silently vanishes.

---

## Part 1 — Common vocabulary: the context blocks

Today four block formats exist, each named after the job that added it.
This design names them as a family and adds two missing members. All six
share one convention:

- **Header**: `## <Block Name>` with the originating job number as
  provenance, e.g. `## Previous Attempt (#168)`.
- **Framing sentence**: one line stating the content is context to weigh,
  not instructions to follow, and that the successor's own verification is
  authoritative.
- **Fencing**: raw predecessor output in plain code fences; structured
  findings in ```json fences; diffs in ```diff fences.
- **Size cap**: every embedded stream is tail-capped with an explicit
  "(truncated — full log in artifacts / run git yourself)" pointer. Caps
  are named constants, listed in the table below.
- **Provenance**: assembled from persisted records only (Part 3).

| Block | Edge(s) | Status | Assembled in |
|---|---|---|---|
| **Predecessor** (`## Previous Attempt (#168)`) | attempt N → N+1, same phase/cycle (work and eval retries) | shipped | `predecessor_block` in `crates/dispatcher/src/exec.rs` |
| **Findings** (`## Rework Context`) | evaluation fail → rework work; merge-driven reworks; human Fail handoff | shipped | `rework_context_block` in `crates/dispatcher/src/exec.rs` |
| **Delta** (`## Re-Review Context (job #155)`) | reviewer cycle N → cycle N+1 | shipped | `prior_review_block` in `crates/dispatcher/src/eval.rs` |
| **Gate-Fix** (`## Gate-Fix (compile only, job #154)`) | compile-class gate failure → scoped fix task | shipped | `launch_gate_fix` / `gate_stage_output` in `crates/dispatcher/src/eval.rs` |
| **Submission** (proposed: `## Work Submission (#169)`) | work → evaluation | **new** | ticket T1 |
| **Upstream** (proposed: `## Upstream Jobs (#169)`) | dependency Done → dependent's work brief | **new** | ticket T5 |

The human-origin flavors — the #121 operator-handoff entry and the proposed
escalation-resolution entry (T3) — render *inside* the Findings block as
labeled results (`**operator handoff (name)**`) rather than as a separate
block: to the successor, "an evaluator failed you" and "an operator failed
you" are the same shape of information.

### Size-cap policy (current + proposed)

| Constant | Value | Applies to |
|---|---|---|
| `PREDECESSOR_TAIL_BYTES` | 12 000 | Predecessor block stdout tail |
| `GATE_LOG_TAIL_BYTES` | 8 000 | command-evaluator / gate output tails |
| `DELTA_DIFF_MAX_BYTES` | 24 576 | Delta block diff |
| `STDOUT_TAIL_BYTES` (triage) | 4 000 | per-task tails in the triage prompt |
| history-digest summary line | 120 chars | Delta block digest |
| structured findings JSON | **uncapped — gap** | Findings block (T7: cap at 16 KiB per evaluator, truncate with pointer) |
| Submission block (proposed) | 8 KiB summary + 8 KiB structured | T1 |
| Upstream block (proposed) | 4 KiB per dependency | T5 |

---

## Part 2 — The continuity matrix

Edges enumerated exhaustively from the state machine
(`crates/domain/src/state.rs` — `crates/dispatcher/src/state.rs` when this was <!-- absent -->
written; the module moved to the domain crate under refactor-plan C1 and the
dispatcher re-exports it, [spec §2.1](../spec.md)) plus the
intra-phase retry/claim/escalation seams. Summary first, detail after.

| # | Edge | Passed today | Gap severity |
|---|---|---|---|
| E1 | Work → Evaluation (cycle 1) | job brief only | **high** — T1 |
| E2 | Evaluation fail → rework work | Findings block (all results, tails) | low — T7 cap only |
| E3 | Work retry, same cycle | Predecessor block | none — ratified |
| E4 | Evaluator retry, same round | Predecessor block | none — ratified |
| E5 | Re-review, cycle N > 1 | Delta block | low — comment drift, T9 |
| E6 | Gate compile-fail → gate-fix | Gate-Fix block (stderr from task records) | none — ratified |
| E7 | Gate test-fail / conflict → full rework | Findings (no output) + conflict context | **high** — T2 |
| E8 | Escalation → human → resumed task | nothing | **high** — T3 |
| E9 | Human Fail-with-notes → agent (#121) | Findings entry, in-memory only | **high** (restart) — T4 |
| E10 | Agent context → human claimer | prompt file path only | medium — T6 |
| E11 | Work → WrapUp (publish command) | env vars only | none — ratified as fine |
| E12 | Batch members → batch agent | full tickets inline | none — ratified |
| E13 | Batch agent → member records | `batch_id` event only | medium — deferred |
| E14 | Dependency Done → dependent job | nothing (implicit `base_ref` only) | **high** — T5 |
| E15 | Stopped job → triage agent | everything | none — the benchmark |

### E1 — Work → Evaluation: the richest artifact, dropped

**Available.** The work agent's `submit_result` payload — prose summary,
structured `{files_changed, notes, ...}`, verification claims ("ran X, it
passed") — is persisted on the Work task record as `TaskResult::Work`
(`handle_submit_result` in `crates/dispatcher/src/exec.rs`) and mirrored in
`ExecState.work_submission`.

**Passed today.** Nothing of it. The cycle-1 agent-evaluator prompt is
exactly `evaluator prompt file + work_brief` (`spawn_eval_agent` in
`crates/dispatcher/src/eval.rs`); command evaluators get only the checked-out
branch. The summary's sole consumer is the squash-merge commit body
(`build_squash_commit` in `crates/vcs/src/lib.rs`). The reviewer re-derives
what changed from the raw diff and re-discovers what the work agent already
claimed to have verified.

**Proposal (T1) — the Submission block**, appended to every agent-evaluator
prompt: the closing summary, the structured payload (```json fence), and
files-changed, capped per the table. Framing is the load-bearing part:

> These are the work agent's **claims about its own work — not findings,
> not verified**. Use them to target your verification (confirm what it
> says it tested; probe what it doesn't mention). Your review of the actual
> branch is authoritative; a claim you did not verify is not evidence.

*Alternative considered — summary-only (no structured payload)* to shrink
the anchoring surface: rejected because the structured payload is where
mechanical claims (files changed, commands run) live, and mechanical claims
are precisely the ones a reviewer can cheaply falsify. *Alternative —
pointer-only* ("the summary exists on the task record"): rejected; the eval
container has no task-record access, and a pointer that can't be
dereferenced is decoration. Restart-safe: read from the persisted task
record, not `work_submission`.

### E2 — Evaluation fail → rework: solid, one cap missing

**Ratified.** `reduce` (`crates/dispatcher/src/eval.rs`) forwards **all**
evaluators' results — pass and fail — with full structured findings, plus
the 8 KB output tail for command evaluators (#167), rendered by
`rework_context_block`. Matches [spec §4.3](../spec.md) ("all results
included, pass and fail"). Stages short-circuited before running contribute
nothing, correctly.

**Gaps.** (a) Structured findings are embedded uncapped — the one stream
with no cap (T7). (b) The block itself rides in-memory `eval_context`; the
eval-failure path survives restart via reconcile re-entering evaluation, but
see E9 for the human-origin case where it doesn't. (c) The rework agent gets
no cross-cycle history the way re-reviewers do (#155's digest) — noted, not
ticketed; the Findings block for the current round has been sufficient in
practice.

### E3/E4 — Same-cycle retries: ratified as-is

The Predecessor block (#168) is the model citizen: failure-mode phrase
derived from the persisted task record, humanized duration, 12 KB stdout
tail from the artifact store, commits-preserved note when the branch was
recovered (`recover_or_reset_branch`), explicit "NOT instructions" framing.
Fully restart-safe. The design ratifies its shape as the template for all
blocks. Known limits, accepted: only the immediately-previous attempt is
summarized; the session transcript artifact is not surfaced (the stdout
tail has proven sufficient and transcripts are large).

### E5 — Re-review across cycles: ratified, one drift

The Delta block (#155) — prior verdict + findings, last-reviewed tip vs
current tip, ancestry-checked delta diff (rebase-aware: falls back to the
workspace full diff when `rework_reason` indicates a rebase), and the
job-history digest — is assembled entirely from task records + git and
documented at [spec §3.3](../spec.md). Ratified.

**Drift (T9).** Comments in `crates/dispatcher/src/eval.rs` claim command
evaluators' output tails feed "#155's re-review", but `prior_review_block`
returns `None` for any non-agent prior — a command evaluator's evidence
never reaches a re-review prompt. Command evaluators are deterministic
(exit code is the verdict), so re-review context for them is genuinely
useless; the fix is to correct the comments, not wire the data.

### E6/E7 — Merge-gate failures: the asymmetry

**Gate-fix (E6, ratified).** The compile-class fast path (#154) is
restart-safe by design: `gate_stage_output` re-reads the failing stage's
stderr from the **persisted** `TaskResult::Command`, and the Gate-Fix
framing scopes the task hard ("minimal change to restore compilation").
The candidate SHA is not quoted in the brief — acceptable, since
`job.base_ref` is repointed and the agent works on the rebased branch.

**Full rework (E7, gap — T2).** Test-class failures (and compile failures
past `GATE_FIX_BUDGET`) take the full-rework path, where `gate_reduce`
deliberately builds its `EvalResult`s with `output: None` — and nothing
re-reads the task record the way `gate_stage_output` does. Net effect: the
**worse** gate failure hands the agent **less** evidence — a Findings block
with no gate output, plus rebase/conflict context. The agent is told the
gate failed but not what the failing test printed.

**Proposal (T2).** Reuse `gate_stage_output` on the full-rework path: embed
the failed stage(s)' persisted output in the Findings block exactly as the
gate-fix brief does (same 8 KB capture cap). Symmetry, not new machinery.

### E8 — Escalation → human → resumed task: dropped on the floor

**Available.** Two things are persisted: the escalation itself
(`Escalation { reason, detail, failing_task }` on the job record — and for
eval-abort escalations `detail` embeds the aborting evaluators' findings),
and the operator's resolution (`TaskResolution::Escalation { action,
structured }`, persisted onto `TaskResult::Human`).

**Passed today.** Neither reaches the resumed task. `escalation_retry` /
`enter_evaluation` rebuild `ExecState` via `ensure_exec_state`, which sets
`eval_context: vec![]` — so a Retry-resumed work attempt gets the
Predecessor block and nothing else; the operator's notes ("I bumped the
lockfile, try again") survive only for triage and the UI. The wire examples
in [spec §1.2](../spec.md) show operators writing
`structured: {"notes": "fixed manually"}` — content the platform stores and
then never uses in execution.

**Proposal (T3).** On escalation Retry (and Resolve→evaluation), inject a
Findings-block entry labeled `**operator resolution (name)**` carrying the
operator's structured payload, plus one line of the escalation's own
`reason`/`detail` so the resumed task knows *why* it stopped. Assembled at
launch from the resolved task record + job `Escalation` field — never from
memory. Same rendering path as #121's operator-handoff entry; no new format.

### E9 — #121 Fail-with-notes: right block, wrong provenance

**Works today** (`handle_resolve_task`, Work-phase Fail arm): the operator's
notes enter `eval_context` as `**operator handoff (name)**`, the branch is
preserved, the next agent sees a proper Findings block.

**Two gaps (T4, T8).** (a) That `eval_context` push is in-memory only. A
dispatcher restart between resolve and the next launch rebuilds
`eval_context` empty — the handoff silently degrades to a bare retry. Fix:
`ensure_exec_state` (or the launch path) must reconstruct operator-origin
entries from the persisted `TaskResult::Human` of the failed attempt, the
same move the Delta and Gate-Fix blocks already made. (b) When the Fail
exhausts `work_retries`, the escalation is raised with a generic
`work_retries_exhausted` detail and the operator's notes are dropped from
it — the next human sees less than the previous human wrote (T8).

### E10 — Human claimers see less than the agent they replace

A claimed agent-typed attempt parks with `kind: Agent { prompt: <file
path> }` (`launch_work_task` park path in `crates/dispatcher/src/exec.rs`)
— the assembled brief, Findings block, and Predecessor block are never
built. A human doing rework cannot see the evaluator findings the agent
would have received without spelunking the task log. Declared-human work
tasks, by contrast, do get `prompt file + brief` — the claimer of an
agent task gets *less* than either an agent or a declared human.

**Proposal (T6).** At park time, run the same `build_prompt` assembly an
agent launch would and store the result on the parked task's prompt field.
Human and agent see identical context; zero new formats. *Alternative — an
on-demand API that assembles the would-be prompt*: rejected as a second
assembly path that can drift from the real one; parking the real prompt is
one code path and restart-safe for free (it's on the task record).

### E11 — WrapUp: ratified as intentionally lean

There is no agent wrap-up flavor — `WrapUpMode` is `merge | none` and the
optional `wrap_up.run` hook is command-only (`crates/types/src/job_type.rs`).
The publish command gets env vars (`JOB_ID`, branch, repo) and the merged
default branch; it neither needs nor wants the work summary. The work
summary does land in its natural home — the squash commit body
(`build_squash_commit` in `crates/vcs/src/lib.rs`), including the operator's
summary for human-claimed work. **No change.** If an agent wrap-up flavor
is ever added, it must receive the Submission block and the evaluation
verdicts; recording that requirement here is the cheap insurance.

### E12/E13 — Batch edges: forward fine, reverse deferred

Forward (E12): `batch_brief_block` inlines every member's full title +
description with per-ticket headers and a "your closing summary must cover
each by number" contract. Uncapped, deliberately — tickets are the work
itself, not context. Ratified.

Reverse (E13): when the batch lands, members flip Batched→Done carrying
only a `job-completed-via-batch` event with the `batch_id`; the batch
agent's per-ticket accounting exists only inside the single squash-body
summary. Per-member summaries would require extending the batch submit
contract (structured keyed by member seq) and a dispatcher fan-out write.
**Deferred by decision** (operator call, this session): member Done state
plus the batch squash body is sufficient until someone actually needs
per-member reports; the matrix records the gap so the future ticket starts
from here.

### E14 — Dependency → dependent: the contract nobody wired

**Available.** When job A completes, its summary + structured result sit on
its Work task record; its delivery is the squash commit on the default
branch. [docs/reference/design-lifecycle.md](../reference/design-lifecycle.md) ("Job outputs: the
structured result is the contract") explicitly designates the structured
result as what downstream consumes.

**Passed today.** Nothing. `try_unblock` (`crates/dispatcher/src/core.rs`)
re-validates B, pins `base_ref` to post-merge HEAD, and enqueues. B's brief
does not mention A — a job depending on a design job is not even told the
doc path it is supposed to implement.

**Proposal (T5) — the Upstream block**, appended to the dependent's work
brief at launch: one entry per direct dependency — seq, title, closing
summary, structured result, merged commit SHA — capped per-dep, assembled
from A's task record + git at B's launch (restart-safe; nothing stored on
B). Framing: "what your upstream jobs delivered — verify in the tree, don't
assume." *Alternative — structured + SHA only, no prose*: rejected; the
summary is where a design job says "the doc is at docs/design/x.md, the key
decision is Y", which is exactly what the dependent needs. *Alternative —
only for design/docs dependencies*: rejected as a special case that would
grow a taxonomy; one uniform block, and code deps whose entire delivery is
"it's merged under you" simply have short entries.

### E15 — Triage: the benchmark

The triage prompt (`build_triage_prompt` in
`crates/dispatcher/src/forge_ingest/triage.rs`) assembles brief + escalation detail +
the full task log with per-task result renderings and 4 KB stdout tails —
including human resolutions with `action`, operator, and structured notes.
It is the existence proof that every datum this design proposes to forward
is already reachable from persisted records. The production edges above
should converge toward (a bounded subset of) what triage already does.

---

## Part 3 — Restart-safety: the rule and today's violations

**Rule (normative, from the brief):** every context block is assembled at
task-launch time from persisted records — job/task KV, the artifact store,
the bare repo. `ExecState` may cache, never originate.

Stated as an invariant in [docs/reference/contracts.md](../reference/contracts.md)'s vocabulary,
so it can graduate from prose to a checkable statement: *a context block is
a pure function of persisted records — a dispatcher restart between
deciding a launch and performing it yields a byte-identical prompt.* Once
golden decision traces exist (docs/reference/contracts.md, "mining intent"), this is
pinned for free: the prompt rides the launch effect, so traces capture
every block verbatim.

Compliant today: Predecessor (#168), Delta (#155), Gate-Fix stderr (#154),
triage. Violations / at-risk:

1. **#121 operator handoff** — in-memory `eval_context` only (T4).
2. **Escalation resolutions** — never forwarded at all; the fix (T3) must
   read persisted records from day one.
3. **Eval-failure Findings** — in-memory, but reconcile re-runs evaluation
   after a restart mid-rework, regenerating the results; acceptable, and
   worth a regression test pinning that behavior
   ([docs/reference/testing.md](../reference/testing.md) tier 2).

New blocks (T1, T5) read task records + git by construction.

## Part 4 — Spec drift to fold in

Code is ahead of [spec](../spec.md) in three places; whichever ticket
lands first should carry the spec edits (T9 sweeps the remainder):

1. §4.3's `EvalResult` omits the `output` field (#167's evidence carrier).
2. §4.3's rework-context format documents neither the `## Previous Attempt
   (#168)` block nor the #167 command-output fencing.
3. The #155/#167 comment drift in `crates/dispatcher/src/eval.rs` (E5).

## Part 5 — Structural alignment (NORTH-STAR / contracts)

This design decides *what flows*; [docs/README.md](../README.md)
decides *where code sits*. Left unstated, the tickets below would each add
another fetch-and-format braid to `crates/dispatcher/src/exec.rs` /
`eval.rs` — the exact files the decider/effects extraction is trying to
shrink. So three placement rules bind every ticket in Part 6:

1. **Rendering is pure; fetching is not.** A context block is rendered by
   a pure function `(persisted values, caps) → String` — zero awaits,
   exhaustively unit-testable at tier 1 ([docs/reference/testing.md](../reference/testing.md)),
   exactly the "grow the pure core" move of NORTH-STAR §1. Fetching the
   inputs (task records, artifacts, git) stays at the call site today and
   moves to the interpreter as the decider extraction lands. No new block
   may interleave the two.
2. **One module, one scope.** The pure renderers live in a single
   `context` module (`dispatcher::context` — a `domain/` citizen in the
   target layout), giving these tickets a real scoping surface — "scoped
   to `dispatcher::context`" is a reviewable job; "scoped to `eval.rs`"
   is not (NORTH-STAR §1). The four shipped blocks migrate
   opportunistically as tickets touch them: T2 moves `gate_stage_output`'s
   rendering, T4 moves the #121 entry, rather than growing them in place.
3. **Name the contract** ([docs/reference/contracts.md](../reference/contracts.md), the
   contract-first change rule). Every ticket below changes the same two
   contracts: the **payload of the task-launch effect** (what prompt text
   accompanies `LaunchContainer`), and the **restart-safety invariant** of
   Part 3. T4 is the rule in miniature — replacing an in-memory
   `ExecState` read with a read-only view of persisted records is the
   layer-2 move of docs/reference/contracts.md's formalization ratchet, applied to one
   path.

Deliberately *not* imported from docs/reference/contracts.md: schema emission for block
formats. Blocks are prompt prose, not wire vocabulary — the invariant and
(eventually) golden traces are the right enforcement; a JSON Schema for
markdown text would be formalism without teeth.

## Part 6 — The tickets, prioritized

| # | Ticket | Edge | Size |
|---|---|---|---|
| T1 | **Submission block**: work agent's summary + structured + files-changed into every agent-evaluator prompt, claims-to-verify framing, read from the task record | E1 | M |
| T2 | **Gate evidence symmetry**: full-rework brief embeds failed gate stage output via `gate_stage_output`, as gate-fix already does | E7 | S |
| T3 | **Escalation resolution handoff**: operator's structured notes + escalation reason/detail into the resumed task's Findings block, from persisted records | E8 | M |
| T4 | **Restart-safe #121**: reconstruct operator-handoff `eval_context` entries from `TaskResult::Human` at launch; regression test = resolve-Fail, restart dispatcher, assert the block survives | E9 | S |
| T5 | **Upstream block**: per-dependency summary + structured + merged SHA in the dependent's brief, assembled at unblock-launch | E14 | M |
| T6 | **Claim parity**: park the fully-assembled prompt on the claimed task record | E10 | S |
| T7 | **Cap policy**: cap structured findings in the Findings block (16 KiB/evaluator, truncation pointer); add the caps table to spec §4.3 | E2 | S |
| T8 | **Exhausted-retries escalation carries the notes**: `work_retries_exhausted` escalation detail embeds the operator's Fail notes / last Findings entries | E9 | S |
| T9 | **Spec + comment sweep**: Part 4 items | — | S |

Deferred, recorded: batch reverse fan-out (E13); rework-agent history
digest (E2c); agent wrap-up inputs (E11 — requirement noted for if the
flavor ever exists).

Ordering rationale: T1 and T2 change what the two highest-traffic
successor prompts see and are pure adds; T3/T4 close the two paths where
a human writes context the platform then loses — the most corrosive
failure for operator trust; T5 unlocks design→code chains, which is how
this very document is meant to be consumed; the rest are hygiene.

## Amendments to already-filed jobs

- **#121** — amended by T4 (provenance) and T8 (exhaustion path); the
  block shape it introduced is ratified unchanged.
- **#154, #155, #168** — ratified unchanged; #154's `gate_stage_output`
  is promoted to shared machinery by T2.
- **#167** — ratified; its evidence carrier gets documented (T9) and
  capped company-wide (T7).
