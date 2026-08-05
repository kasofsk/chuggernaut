# Design — Binary artifact handoff between jobs (gap 5)

Status: FINDING — gap 5 is a harvest-and-retention gap, not a store; the cross-job half is retired. S0–S2 landed (jobs #363, #381); S3 stays deferred behind a second consumer.

Written against the tree at `8997c4e` (2026-08-01; the source tree is unchanged
since `a539b7d`, which this document's first revision was written against).
Every claim about
Chuggernaut's current behavior below was read out of the source or out of
[`docs/spec.md`](../spec.md) in this tree, not carried over from the brief or from
a sibling design; where the brief or a sibling disagrees with the source, the
source wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree). The **beacon** half is
different: `~/beacon` is not checked out in this workspace, so nothing here
re-derives it — the artifact inventory is the operator's 2026-08-01 inspection,
relied on secondhand and marked as such wherever it is load-bearing.

This document closes [#308](308-gha-port.md) gap 5.

## Current state

*The **mutable head** ([#415](415-knowledge-architecture.md) [D2](415-knowledge-architecture.md#d2-every-design-doc-opens-with-a-mutable-current-state-head)):
rewritten to current truth whenever anything below it changes. Everything after
this section is append-only — the original argument and its dated corrections,
never edited into the prose above them.*

The finding holds: gap 5 is a harvest-and-retention gap, and the harvest half
shipped. The rows below are the states of [Sequencing](#sequencing)'s table,
which keeps each slice's full argument.

| Slice | What | State |
| --- | --- | --- |
| **S0** | Cap the worker `copy_file` reply and name the error | **Landed** (job #363) |
| **S1** | `ArtifactKind::Output`, `collect_output` on the work-side monitors, the 16 MiB cap, a chunked `copy_file` op | **Landed** (job #381) |
| **S2** | The `outputs` bucket with its own `max_age` + byte ceiling; revoke-time GC | **Landed** (job #381) |
| **S3** | Declared `outputs:` schema, cross-job reads, per-attempt selection | Deferred — gated on a **second** consumer with per-output addressing needs |
| — | The S3/Minio artifact store | Stays deferred; nothing here needs it |

## The question

`crates/store/src/artifacts.rs` holds session transcripts, container stdout and
operator-uploaded job attachments — the record of what a task *did*. Nothing
carries a **build output** from one job to another, and `docs/spec.md`'s Appendix:
Deferred still lists "Binary artifact store: S3/Minio for non-git artifacts" as
open. [#308](308-gha-port.md) ranks that gap 5 of 11.

So: **does Chuggernaut need a binary artifact store, and if not, what is the
real shape of gap 5?**

The answer this document reaches is that gap 5 decomposes into three pieces of
very unequal size, and the one everybody names — inter-job binary handoff — is
the one with no consumer:

1. A **latent defect** on a shipped path (`copy_file` on a worker node has no
   size bound), which must be fixed whatever else is decided.
2. A small, cheap **output-harvest convention**, worth building when a first
   consumer exists and not before.
3. **Retention differentiation and lifecycle GC**, which is the only genuinely
   new design work here — and less new than the brief assumed, because a
   bucket-wide TTL already exists.

Inter-job handoff is refuted as a need, and this document argues that it should
stay deferred rather than being built speculatively.

## What is true today (verified in this tree)

| Fact | Where | State |
| --- | --- | --- |
| Blobs live in a JetStream **Object** Store — chunked internally, so not bound by `max_payload` | `crates/store/src/artifacts.rs` module header; `crates/store/src/lib.rs` (`create_object_store`) | Shipped |
| Blobs are gzipped then age-encrypted under `age_artifacts` — a **second** keypair, held by dispatcher *and* api, so the api serves blobs directly instead of proxying through the dispatcher | `crates/store/src/artifacts.rs`; `crates/api/src/run.rs`; `docs/spec.md` §1.6 | Shipped |
| The artifacts bucket is created with **`max_age: 90 * DAY`** and no byte ceiling | `crates/store/src/lib.rs` `ensure_topology_inner` | Shipped |
| `ArtifactKind` is a closed enum with exactly two variants (`session.jsonl`, `stdout.log`) | `crates/store/src/artifacts.rs` | Shipped |
| Attachments pack content type + plaintext size into the object description, so a listing never opens a blob; capped at 16 MiB | `crates/store/src/artifacts.rs`; `crates/api/src/routes.rs` (`MAX_ATTACHMENT_BYTES`) | Shipped |
| Artifact keys are `{owner}.{project}.{seq}.{task_id}.{kind}` — no attempt segment, because a retry is a **new task id** | `crates/store/src/keys.rs`; `crates/domain/src/decide/work.rs` (`next_task_id`) | Shipped |
| Harvest already does exactly the operation gap 5 needs: `copy_file` out of the exited container → `ArtifactStore::put` | `crates/platform-ops/src/harvest.rs` (`collect_agent`) | Shipped |
| Containers are removed only *after* every `logs`/`copy_file` read — the 2026-07-21 disk-leak fix | `crates/platform-ops/src/harvest.rs` (`dispose`); `docs/spec.md` §3.1 | Shipped |
| An evaluator already reads a file out of its own container (`/workspace/eval-result.json`) | `crates/dispatcher/src/launch_queue.rs`; `docs/spec.md` §3.3 | Shipped |
| `copy_file` is **single-file** by contract and is in the shared backend conformance suite | `crates/container/src/lib.rs`; `crates/test-utils/src/backend_suite.rs` | Shipped |
| On a worker node, `copy_file` base64s the whole file into **one** NATS reply, with **no cap** | `crates/worker/src/daemon.rs` (`copy_file`); `crates/worker/src/backend.rs` | Shipped — and see [C3](#c3-the-real-size-regime-is-copy_file-on-a-worker-node-not-the-object-store) |
| `logs` *is* capped, at `LOGS_CAP` = 700 KiB, with a truncation marker | `crates/worker/src/daemon.rs`; `docs/spec.md` §3.1 | Shipped |
| `MAX_REQUEST_BYTES` (900 KiB) guards the worker **request** only; replies are unguarded | `crates/store/src/worker.rs` | Shipped |
| VCS *is* the platform's artifact-passing mechanism, normatively | `docs/spec.md` §5.1 "Artifact passing" | Normative today |
| No retention concept below the bucket; no GC on revoke/retry; `delete_attachment` is the only explicit delete | `crates/store/src/artifacts.rs` | Shipped |
| Binary artifact store (S3/Minio) | `docs/spec.md` Appendix: Deferred, Appendix: Infrastructure Summary | Deferred |

Two rows deserve emphasis before anything is proposed.

**`docs/spec.md` §5.1 already answers "how does a job hand output to another job".**
Its "Artifact passing" paragraph is not a placeholder: all jobs work on
`job/{seq}`, evaluation-pass squash-merges to the default branch, and
"downstream jobs start from the default branch — upstream work is already there
by the time they launch, guaranteed by DAG dependency ordering." Gap 5 is
therefore not "jobs cannot hand each other output". It is narrower: **output
that must not go into VCS** has nowhere to go.

**The mechanism for capturing such output already exists, on one of the two
harvest paths.** `Harvester::collect_agent` does `backend.copy_file(id, &path)`
→ `self.store(…, ArtifactKind::SessionTranscript, &bytes)`. Its sibling
`collect_logs` — command work, wrap-up, command evals — reads `logs` only and
does no `copy_file` at all, so the change is a new `Harvester` method and two
call sites rather than one line
([which containers are read](#which-containers-are-read-and-what-that-actually-costs)).
That is still a smaller change than anything in the brief's option space.

## Corrections (verified against the tree)

The brief asked for these explicitly, and three of them change the shape of the
work.

### C1. #308 does not attribute gap 5 to mobile IPA/APK outputs

The brief says "#308 category F is wrong and this doc should correct it — #308
says gap 5 is needed for mobile outputs (IPA/APK from the fastlane jobs)."
Verified: the strings `IPA`, `.apk`, `TestFlight` and `Play Store` do not occur
anywhere under `docs/` or in `docs/spec.md`, in any case. #308's gap-5 row says only
"`crates/store/src/artifacts.rs` holds transcripts, stdout and attachments;
there is no inter-job binary handoff", and #308 §F ("Mobile (2.5 workflows)") is
entirely about Xcode and `xcrun simctl` needing a macOS host — it never mentions
artifacts.

So the correction has no target in #308; the belief it corrects is the brief's
own. The *substance* still matters and is recorded here as evidence rather than
as a retraction: **no IPA or APK is uploaded as an artifact in beacon** — the
fastlane runs call `upload_to_testflight` / `upload_to_play_store`, so the
binary goes to Apple and Google from inside the build and never lands in
artifact storage. (Secondhand: operator inspection, 2026-08-01.)

### C2. The artifact store already has a retention concept

The brief says "`artifacts.rs` today has no retention concept — only an explicit
`delete_attachment`." The store's *API* has none, but the bucket does:
`ensure_topology_inner` in `crates/store/src/lib.rs` creates the artifacts
object store with `max_age: 90 * DAY`, sitting alongside a 90-day `job.events`
stream and a 7-day channel inbox. Artifacts already expire.

What actually does not exist, restated precisely:

- **No differentiation.** One TTL for transcripts, stdout and attachments alike.
  Beacon's two classes (7-day failure logs, 30-day coverage) have no expression.
- **No byte bound.** The bucket is created with `..Default::default()`, so
  `max_bytes` is unset: retention is age-bounded and **not** size-bounded. A job
  that writes 4 GB of output is bounded only by wall-clock.
- **No lifecycle GC.** Revoking a job, retrying a task, or deleting a project
  leaves the blobs to age out on the bucket clock.

That reframes the largest piece of work from "invent retention" to
"**differentiate** retention and add a byte bound". It is a materially smaller
and better-shaped problem, and
[Retention and GC](#retention-and-gc-the-only-genuinely-new-piece) below takes
it in that form.

### C3. The real size regime is `copy_file` on a worker node, not the object store

The brief is right that transport is mostly a non-problem, and right about why:
the object store chunks internally, `age_artifacts` exists precisely so the api
decrypts and serves blobs directly instead of proxying them through a
`max_payload`-bound dispatcher reply, and the #344/#345 diff paging exists
because a diff is regenerated per page from live refs (hence
`DiffPage::digest` on every page, `crates/vcs/src/diff_page.rs`) rather than
because blobs cannot be stored. All verified. Storage is not the constraint.

**But the brief asked for a size regime where the mechanism genuinely does not
serve, and there is one — on the harvest path, not the storage path.**

On a worker node, `FleetBackend::copy_file` (`crates/worker/src/backend.rs`)
routes to `WorkerRpc::copy_file`, and the daemon
(`crates/worker/src/daemon.rs`) answers with the entire file base64-encoded into
a single JSON reply:

```rust
Ok(CopyFileOk { data_b64: data.map(|d| b64_encode(&d)) })
```

There is **no cap on that reply**. The sibling `logs` op caps at `LOGS_CAP` =
700 KiB and sets a `truncated` flag; `copy_file` does neither.
`MAX_REQUEST_BYTES` (900 KiB, `crates/store/src/worker.rs`) guards the
*request* direction only. Base64 costs 4/3, so with NATS's default 1 MiB
`max_payload` the largest file that can come back is roughly:

```text
1,048,576 × 3/4  ≈  786,432 bytes  ≈  768 KiB raw, before the JSON envelope
```

Call it **~760 KiB**. Above that the daemon's reply cannot be published, the
requester blocks until `OP_TIMEOUT` (60 s, `crates/store/src/worker.rs`) and
sees a transport error.

Three consequences, in ascending order of severity:

1. **It is placement-dependent.** `NodeHandle::Docker` calls the backend
   in-process, with no NATS hop and no bound but memory. The identical file
   harvests fine on the dispatcher's own Docker node and fails on a worker —
   and per [361-per-run-placement.md](361-per-run-placement.md), *not one job type in
   this repo sets `placement:`*, so which node a container lands on is not
   controlled by anyone.
2. **It is already live, on a shipped path.** `eval-result.json` extraction
   (`crates/dispatcher/src/launch_queue.rs`) is `copy_file(…).ok().flatten()`,
   so an oversized structured result degrades to `eval_json: None` — after a
   silent 60-second stall — rather than to an error anyone sees. A command
   evaluator emitting a large `failed_tests` list on a worker node hits this
   today.
3. **It is the exact thing an output feature would do.** "Harvest a declared
   file" is `copy_file` on a file chosen to be large. Building output harvest
   on top of this bound would ship a feature that works on one node kind and
   silently fails on the other.

`docs/spec.md` §3.1's small-message discipline documents the `logs` tail explicitly
and says nothing about `copy_file`. That silence is the specification bug behind
the code bug.

**This is the first slice of work, and it is a defect fix, not a feature.**

### C4. `InjectedFile.artifact` is unrelated to the output direction

`InjectedFile.artifact` (`crates/container/src/lib.rs`) names a **static,
node-local, deploy-time-provisioned** file — the channel MCP binary — that a
worker-proxying backend references by name so the bytes never ride in the launch
payload; the daemon fails a launch naming an unknown one, and reports artifact
hashes in `ping` (`docs/spec.md` §3.1). It is an input-side optimization keyed to
things provisioned *with the worker*, at the worker's git SHA. Nothing about it
generalizes to per-job output: an output is not known at deploy time, is not
shared across jobs, and must travel *from* the node rather than being found
there.

The name collision is unfortunate. There is exactly one thing worth borrowing,
and it is a constraint rather than a mechanism: this design exists because
**the platform already decided that bulk bytes do not ride the worker RPC**
(`docs/spec.md` §3.1, "static artifacts are node-local"). C3 is that same rule being
violated in the return direction.

### C5. #313 half A gives a token, not a bucket

The brief says half A "gives keyless GCS without new platform storage". Half A
issues a per-container workload-identity token that federates to a cloud service
account ([#313](313-workload-identity-image-builds.md) A1, A5), and its own
sequencing names "a job that reads a bucket" as a half-A-alone use case. So the
conclusion holds — but the platform ships a *credential*; someone still has to
provision a bucket, an IAM binding and a lifecycle rule on the cloud side.
That is an operator cost, not a code cost, and this document counts it as such
below.

## The hypothesis, tested

The brief's hypothesis:

> Gap 5 is not a binary artifact store. It is (a) declared-file harvest of small
> diagnostic outputs into the object store that already exists, plus (b)
> retention and GC, which genuinely do not exist. Build outputs are covered by
> the registry and, where a bucket is wanted, by #313 half A. No new platform
> storage is needed.

**Confirmed on its main claim, with three amendments.**

Confirmed: no new platform storage is needed. The object store handles the size
class in question (see the [size band](#the-size-band-with-numbers)), the
crypto, the API routes and the UI surface all exist, and the S3/Minio line in
Appendix: Deferred should stay deferred.

Confirmed, and worth stating harder than the brief does: **the cross-job half of
gap 5 has no consumer at all.** Zero binaries cross a job boundary in beacon
(secondhand). No deploy consumes a build artifact — `deploy-web-prod.yml` passes
`image_tag: ${{ github.sha }}` and resolves through the registry, and
[#313](313-workload-identity-image-builds.md) B4 already prescribes the stronger
version of that ("prefer a digest input over an `image_tag` input… rolling back
to a tag rolls back to whatever that tag points at *now*"). The single genuine
cross-job read is two small `test-results.json` files consumed by a
`notify-on-failure` job — and [#308](308-gha-port.md) §A already maps beacon's
failure ping onto `Escalated` + a Human escalation task (§3.4), not onto a
downstream job. An escalation is composed by the dispatcher from task results it
already holds; it does not read an artifact. **That case does not survive as an
artifact problem.**

Amendment 1 — **(a) is not free, because of [C3](#c3-the-real-size-regime-is-copy_file-on-a-worker-node-not-the-object-store).**
Declared-file harvest is `copy_file` on a deliberately larger file. The bound
must land first, or the feature is nondeterministically broken by placement.

Amendment 2 — **(b) is smaller than stated, per [C2](#c2-the-artifact-store-already-has-a-retention-concept).**
A bucket-wide 90-day TTL exists. The work is differentiation, a byte bound, and
lifecycle GC.

Amendment 3 — **"declared-file" is the wrong half of "declared-file harvest".**
The declaration should not be in the job-type schema. That is
[Decision 2](#decision-2-a-well-known-path-not-a-schema-field) and it is the
main design argument in this document.

## Options weighed

### Option 1 — Do nothing; retire gap 5 outright

Close gap 5 as "not a gap": VCS is the artifact-passing mechanism (`docs/spec.md`
§5.1), the registry is the image-passing mechanism (#313 B4), and a bucket
via #313 half A covers anything else.

*For:* zero code. Consistent with the evidence: nothing in beacon needs handoff.

*Against, and this is real:* the diagnostic-capture case is unserved and the
current workarounds are all bad. A container is removed at exit (`docs/spec.md`
§3.1), so anything not harvested is gone. Today the escape hatches are (i) print
it to stdout — captured as `stdout.log`, but truncated to the most recent 700
KiB on a worker node, and a truncated tarball is worthless where a truncated log
is not; or (ii) commit it to the job branch — which then squash-merges into the
default branch, which is wrong for a 4 MB coverage tree, and impossible for a
`wrap_up: type: none` job whose branch is scratch and is deleted at the terminal
state (`docs/spec.md` §5.1, "Branch cleanup").

**Rejected**, but only barely, and only because option 2 is so cheap. If option
2 turns out to cost more than the sketch below, option 1 is the correct
fallback.

### Option 2 — Harvest an output file into the store that already exists

Add one `ArtifactKind`, harvest one well-known path at container exit before
`dispose`, serve it through the artifact routes that already exist.

*For:* reuses the object store, the `age_artifacts` crypto, the key layout, the
api routes, the UI, and `Harvester`'s existing call shape. The whole feature is
a variant of a code path that ships today. No new dependency, no new bucket
semantics, no new access-control surface — an output is scoped to
`(owner, project, seq, task_id)` exactly like a transcript.

*Against:* it inherits the object store's ceiling (see the size band), it needs
the C3 bound fixed first, and it adds a second thing competing for the same
bucket's disk — which is what forces the retention work.

**Recommended**, gated on a first consumer.

### Option 3 — Lean on external storage via #313 half A

The job's own script pushes to a cloud bucket using a workload-identity token.

*For:* zero platform storage, unbounded size, and — the underrated part —
**retention comes free and better**: a GCS object lifecycle rule is one
terraform stanza and is the operator's to tune, versus code and a bucket TTL
here. It is also the same shape as the already-settled answer for images (#313
B4 registry digests), which is evidence the factoring is right.

*Against:* the artifact becomes invisible to the platform. No per-task listing,
no link from the job page, no access control derived from project membership —
the bucket's IAM is a separate world with separate grants. It requires #313 half
A to have shipped (it has not) and an operator to provision a bucket per
project. And a project with no cloud identity cannot use it at all, which
includes this repo today.

**Recommended for anything above the size band**, and *not* as the primary
answer, because the primary consumers are small.

### Option 4 — Build the inter-job artifact store

Content addressing, a `uses:`/`needs:` handle by which a consuming job names a
producer's artifact, and semantics for revoked / retried / never-merged
producers.

*For:* it is what "gap 5" literally says, and it is what GHA has.

*Against:* **no consumer.** The evidence says zero binaries cross a job boundary;
the one cross-job read maps onto Escalation; images resolve through the
registry; source-shaped output resolves through VCS by spec. Worse, storage is
the easy half — the hard half is lifetime semantics (what does a consumer read
when the producer was revoked? retried? escalated? when its branch never
merged?), and every one of those answers is a policy nobody currently needs and
therefore nobody can adjudicate. Building it now means guessing, and a wrong
guess here is a durable wire contract, not a local refactor.

**Rejected.** Keep the Appendix: Deferred line. What a future consumer would
need is written down in [Addressing and lifetime](#addressing-and-lifetime-if-a-consumer-ever-appears)
so the next author starts from the constraints rather than from scratch.

## Decision 1: fix the `copy_file` bound first, as a defect

Independent of every feature question. Three ways:

1. **Cap and fail loudly.** The daemon refuses a `copy_file` whose encoded reply
   would exceed a `MAX_REPLY_BYTES` guard, returning a named `WorkerError`. ~20
   lines. Converts a silent 60-second stall into a diagnosable error, and
   satisfies STYLE Tier 2 rule 3 ("every wait a timeout… on hitting a bound fail
   fast and loud").
2. **Chunk it.** A cursor-shaped read, exactly like `logs_tail`'s byte offsets
   — the precedent is in the same file and the same trait. Because the daemon
   logs-and-falls-back on an unknown op, adding a *new* op is additive and does
   **not** bump `WORKER_RPC_VERSION` (`crates/types/src/version.rs` says so in
   as many words); changing `copy_file`'s existing reply shape would.
3. **Have the worker write the object store directly.** Rejected on a structural
   ground: the worker would need the `age_artifacts` identity, and the whole
   point of that keypair is that exactly two holders exist — the dispatcher
   (writes) and the api (reads). Handing a decrypt-capable key to every fleet
   node to save one hop trades a real boundary for a small efficiency. `docs/spec.md`
   §10.2's dispatcher-only posture is the same argument.

**Decision: ship (1) now, add (2) with the output feature, never (3).** (1) is
correct on its own — an oversized `eval-result.json` should be an error and is
currently a silence — and it is the honest thing to have in the tree if the
output feature never happens. (2) as a new op keeps the N±1 contract additive
per `docs/spec.md` §14.1.

`docs/spec.md` §3.1 gains a sentence putting `copy_file` under the same small-message
discipline it already states for `logs`.

## Decision 2: a well-known path, not a schema field

The brief asks which job-type level declares the captured paths, and how
`if: failure()` semantics are expressed. **The recommendation is that neither
question should be answered, because the declaration should not be in the job
type at all.**

### Why a schema field is the expensive answer

Per-container scoping argues for nesting the declaration where `secrets:` lives
— `WorkSpec.secrets` and `Evaluator.secrets` — and #313 A5 makes cloud identity
per container for the same reason. But every nested block carries
`deny_unknown_fields` deliberately (`docs/spec.md` §14.2: an ignored key inside an
`Evaluator` could silently skip a merge gate). So an N-1 dispatcher meeting
`work.outputs:` **fails the whole parse** — and it fails it *before* reading
`min_dispatcher`, which is a top-level field in the same
`serde_yaml::from_str`. The result is not §14.2's park-with-a-clear-reason; it
is the 2026-07-22 shape, a config refused outright.

Hoisting it to the top level inverts the failure: an N-1 dispatcher tolerates
the unknown field, warns, and runs every job producing nothing — which is
exactly the failure mode `INPUTS_SCHEMA_EPOCH` exists to prevent. Mitigating it
means the `inputs:` treatment in full: a new epoch constant, a field rule in
`JobType::validate` requiring `min_dispatcher`, a `CONFIG_SCHEMA_EPOCH` bump,
the §14.3 merge-time skew gate, and the deploy-before-merge ordering that comes
with it.

**So: yes, a declared `outputs:` forces an epoch bump** — that is the direct
answer to the brief's question — and the bump is the smallest part of the cost.

### The cheaper answer, and why it is also the better one

`eval-result.json` is the precedent, and it has **zero declaration**: the
platform extracts one well-known path from every eval container, and a command
evaluator that wants a structured verdict writes it (`docs/spec.md` §3.3). Do the
same:

```text
/workspace/chug-output.tar.gz   — harvested if present, at container exit,
                                  before dispose; stored as ArtifactKind::Output
```

One path. One `copy_file` (which is single-file by contract — hence a tarball;
the producing script does its own archiving, which is also how a *directory*
like `coverage-html/` becomes capturable at all). One new `ArtifactKind`
variant. One new value on the existing artifact routes. No schema field, no
epoch bump, no skew rule, no field-rule test.

### Which containers are read, and what that actually costs

"A well-known path" is only well-defined once it says *whose* container. The
harvest entry points differ by task kind, and they do not all read files today:

| Entry point | Covers | Reads files today? |
| --- | --- | --- |
| `Harvester::collect_agent` (via `collect`) | agent **work** (`crates/dispatcher/src/exec.rs`), agent **eval** (`crates/dispatcher/src/eval.rs`), **triage** (`crates/dispatcher/src/forge_ingest/triage.rs`) | Yes — `logs` **and** a `copy_file` of the transcript |
| `Harvester::collect_logs` | command work and wrap-up (`MonitorKind::Logs`), the eval log tail, the `scan.rs` timeout harvest | **No** — `logs` only |
| `MonitorKind::Eval` arm | command eval / merge gate | Yes — `copy_file` of `/workspace/eval-result.json` |

**Decision: work-side containers only** — agent work and command work, plus
wrap-up. Eval, merge-gate and triage containers are not read, consistent with
[Are artifacts inputs to evaluation?](#are-artifacts-inputs-to-evaluation): an
evaluator's structured output already has a channel (`eval-result.json`), and
adding a second one would multiply the disk pressure R1/R2 are sized against by
the evaluator fan-out (2–4 containers per job) rather than by the work count
(1–2).

Two consequences that correct the "one line" framing earlier in this document:

- **The hook cannot go inside `collect_agent`.** `collect` is called
  identically from the agent-work path and the agent-eval path, so a line added
  there would silently harvest evaluators and triage too. It is instead a new
  `Harvester::collect_output(owner, project, seq, task_id, id)` — a `copy_file`
  plus a `store`, the same shape as `collect_agent`'s transcript branch — called
  from the work-side sites.
- **It is not the same edit twice.** On the agent path it is a second
  `copy_file` next to one that already exists. On the command path it is the
  *first* `copy_file` in `collect_logs`'s world, which reads only `logs` today.
  Still small — one method and two call sites — but "one line" was wrong.

**Wrap-up is included deliberately, not by accident.** `MonitorKind::Logs`
covers command work *and* wrap-up, and `spawn_exit_monitor` is handed a
`task_id` rather than a `TaskPhase`, so excluding wrap-up would mean threading
the phase through or adding a third `MonitorKind`. A wrap-up container is
work-shaped and its output has the same lifetime, so the cheaper answer is to
let it produce one too. If a later consumer needs work-only capture, that
plumbing is the cost, and it is worth paying then rather than now.

And it dissolves the `if: failure()` question rather than answering it. GHA
needs `if:` because the upload is a declarative *step* in a list that the runner
evaluates; here the producer is a shell script that already knows whether it
failed:

```sh
trap 'tar czf /workspace/chug-output.tar.gz -C fastlane report.xml' EXIT
```

Failure-only capture, success-only capture, capture-with-different-contents —
all of it is ordinary shell, in the job's own script under `.chug/tasks/`, with no
conditional expression language to design, specify, validate or version. This is
[#311](311-job-inputs.md) Decision 1's move applied to the output direction: it
refused a substitution engine and delivered a value to the container instead, so
that "parameterization happens inside the work script where it always belonged".
The same argument holds here, and it holds more strongly, because conditionality
is harder to express declaratively than substitution.

*Honest cost of the convention:* the platform cannot pre-announce "this job type
produces X" in the UI before a run; the blob is opaque (fixed content type, no
per-entry listing without unpacking); and there is one output per task, not
several with separate retentions. Every one of those costs is only paid in a
world with multiple outputs and multiple consumers — Option 4's world, which the
evidence refutes. **Revisit the schema only if a second consumer appears that
needs per-output addressing.** Until then this is the STYLE Tier 3 call:
a simpler shape would do.

### The size band, with numbers

- A workspace-scale `coverage.lcov` is order 1–3 MB raw and gzips to a few
  hundred KB. `coverage-summary.txt` and `report.xml` are tens of KB.
- A `coverage-html/` tree is order 5–20 MB raw, and tars+gzips to order 1–3 MB.
- A container image, an IPA or an APK is 50 MB–2 GB and belongs in a registry or
  a bucket, never here.

So the band this design serves is **up to ~16 MiB compressed**, and the number
is not invented: `MAX_ATTACHMENT_BYTES` (`crates/api/src/routes.rs`) is already
16 MiB, the largest blob the platform accepts anywhere. Reusing it keeps one
number instead of two.

Above the band, the answer is Option 3 (#313 half A + a bucket). Two things are
true at once about what the platform does with an over-band output, and an
earlier draft of this document conflated them.

**The bound is refused loudly; the task is not failed.** Refusing to store is a
deliberate inversion of the `logs` behavior — a truncated log still carries the
tail that matters, a truncated archive carries nothing, so the over-band case
must never be a silent truncation. But "loudly" means *a named error at the
bound*, not a failed task, and that is a deliberate **alignment** with
`Harvester`'s discipline rather than a departure from it. Every path in
`crates/platform-ops/src/harvest.rs` is warn-only by rule: `collect_agent`
warns on a failed `logs`, on `Ok(None)` from `copy_file` and on `Err` from
`copy_file`; `collect_logs` warns and returns `None`; `store` warns on a failed
`put`; and `dispose`'s doc comment states the rule outright — "a failed removal
leaks disk but must never fail a job, so it only warns." The doc comment on
`collect` gives the why: "a job must never fail because its *reporting*
failed." An output archive is reporting.

So the shape is: the worker daemon refuses the oversized `copy_file` with the
named `MAX_REPLY_BYTES` error from
[Decision 1](#decision-1-fix-the-copy_file-bound-first-as-a-defect), and the
harvest logs that error at `error!` (not `warn!`, since unlike a missing
transcript it names a specific operator action — move the output to a bucket)
and stores nothing. STYLE Tier 2 rule 3 is satisfied where it applies: the bound
exists, it is checked, and hitting it produces a named failure instead of a
truncation or a 60-second stall.

The mechanical argument points the same way. Harvest runs in the post-exit
monitor, *after* the container's exit code is known and while `TaskExit` is
being composed (`crates/dispatcher/src/launch_queue.rs`). "Fail the task" for a
task that exited 0 is therefore not a `tracing::error!` at all — it is a new
`TaskExit` field and a state-machine decision about a command that succeeded,
which is a contract change (STYLE's contract-first rule) bought for a reporting
miss. The operator's signal is the error line plus the artifact's absence from
the task's listing. If that proves too quiet in practice, the place to fix it is
an explicit *absence* entry in the artifact listing — recorded here as the
follow-up, not proposed now.

## Retention and GC (the only genuinely new piece)

Per [C2](#c2-the-artifact-store-already-has-a-retention-concept) the starting
point is a single 90-day, byte-unbounded bucket. Three things to decide.

### R1. Differentiation is a second bucket, not a policy engine

JetStream has no per-object TTL — `max_age` is a property of the object store's
backing stream, so every object in a bucket shares one clock. Two ways to get
two retention classes:

- **A sweeper**: a periodic pass listing objects and deleting by age/class. New
  loop, new bound, new failure mode, and it duplicates what the server already
  does.
- **A second bucket**: `outputs`, created alongside `artifacts` in
  `ensure_topology_inner`, with its own shorter `max_age` and — the important
  part — its own **byte ceiling**, so that a job which writes 16 MiB every run
  evicts *outputs* and can never evict a transcript.

**Decision: a second bucket.** It is one config literal and one key prefix
against a whole eviction loop, and the isolation property is the substantive
one: **transcripts are the audit record of what an agent did and must not be
evictable by a build byproduct.** Beacon's two classes (7-day logs, 30-day
coverage) map onto exactly this shape — two buckets, not a policy language — and
if a third class is ever wanted it is a third bucket, which is a much better
scaling story than a per-object retention field would have been.

Starting values, tuned by the operator rather than fixed here: `outputs` at
**14 days** and a byte ceiling sized from the node's free disk, both read from
`CHUG_OUTPUTS_MAX_AGE_DAYS` / `CHUG_OUTPUTS_MAX_BYTES` at `chuggernaut init`
and re-applied to the live bucket each time it runs.

`async-nats` 0.38 **does** expose the ceiling: `jetstream::object_store::Config`
has `max_bytes: i64` beside `max_age: Duration` (confirmed against
`async-nats 0.38.0`, which the workspace resolves). The sweeper fallback is not
needed.

**The ceiling refuses; it does not evict.** `Context::create_object_store`
builds the backing stream with `discard: DiscardPolicy::New`, so a bucket at
`max_bytes` rejects new writes and keeps what it holds. That is the *better* of
the two behaviors and the implementation keeps it: evicting old messages from an
object store's stream would drop an object's oldest *chunks* while its newer
metadata survived, turning a stored archive into a corrupt read rather than a
clean absence. So the failure at the ceiling is a refused `put`, logged at
`warn!` with the dial to raise — [R3](#r3-who-pays-and-how-loudly)'s "never
discovered as a 404", reached by refusal rather than by eviction. The isolation
property R1 exists for is unaffected: the two buckets are separate streams, so
output pressure cannot touch a transcript either way.

### R2. Lifecycle GC deletes outputs, never transcripts

`Harvester::dispose` exists because container overlays were a disk leak that
took the platform down on 2026-07-21. Outputs are the next thing in that class:
they scale with what a job *built*, not with how many tokens it spent.

- **On revoke**: delete the job's outputs. Best-effort, off the actor thread,
  the same shape and the same never-fail-a-job discipline as `dispose`.
- **Never on revoke**: transcripts, stdout, attachments. A revoked job is still
  an audit record, and `docs/spec.md`'s Appendix: Deferred ("terminal jobs are
  immutable") is the posture to preserve.
- **On retry**: nothing. A retry is a **new task id**
  (`crates/domain/src/decide/work.rs`, `next_task_id`), so per-attempt
  artifacts already coexist under distinct keys — this is a problem the existing
  key layout already solved, and it should not be re-solved.

### R3. Who pays, and how loudly

Disk is the operator's, and today they cannot see it coming: there is no
per-project artifact-usage surface anywhere. Two things make the cost legible
without building an accounting system:

1. The per-task cap (16 MiB, above) bounds the worst case per task, and the
   bucket ceiling bounds the worst case in aggregate. Both are hard bounds that
   fail loudly.
2. The ceiling is loggable: a bucket at its ceiling turning an output away is a
   fact worth a `tracing::warn!` naming the dial to raise, because "my output
   isn't there" should never be discovered by a 404.

A per-project quota is **not** proposed. It needs an accounting surface, an
enforcement point and an operator UI, and the two bounds above cover the failure
that actually took the platform down.

## Are artifacts inputs to evaluation?

The precedent the brief names is real: `crates/dispatcher/src/launch_queue.rs`
already does `copy_file(&id, "/workspace/eval-result.json")`, so an evaluator's
own container is already read for a file. The question is whether an *evaluator*
should be able to read the *work* container's output.

**No, and no mechanism should be added.** Three reasons:

- The work container is gone by then. `dispose` removes it at exit (`docs/spec.md`
  §3.1), long before the eval fan-out; only the stored blob remains, so this
  would be "an evaluator reads the artifact store", which is a new read path
  with new access questions, not an extension of `copy_file`.
- It is not needed for the case in hand. A `coverage` job that wants a threshold
  gate has the same script produce the number and the verdict;
  `eval-result.json` already carries structured rework context (`docs/spec.md` §3.3).
- The merge gate deliberately re-runs command evaluators against
  `merge-gate/{seq}` — a *different tree*, in a fresh container (`docs/spec.md`
  §3.3). Nothing the work container built exists there, by design, because the
  gate verifies the integration rather than the change. An artifact-as-eval-input
  mechanism would sit awkwardly across that boundary and would invite exactly
  the confusion the staged gate exists to prevent.

## Addressing and lifetime (if a consumer ever appears)

No case in the evidence survives to need this, so nothing here is proposed. It
is recorded so the next author starts from the constraints:

- **Addressing is by producing job.** `keys::artifact_key` is already
  `{owner}.{project}.{seq}.{task_id}.{kind}`; a consumer names a `seq` it
  already has from `deps`. No content addressing, no declared names, no new
  namespace — the existing key *is* the address.
- **Per-attempt already works.** A retry is a new task id, so attempts do not
  overwrite each other. What is *unowned* is "which task id is the current
  one" from a consumer's point of view — that is a job-record read, and it is
  the first thing a real consumer would have to specify.
- **A revoked producer's outputs are gone** (R2), so the address 404s.
  `ArtifactStore::get` already returns `None` for "never captured", so a
  consumer cannot distinguish absent-because-revoked from
  absent-because-never-produced. A consumer must treat absence as a hard
  failure, never as a default — and if it needs to tell those apart, that
  distinction is the design's first new requirement.
- **A never-merged branch is not a problem.** Artifacts are keyed by job seq,
  not by ref, so they outlive `job/{seq}`'s deletion (`docs/spec.md` §5.1). This is a
  genuine argument for the object store over "commit it to the branch", and the
  only place where the deferred inter-job story would be *easier* than it looks.

## Sequencing

| Slice | What | Gate on |
| --- | --- | --- |
| **S0** | Cap the worker `copy_file` reply and name the error; one sentence in `docs/spec.md` §3.1 | **Landed** (job #363), as the defect fix of [Decision 1](#decision-1-fix-the-copy_file-bound-first-as-a-defect); the `copy_file` rows and [C3](#c3-the-real-size-regime-is-copy_file-on-a-worker-node-not-the-object-store) above describe the tree before it |
| **S1** | `ArtifactKind::Output`; a `Harvester::collect_output` reading `/workspace/chug-output.tar.gz` before `dispose`, wired to the **work-side** monitors only ([scope](#which-containers-are-read-and-what-that-actually-costs)); the 16 MiB cap, refused with a named error and warned rather than failing the task ([why](#the-size-band-with-numbers)); a chunked `copy_file` op | **Landed** (job #381), on the gate job #375 met: the consumer is `.chug/jobs/coverage.yaml` + `.chug/tasks/coverage.sh`. See below |
| **S2** | The `outputs` bucket with its own `max_age` + byte ceiling; revoke-time GC | **Landed** (job #381), with S1 as required — S1 without it re-creates the 2026-07-21 disk class |
| **S3** | Declared `outputs:` schema, cross-job reads, per-attempt selection | A **second** consumer with per-output addressing needs. Deferred, deliberately |
| — | S3/Minio artifact store (`docs/spec.md` Appendix: Deferred) | Stays deferred; nothing here needs it |

**The first consumer is in this repo, not in beacon.** [#308](308-gha-port.md)
§G retires `rust-coverage` into "an ordinary job — coverage is a thing you ask
for, not a thing that runs on every push". That `coverage` job type now exists
(`.chug/jobs/coverage.yaml`): it produces `coverage.lcov` plus an HTML tree and a
summary, and only the summary survives, because stdout is the one thing harvest
keeps. That is the smallest honest consumer for S1. Note which side
of the scope table it lands on: a coverage run is command-shaped, so it is a
command **work** container and its harvest is the new `copy_file` in
`collect_logs`'s world — the half of S1 that does not yet exist, not the half
that is a second line next to the transcript copy.

**The beacon import needs none of this.** Taking the four artifact sites in
turn: `rust-coverage` is terminal and a human reads it (S1, or Option 3);
the two fastlane uploads are `if: failure()` logs whose value the escalation
path already delivers, since a failing task's stdout is harvested and the
escalation carries it (`docs/spec.md` §3.4); `flutter-integration-tests`'
`test-results.json` pair feeds a failure issue, which #308 §A maps onto
Escalation. So **gap 5 is not on the beacon critical path** and should not be
sequenced as if it were.

One ordering constraint worth naming: mobile lands under
[#309](309-host-native-execution.md) host-native execution, where a "container"
is a host process and what `copy_file` even means is #309's to define. That is a
further reason not to freeze an output *schema* before #309 lands — a
convention survives a backend change more gracefully than a wire contract does.

## What would refute this

Stated plainly, because a FINDING that cannot be falsified is an opinion:

- A consumer that genuinely needs a **binary** produced by job A inside job B's
  container, where neither VCS (`docs/spec.md` §5.1) nor a registry digest (#313 B4)
  serves. That reopens Option 4, and its first requirement is the
  revoked/retried semantics in
  [Addressing and lifetime](#addressing-and-lifetime-if-a-consumer-ever-appears).
- An output class routinely above 16 MiB compressed with no cloud identity
  available. That makes Option 3 unusable and forces a real store — the
  Appendix: Deferred S3/Minio line, on its original terms.
- Two consumers wanting **different** retentions for outputs of the same job.
  That breaks R1's one-bucket-per-class shape and is where a declared schema
  (S3) starts paying for itself.

## Related

- [#308](308-gha-port.md) — gap 5, §A (failure ping → Escalation), §F (mobile),
  §G (`rust-coverage` becomes a job)
- [#313](313-workload-identity-image-builds.md) — half A (workload identity),
  B4 (digest-first tagging, the registry answer for images)
- [#311](311-job-inputs.md) — Decision 1, the "value delivered to the container,
  not a substitution" precedent this document applies to the output direction
- [#309](309-host-native-execution.md) — host-native execution, which redefines
  what harvesting means for mobile
- [361-per-run-placement.md](361-per-run-placement.md) — the model for handling a brief
  that overstates its case, and the source for "no job type sets `placement:`"
- `docs/spec.md` §1.6, §3.1, §3.3, §5.1, §14; Appendix: Deferred
- `crates/store/src/artifacts.rs`, `crates/store/src/lib.rs`,
  `crates/store/src/keys.rs`, `crates/platform-ops/src/harvest.rs`,
  `crates/container/src/lib.rs`, `crates/worker/src/daemon.rs`,
  `crates/worker/src/backend.rs`, `crates/store/src/worker.rs`,
  `crates/dispatcher/src/launch_queue.rs`, `crates/types/src/version.rs`
