# Design #238 — forge-ingest stays in the dispatcher (for now)

Status: FINDING.

Written against the tree at `01624fd`, the parent of the C9 commit that moved
platform-ops out. Every claim below was read out of the source, not inferred
from the docs.

C9 asked for two context crates. One landed: `crates/platform-ops` holds
`fleet`/`cd`/`harvest`/`seed`, and `crates/dispatcher/src/platform_ops.rs` is
the adapter that lends it views off the single writer's fields. The other did
not, and this note is the "stop and file a design note instead of forcing it"
half of the ticket: **forge-ingest is not separable from the lifecycle loop
today**, the reason is specific and cheap to state, and the work that makes it
separable is already planned as Track H.

Related: [refactor-plan](215-refactor-plan.md) (Track H; the "no
_speculative_ crate splits" rule), [NORTH-STAR §1](../README.md),
[docs/reference/style.md](../reference/style.md) Tier 3 (single writer), [spec §1.2, §5.3, §3.3](../spec.md).

## The boundary condition

The refactor-plan's rule for graduating a module boundary to a crate has two
clauses: the boundary aligns with a north-star seam, **and** its interface no
longer needs `&mut Core`. forge-ingest passes the first and fails the second.
That is not an aesthetic judgement — it is a compile error waiting to happen.
A crate cannot hold an `impl Core` block for a `Core` defined in the crate that
depends on it, so a context that still reaches into the actor's fields has
nothing to move except its own name.

Platform-ops passed both clauses because the only mutable state it touched was
its *own* republish caches (`last_fleet_status`, `snapshot`), which the adapter
now lends it by value. forge-ingest touches the lifecycle's state.

## What is braided, precisely

### `origin.rs` (502 lines) — writes merge-gate input, then pumps the gate

`release_holds` is the set of project slugs whose merge queue is held by an
open origin release. It is *not* origin's private state: `eval.rs:1215` reads
it when deciding whether a job may land, and `core.rs:1019` rebuilds it at
startup. `origin.rs` writes it in three places and, in two of those, drives
the merge gate directly:

| Site | What it does |
| --- | --- |
| `origin_release` (`origin.rs:331`) | `self.release_holds.insert(slug)` — takes the hold |
| `origin_sync`, merged arm (`origin.rs:434`) | removes the hold, then `self.pump_merges(owner, project).await?` |
| `origin_sync`, closed arm (`origin.rs:443`) | same |

`pump_merges` is the §3.3 merge-gate driver. A crate boundary here would mean
either a second writer of `release_holds` (docs/reference/style.md Tier 3 forbids it) or
exporting `pump_merges` as part of the "narrow interface", which is not narrow
— it is the landing pipeline.

`link_project` and `origin_release` also wear `TODO(track-C8)`
`too_many_lines` allows, which the refactor-plan notes shrink only "when their
decisions leave" — i.e. under H1, not under a file move.

### `triage.rs` (451 lines) — creates task records and launches through the actor

Triage is advisory (§1.2) and never calls `set_state`, so the *charter* holds.
The plumbing does not. `triage_job` reads the job off the in-memory graph
(`must_get`, :77), creates a task record through the single writer
(`task_create`, :137), builds container env through the exec slice's credential
injectors (`inject_git_ssh_command`, `inject_platform_agent_secrets`), and
launches with `self.provider` + `self.launch_reporter` while cloning
`self.self_tx` (:191) so the exit re-enters as a `Msg`. `on_triage_exited`
then writes the task terminally (`task_put`, :253/:276) and publishes
job-events.

Every one of those is the actor's own machinery. Extracting the context would
mean either re-exporting task writing and container launching across the crate
boundary — a wider interface than the one it replaces — or moving triage's
launch tail with it, which is exactly the "second writer of job state" the
brief rules out.

### `github.rs` (207 lines) — clean, but not worth moving alone

The REST client takes no `Core` and is already behind the `PullRequestApi`
trait. It could move today. It should not: `Core::pr_api` is the only consumer,
`origin.rs` is the only caller, and splitting one context across two crates to
bank a 207-line win buys a boundary nobody reads while leaving the boundary
that matters unchanged. It moves when origin moves.

## What has to happen first

Track H already describes it, and its dependency (`B2`) is why it has not run:

- **H1 (`decide::origin`)** carves the link/release/status/sync classification
  into `crates/domain/src/decide/`, adds the forge effect variants (push a
  `chug/release-{n}` snapshot, open a PR, reset `integration`), and — the part
  that matters here — names `release_holds` as the point where the origin and
  merge-gate deciders meet. Once the hold is an effect the interpreter applies,
  origin stops writing it and stops calling `pump_merges`.
- **H2 (`decide::triage`)** carves out admissibility, task shape, and exit
  recording, leaving a launch tail that can be handed the ports it needs.

After both, forge-ingest's remaining surface is git ops, REST calls and one
container launch — a shape that takes ports as arguments the way
`platform-ops::harvest` does, and re-enters the loop through `Msg` rather than
through `&mut Core`. **That** is when the crate is worth carving, and the
interface should be messages, not a trait: the effect results already re-enter
as events elsewhere in the tree, and matching that keeps one story.

## Recommendation

1. Do not carve `crates/forge-ingest` before H1 and H2 land. Re-run C9's <!-- intent -->
   second half as a follow-on ticket that depends on them; its acceptance is
   unchanged (own crate, no dispatcher internals beyond the interface,
   integration tests green, registry updated).
2. Keep the context's directory and charter where C8 put them. They are
   accurate — no member drives a transition — and the charter is what H1/H2
   are measured against.
3. The C9 machinery is reusable as-is: `.chug/tasks/ci.sh`'s registry gate now walks
   any context crate's `src/`, and `boundary_guard.rs` has the
   allowed-edge-list shape (`platform_ops_declares_only_its_charter_edges`)
   that a forge-ingest crate would copy with a different constant.
