# Design #311 — Job inputs (parameterizing a run without rewriting it)

Status: IMPLEMENTED IN PART — slice A shipped in jobs #314–#319.

[Slice A](#minimum-useful-version) — the recommended first ship — landed end to
end and is deployed: `CONFIG_SCHEMA_EPOCH` is 2 in
`crates/types/src/version.rs`, `crates/types/src/inputs.rs` holds the shared
rules and `crates/domain/src/inputs.rs` the pure decider, `CHUG_INPUT_*` is
injected into work, eval and wrap-up containers, and `.chug/jobs/rollback.yaml`
plus `.chug/tasks/rollback.sh` are the first consumer (#314–#317, with the
deploy #318 carrying the epoch bump to prod). Slice B's web half — declared
inputs rendered on the create form, the Draft editor and the job header —
shipped in #319. **Still open:** slice B's `### Inputs` block in
`job_brief_block` (`crates/dispatcher/src/exec.rs`) and the squash-body
`Inputs:` line, which is what agent jobs need; and slice C (`inputs:` on a
schedule file), blocked on [#310](./310-scheduled-jobs.md) landing.

Written against the tree at `acdb2c6`. Every claim about current behavior below
was read out of `spec.md` and the source in this repo; where the brief and the
tree disagree, the tree wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree).

Doc 3 of 4 extracting implementable specs from
[design #308](./308-gha-port.md). Gap 1 of that doc is the motivation
("Twelve deploy workflows differ by two strings"); category C is the blocked
case (`rollback` needs an `image_tag`). #308 fixes the frame — an input
"parameterizes a run without rewriting it" — and defers everything else here.

Sibling docs 1 and 2 — [host-native execution](./309-host-native-execution.md) and
[scheduled jobs](./310-scheduled-jobs.md) — are **also PROPOSED and not
implemented**. Nothing below assumes any field, epoch or mechanism either of
them proposes exists; where sequencing matters it is called out explicitly
(see [Skew](#skew-what-a-new-field-costs)).

Related: [spec.md](../../spec.md) §1.1 (the `Job` record, job types, the
field-rule matrices, the config root), §2.1 (state machine, batches), §2.2
(release validation's three passes), §4.1 (container env), §4.3 (the job
brief), §5.3 (the reserved `CHUG_` prefix), §8.1 (vars), §8.2 (secrets), §10.3
(audit trail), §13.4 (factory semantics), §14 (config and version skew);
[design-lifecycle.md](../../design-lifecycle.md) (the eval floor — the
governing constraint); [STYLE.md](../../STYLE.md) (Tier 1 pure `types`;
Tier 2 #2 asserts, #3 bounds, #4 naming, #6 tests; Tier 3 simplicity);
[contracts.md](../../contracts.md) and [NORTH-STAR.md](../../NORTH-STAR.md)
(decider/effect factoring); [testing.md](../../testing.md) (test tiers);
[CLAUDE.md](../../CLAUDE.md) (per-consumer forge; single writer).

## Problem

A job type is a static file; a job carries no parameters. Three consequences,
all live:

1. **`rollback` is unexpressible.** The GitHub workflow it would replace takes
   a git SHA as a `workflow_dispatch` input. There is no per-job value of any
   kind, so #308 records rollback as *blocked* while forward-only deploys port
   today.
2. **Twelve near-identical deploy job types.** #308's survey counts 16 deploy
   workflows, of which twelve are the product `deploy|rollback × {web, worker,
   bot} × {dev, prod}`. Ported one-for-one, twelve job-type files differ by two
   strings each.
3. **A schedule has nothing to pass.** [#310 Decision
   10](./310-scheduled-jobs.md) stops at a single static `description` and
   names `inputs:` on a schedule file as this doc's to add.

The mechanism one reaches for first is already ruled out.
[design-lifecycle.md](../../design-lifecycle.md) ("eval criteria are a floor,
additive per job") rejects full per-job override because it "would let a job
creator silently drop the type's merge-gate protections". **The central risk of
this feature is that inputs become the backdoor that decision closed** — a job
creator who cannot override `eval:` directly should not be able to reach the
same outcome by supplying a value that selects a different evaluator, image or
secret. Everything below is organized around making that unreachable rather
than discouraged.

## Corrections (verified against the tree)

Three claims in the brief that shaped this doc do not survive contact with the
source. Each changes an argument, so each is recorded.

1. **The reserved `CHUG_` prefix is spec §5.3, not §11.** §11 is "Mobile /
   PWA". The rule lives in §5.3 (linked-origin credentials): "The `CHUG_` name
   prefix is **reserved**: declaring such a secret in a job type is a
   release-validation error and injection skips them". Enforced at
   `crates/dispatcher/src/release.rs` (`static_errors`, against
   `forge_ingest::origin::RESERVED_SECRET_PREFIX = "CHUG_"`) and again at
   injection in `crates/dispatcher/src/exec.rs` (`container_env`).

2. **The reserved prefix covers secrets only — vars are unchecked.**
   `static_errors` applies the prefix rule to `work.secrets` and per-evaluator
   `secrets`; the `vars` loop beside it checks existence only. `container_env`
   likewise filters `CHUG_`-prefixed *secrets* out of injection and applies no
   such filter to vars. Var names are validated to `[A-Za-z0-9_]+` at write
   time (`crates/store/src/keys.rs`, spec §1.4), so `CHUG_INPUT_IMAGE_TAG` is a
   legal var name today. Any design that puts inputs behind the `CHUG_` prefix
   must close that gap first — see
   [Decision 4](#decision-4-delivery-via-one-reserved-env-namespace).
   **Closed by slice A1**: the prefix rule now covers vars in `static_errors_kv`
   and `container_env`, and spec §5.3/§8.1 say so. This correction records the
   state that motivated the change, not the state of the tree.

3. **This codebase does not use an `<untrusted_input>` delimiter convention.**
   The single occurrence of "untrusted" in the tree is
   `web/src/components/CoverWidget.tsx`, about HTML sanitization. The delimiter
   idiom is an ecosystem convention, not an established repo one; adopting it
   would be new. That matters because it cannot be the load-bearing control —
   see [Decision 5](#decision-5-injection-safety).

One smaller note: job creation is handled in
`crates/dispatcher/src/handlers/jobs.rs` (`CreateJobBody` → `CreateJobRequest`
→ the single-writer actor); `crates/api/src/routes.rs` only forwards the body
opaquely as `serde_json::Value`.

## Decision 1: an input is a value, not a substitution

### The classification the brief asks for

Working through the §1.1 field-rule matrices and `JobType::validate` in
`crates/types/src/job_type.rs`, asking of each field "may a job-supplied value
choose this?":

| Field | Classification | Why |
| --- | --- | --- |
| `eval[*]` (all of it) | **never** | The floor. Selecting an evaluator, its `run`, `prompt`, `required` or `stage` by input *is* the override design-lifecycle.md rejected |
| `image`, `eval[*].image`, `wrap_up.image` | **never** | The image is the execution contract: what tools, what CA bundle, what baked target. An input-chosen image is an input-chosen gate (the `ci` evaluator's `image` decides what `cargo` even is) |
| `work.secrets`, `eval[*].secrets`, `wrap_up.secrets` | **never** | A computed secret name (`DEPLOY_KEY_${env}`) defeats §2.2's "every secret named in `secrets:` has an entry in the KV bucket" — the blast radius stops being readable from the file |
| `wrap_up.*` | **never** | `type: merge \| none` decides whether the merge gate runs at all. An input flipping it to `none` skips the gate outright |
| `work.type`, `work.run`, `work.prompt`, `work.review` | **never** | These *are* the job. `run` is executed as `sh -c` inside the bootstrap's `sh -c` (`crates/container/src/lib.rs`, `bootstrap_cmd`); `prompt` is the instructions. Substituting into either is arbitrary code / arbitrary instructions — see [Decision 5](#decision-5-injection-safety) |
| `work.provider`, `work.model` | **never** (by inputs) | Already covered by `Job.model` (§1.1), scoped to Work-phase agents so evaluators keep the type resolution. A second path would be a second mechanism for one idea |
| `resources.*` | **never** (by inputs) | Already partly covered by `Job.timeout`, deliberately Work-phase-only. `cpu`/`memory` are fleet-capacity concerns ([#293](./293-worker-capacity.md)), not job targets |
| `work_retries`, `eval_retries`, `rework_budget` | **never** (by inputs) | These are the closest call in the table, and they land on the same side. They bound *how many times a gate is re-run*: raising `rework_budget` buys more attempts at passing eval, and lowering `eval_retries` decides how hard a flaky evaluator is tried before the job gives up. That is gate-adjacent, not merely operational. Same "one mechanism per idea" answer as `resources`: if per-job retry budgets are wanted, they belong as a `Job.*` field argued on the `Job.timeout` precedent, where the Work-phase-only scoping can be argued too |
| `job_deadline` | **never** (by inputs) | Groups with `resources` — a scheduling bound, not a job target; `Job.timeout` is again the precedent for the per-job version |
| `placement.node` | **never** | Placement is a fleet fact; an input naming a node lets a job creator pick which host runs project code |
| `knowledge`, `vars` | **never** | `knowledge_tags` already has a per-job additive path (§1.1). `vars` names are KV-existence-validated like secrets |
| `min_dispatcher`, `name`, `display_name`, `description` | **never** | Identity and the skew gate |

That is every field of `JobType` and its nested blocks
(`crates/types/src/job_type.rs`); `unknown` is the §14.2 catch-all, not a field
an author writes.

Every field classifies the same way. There is no "conditional" row and no
"safe" row. That result is the design: **if no job-type field may be chosen by
an input, then no substitution engine needs to exist**, and the classification
does not have to be maintained as a list somebody can forget to extend when the
schema grows a field.

Note the existing precedent this sits beside honestly. The floor is *not*
absolute today: `Job.timeout` and `Job.model` (§1.1) do override type fields per
job. The line the repo already drew is **gate relevance** — `eval` is
additive-only, `timeout` is scoped to Work tasks so "evaluators keep the type
default", and `model` likewise. Inputs sit on the safe side of that same line,
and go further: they override nothing at all.

### The decision

> **An input is a value delivered to a running container. It is never
> substituted into job-type configuration, and the job type is resolved without
> reading it.**

Concretely: `Job.inputs` is a `BTreeMap<String, String>` on the job record. Its
only consumers are the two composition points that build what a container sees
— `container_env` (env) and `job_brief_block` (the agent prompt). The job type
is loaded from `base_ref` by `release::load_job_type` and merged with the job's
additive evaluators by `domain::release::with_job_evaluators`; neither reads
`inputs`, and neither gains a parameter to.

The `run:` string is passed to `bootstrap_cmd` byte-for-byte as it appears in
the YAML. The `prompt:` path is resolved and read byte-for-byte. Twelve deploy
types collapse not because `run:` is templated, but because one
`run: ./.chug/tasks/deploy.sh` script reads `$CHUG_INPUT_SERVICE` — the
parameterization happens *inside* the work, where it always belonged.

### How this is enforced, not documented

Three mechanisms, in decreasing strength:

1. **A pure equality property, tier-1 tested.** `JobType` derives
   `PartialEq`. The invariant is: *for any job type, any job, and any two input
   maps, the `JobType` returned by the release path is equal.* That is one
   property test in `crates/domain/src/release.rs`'s test module, at the lowest
   tier that can express it (`testing.md`), and it fails the moment anyone
   threads inputs into config resolution. It is the closest thing to
   unrepresentable available without a newtype ceremony that `types` (pure
   data, no async, no I/O) should not carry.
2. **No syntax to write.** There is no `${...}` or `{{ ... }}` handling
   anywhere in the config loader, and this design adds none. The failure mode
   of writing `run: ./deploy.sh ${service}` in a job type is that the shell
   expands an unset variable — visible, not silently substituted.
3. **A launch-path assert (STYLE Tier 2 #2, negative space).** `container_env`
   asserts that every `CHUG_INPUT_*` key it inserts was absent from the map,
   and that no non-input key it inserted starts with `CHUG_INPUT_`. A collision
   means an invariant is broken upstream, not that a value should quietly win.

## Decision 2: declaration is a typed schema on the job type

Inputs are declared where every other contract is declared — the job type file,
repo-versioned, resolved at `base_ref`.

```yaml
# ── Top-level ────────────────────────────────────────────────────────────────
inputs:                        # optional; declares the values a job of this type accepts
  - name: string               # required; [a-z][a-z0-9_]* — lowercase so the env mapping is injective
    type: string | enum        # required
    required: bool             # optional; default false
    default: string            # optional; disallowed when required: true
    values: [string]           # required for type: enum; disallowed otherwise
    pattern: string            # optional, type: string only; a regex that must MATCH the whole value
    description: string        # optional; shown in the create form and the agent brief
```

`Input` is a nested block, so it carries `#[serde(deny_unknown_fields)]` like
every other nested block (§14.2): an unknown key inside an input declaration
could silently drop a `pattern`, which is a validation control.

`default` is a declared value the platform materializes onto the job record, not
a create-form pre-fill — see
[when a default becomes a value](#when-a-default-becomes-a-value), which is
load-bearing for the audit story in
[Decision 6](#decision-6-immutability-and-audit).

### Typed schema, or a free-form string map?

**Option A — free-form `map<string, string>`, no declaration.** A job carries
whatever keys the creator sends; the script checks what it needs.
*For:* zero schema, zero validation code, zero skew question about the job type
file, and it composes with anything.
*Against:* it moves every check inside the container, so a missing or malformed
value fails **at launch**, in a work task, after a container start — which is
precisely the failure moment §2.2's three passes exist to avoid, and the one
[#310](./310-scheduled-jobs.md) and §13.4 both argue against (a factory's
`auto_release` validation failure "leaves the job Frozen and surfaces a
`factory-release-failed` event rather than escalating"). It also gives the
create form nothing to render, gives the operator no way to discover what a
type accepts, and — fatally — gives injection safety no place to live: an
undeclared value has no charset, no pattern, and no reviewable declaration.

**Option B — a typed schema (recommended).** Names, kinds, required/optional,
defaults, allowed values, pattern.
*For:* it matches how this repo already thinks. `types` is pure data whose
validation is shared by every consumer (CLI, release, launch); `resources.memory`
is format-validated at parse "so a bad value fails offline instead of at
container launch"; the §1.1 field-rule matrices are the culture. It gives
`chuggernaut validate` and `web/src/pages/NewJob.tsx` something to work from,
and it makes `pattern` a *declared, reviewed, repo-versioned* control rather
than a convention.
*Against:* real cost — a new nested block, a new field-rule set, a regex
dependency in `types` (or a hand-rolled matcher), and a skew bump. And it can
overreach: a rich type system here would be a second config language.

**Option C — typed, but the type set stays at two.** The recommendation. `string`
(optionally narrowed by `pattern`) and `enum` (a closed list). Deliberately
**no `bool`, no `int`, no list, no object** in v1:

- `bool` is `type: enum` with `values: ["true", "false"]` — the env
  representation is the string either way, and adding a kind buys nothing but a
  rendering question.
- `int` is a `string` with `pattern: '^[0-9]+$'` — and the script would parse
  the env var as text regardless.
- lists and objects have no env representation that is not an encoding
  decision, and nothing in the motivating cases needs one. When something does,
  the extension point is a JSON file (see
  [Decision 4](#decision-4-delivery-via-one-reserved-env-namespace)), not a fifth
  kind.

This is Tier 3 "simplicity over performance" applied to a schema: two kinds
cover all three motivating cases, and the third kind can be added additively
without moving the epoch again.

**Bounds** (STYLE Tier 2 #3): at most `inputs_count_max = 16` declared inputs
per job type; each value at most `input_value_len_max = 256` characters — which
is also 256 bytes, since the charset in
[Decision 5](#decision-5-injection-safety) is ASCII-only. Both are hard errors,
not truncation.

## Decision 3: supply paths, and where a bad input fails

### Who may supply

| Path | Supplies inputs? | Notes |
| --- | --- | --- |
| `POST .../jobs` (§6.2) | **yes** | `inputs: { name: value }` on the create body, alongside `eval`, `timeout`, `model` |
| `PATCH .../jobs/{seq}` (Draft, §2.1) | **yes** | Full-field replace like every other field; Draft only |
| A schedule file ([#310](./310-scheduled-jobs.md) Decision 10) | **yes** | `inputs:` on the schedule, passed through the same origination path — the seam #310 reserved. One-directional: this design does not depend on schedules |
| Factory `create_job` MCP tool (§13.4) | **yes** | Same trust as a triage agent choosing `type` and `description` today; the non-weakening invariant is what makes that safe |
| A **claim** (§1.2) | **no** | A claim parks the *next attempt*; it never redefines the job. See [Decision 6](#decision-6-immutability-and-audit) |
| A **rework** cycle | **no** | Same reason. A rework re-enters Work against the same target |
| Batch creation (§2.1) | **no** (v1) | A batch collapses N *members* into one branch and one run; if the members carry differing inputs there is no defensible union (unlike `deps` and `eval`, values do not union). Reject a batch whose members carry inputs, with a clear field error, rather than inventing one |

### Where each failure surfaces

§2.2 runs validation in three passes. Inputs use all three, and the split
follows what each pass can actually know:

| Failure | Pass | Result |
| --- | --- | --- |
| Malformed name (`[a-z][a-z0-9_]*`), value over the length cap, value outside the default charset, more than `inputs_count_max` entries | **Creation** (422) | Needs no job type file, so the operator gets it back on the form immediately. These are exactly the injection-relevant checks — cheapest place, earliest moment |
| Undeclared input name; missing `required` input; `enum` value not in `values`; `string` value not matching `pattern` | **Release-time** (pass 1) | `ValidationError { field: "inputs.{name}", … }`; the release request is rejected with the offending job named, exactly like a bad `timeout` or a colliding additive evaluator |
| The type's `inputs:` declaration changed between release and Blocked→Ready | **Ready-transition** (pass 2) | Re-checked at `base_ref` with the rest of static config. Pass 2 today re-checks *files only*, and the declaration is a file-derived fact, so this fits without changing pass 2's character |
| A value that no longer satisfies the charset immediately before injection | **Launch** (pass 3) | Same class as a missing secret/var (`work::EntryFailure`): the job parks rather than launching. Cheap, and it is the defense-in-depth check for a record written before the rule existed — the same reasoning as `container_env` re-filtering reserved secret names "even if a job record predates the rule" |

**Does "fail at validation with an event beats fail at launch" hold?** Yes, and
it is the reason inputs are declared at all. The one deliberate exception is the
launch-time charset re-check, which is not a substitute for the earlier passes —
it is the belt to their braces, and reaching it means an earlier pass was
bypassed.

For a schedule-originated job with `auto_release` (#310), a release-validation
failure lands in the shape §13.4 already defines: the job stays Frozen and an
event surfaces the errors, rather than escalating. No new failure state.

### When a default becomes a value

A declared `default` is useless as a documentation-only field: if it is only
consulted inside `container_env`, the value that actually ran appears on no
audit surface, which defeats [Decision 6](#decision-6-immutability-and-audit)
for exactly the jobs most likely to lean on defaults. So it is materialized.

> **Declared defaults are resolved once, in the same single-writer write that
> first records `base_ref`, and written into `Job.inputs` for every declared
> input the creator did not supply. From that moment `Job.inputs` is the
> complete effective input set.**

That moment is the **Ready-transition**, not release-time pass 1 — a
distinction §2.2 forces. Pass 1 is explicitly "checked against current HEAD …
**not** an execution guarantee", and `base_ref` is recorded by the transition
into Ready (§2.1: `Draft|Frozen|Blocked → Ready` all "record `base_ref` =
current default HEAD"). For a job released straight to Ready the two coincide;
for a job released into **Blocked** they do not, and only `base_ref` is the ref
the run actually uses ("All subsequent execution uses `base_ref` exclusively",
§2.2). Resolving against anything else would let a job execute a script from
`base_ref` with a default read from a different tree.

Consequences, each of which some later section rests on:

- Pass 1 still *checks* required/`enum`/`pattern` for fast feedback — a
  declared `default` satisfies the presence check there, so a missing optional
  input is never a release error.
- A declared `default` must itself satisfy the charset and its own
  `pattern`/`values`, checked in `JobType::validate` at parse time. Otherwise a
  value that no supply path could have produced arrives by the back door and is
  caught only by the launch-time re-check — a Stalled job blamed on nobody.
- Later `base_ref` movements — the §3.2 Work→Evaluation rebase, a merge-conflict
  or merge-gate re-base — **do not** re-resolve. Defaults resolve exactly once;
  re-resolving would make the target mutable mid-flight, which
  [Decision 6](#decision-6-immutability-and-audit) rejects.
- The Ready-transition write only ever **adds** keys absent from the map. It
  never overwrites a supplied value. That is an assert at the write site
  (STYLE Tier 2 #2, negative space), not a merge policy.
- `container_env` therefore reads one map and does no defaulting of its own.
  See [Decision 4](#decision-4-delivery-via-one-reserved-env-namespace) for what
  happens to an optional input that has neither a supplied value nor a default.

## Decision 4: delivery via one reserved env namespace

### The collision story

`container_env` (`crates/dispatcher/src/exec.rs`) currently composes one
`HashMap<String, String>` from four sources: platform variables (`JOB_ID`,
`NATS_URL`, …), `CHUG_*` task-origin stamps (§4.1, §6.3), declared vars (§8.1,
injected under their declared names), and declared secrets (§8.2, decrypted and
injected under their declared names). Vars are inserted **before** secrets, so a
same-named secret silently wins today. Agent containers additionally receive
platform agent credentials, where "declared secrets win on collision" (§4.1).

Adding a fourth un-prefixed namespace to that map would make an already-implicit
precedence rule worse. So:

> **An input named `image_tag` is delivered as `CHUG_INPUT_IMAGE_TAG`.**

The prefix is not decoration; it is what makes the namespace collision-proof by
construction. `CHUG_` is already reserved (§5.3), so no *secret* can be named
into the input namespace. But per [correction 2](#corrections-verified-against-the-tree),
**vars are not covered by that rule today**, so this design has a prerequisite:

- extend the reserved-prefix check in `static_errors` from `secrets` to `vars`
  (release-validation error, same message shape), and
- extend the injection-time filter in `container_env` to vars.

That is a small change that also closes a pre-existing hole — a project can
today set a var named `CHUG_PHASE` and clobber a §6.3 origin stamp.

Input names are lowercase (`[a-z][a-z0-9_]*`) precisely so `name.to_uppercase()`
is injective: without that rule, `image_tag` and `IMAGE_TAG` would both map to
`CHUG_INPUT_IMAGE_TAG`. Inputs are inserted **last**, under the assert from
[Decision 1](#how-this-is-enforced-not-documented).

**Absent means absent.** `container_env` injects one key per entry in
`Job.inputs` and nothing else. By
[the default rule](#when-a-default-becomes-a-value) that map holds exactly the
inputs with a *resolved* value — supplied, or filled from a declared `default`.
A declared optional input with neither gets **no** `CHUG_INPUT_*` key at all;
it is never injected as an empty string. Two things depend on this and would
break under the empty-string alternative: a `set -u` script fails loudly on
`$CHUG_INPUT_SHA` instead of running `update.sh ` with an empty argument (see
[Skew](#skew-what-a-new-field-costs)), and `${CHUG_INPUT_X:-fallback}` means
what a shell author expects.

### Does an evaluator get inputs? The invariant's hard case

This is where the non-weakening invariant is closest to leaking, so it gets the
argument rather than an assurance. The `ci` evaluator that
`.chug/jobs/_defaults.yaml` appends to **every** job type runs
`run: ./.chug/tasks/ci.sh`. Delivering inputs to eval containers puts an
operator- or schedule-supplied value into that gate's environment, one line of
shell away from `if [ "$CHUG_INPUT_SKIP" = 1 ]; then exit 0; fi`.

**The distinction that makes it hold is capability versus value.**
design-lifecycle.md closed a threat with a specific shape: a job creator, with
nothing but a create call, *silently* drops the type's merge-gate protections.
Unilateral, and invisible — nothing in the repo records that it happened. An
input into an evaluator's environment is neither half of that. For the value to
change a verdict, some author must first have written the `CHUG_INPUT_SKIP`
branch into `.chug/tasks/ci.sh` — a **commit in the project repo**, on the same
review path as the `eval:` declaration and the `inputs:` declaration
themselves. (An eval command container clones `job/{seq}`, not `base_ref` —
`bootstrap_cmd` in `crates/container/src/lib.rs` — so such an edit can also
arrive on the job branch, where it lands in the diff the stage-0 agent reviewer
reads and re-runs through the merge gate. Either way it is a reviewed artifact,
not a create-call field.) **A job creator cannot open the path; they can only
supply a value down one a repo author already opened, and both the declaration
and the value are on the record.** That is why
[Decision 1](#the-classification-the-brief-asks-for) forbids selecting
`eval[*].run` — choosing *which* script runs is the capability — while allowing
a value into that script's environment.

**The residual, named.** This is still a widening. Before inputs, a lenient
`ci.sh` is lenient for every job equally, which is visible in aggregate the
first time anyone looks; after, it can be lenient only when a particular job
asks, which is not. Three things carry that:

1. The eval task record and the Ready-transition event carry the effective
   inputs, so "this gate ran under these values" is in the §10.3 layer-1 stream
   rather than reconstructable only from the container.
2. A golden trace asserts a job with **no** inputs produces an eval container
   env byte-identical to today's — the feature is off, not merely unused.
3. `.chug/tasks/check-duplication.sh` and code review already treat the scripts
   under `.chug/tasks/` as reviewed artifacts; that this is the control is stated
   here so a reviewer knows an input-reading branch in a gate script is a
   gate-relevant change, not plumbing.

**The narrowing weighed and rejected: work containers only.** Deliver
`CHUG_INPUT_*` to work (and wrap-up) containers and never to evaluators. It
serves all three motivating cases — rollback's `sha`, the twelve deploys'
`service`, and a schedule's target are all consumed by the work script — and it
would make the invariant structural instead of argued. It loses on one fact:
**§4.3 sends the job brief to every agent evaluator by construction**
("evaluators judge against the same brief the author saw"), and
[Option B](#reaching-an-agent-jobs-prompt) puts inputs in that brief. Excluding
evaluators would therefore mean forking the brief block into a work variant and
an eval variant — destroying the single-choke-point property §4.3 calls
"airtight" — or accepting that agent evaluators see inputs while command
evaluators do not. The second is the worse of the two: an agent evaluator is a
gate, and a far more suggestible one than a shell script. A narrowing that
leaves the more suggestible gate reading inputs is not a structural guarantee,
it is a smaller-sounding one. The cost of the narrowing is real too — a
`deploy` type parameterized over `service` wants `.chug/tasks/deploy-health.sh`
(`.chug/jobs/deploy.yaml`, stage 0) to health-check the service that was
actually deployed, which it cannot do blind.

So: **work, wrap-up and eval containers all receive the inputs**, on the
capability/value argument above rather than on "read-only data is harmless".

### Why env, and what was rejected

**Rejected — a JSON file at `/chuggernaut/inputs.json`**, mirroring §13.4's
`/chuggernaut/events.json` and §4.3's `/chuggernaut/prompt.md`.
*For:* no env collision question at all, no charset restriction needed, and
structured types (lists, objects) for free.
*Against:* every `run:` script would need `jq` to read a value —
`.chug/tasks/deploy.sh` needs `ssh` and `git` and gets them from
`chuggernaut/agent:prod`; adding a JSON parser to the image contract for a
single string is a poor trade. Env is the idiom vars and secrets already use,
which means one mental model rather than two. And a file does nothing for the
agent-prompt case, which needs its own answer regardless.

This is a *deferral*, not a dismissal: if a future input genuinely needs
structure, the file is the extension point, and it is additive.

**Rejected — bare names (`IMAGE_TAG`).** Nicer to write in a script, and that
is the whole case for it. Against: it reopens the collision question against
both vars and secrets, and it erases the one property that matters at review
time — a reader of `deploy.sh` can tell at a glance that `$CHUG_INPUT_SERVICE`
is operator-supplied and therefore untrusted, while `$MINI_DEPLOY_KEY` is
platform-supplied. That distinction is load-bearing for
[Decision 5](#decision-5-injection-safety).

### Reaching an agent job's prompt

§4.3 is explicit that the job brief is "the single point where a job's instance
identity enters any prompt, and it consumes **only** `title` and
`description`", with a regression test asserting the block is byte-identical
with and without `cover_html`. Adding inputs touches that property, so the
options deserve care.

**Option A — inputs never enter the prompt; agents read env like command jobs.**
*For:* the choke point is untouched, and the property stays exactly as stated.
*Against:* an agent will not reliably read an env var it was not told about, and
the value is the *target of the work*. This makes inputs a command-jobs-only
feature in practice.

**Option B (recommended) — an `### Inputs` subsection appended to the brief**,
nested under the existing `## Job Brief` heading, after title/description, one
`name: value` line per input with a resolved value, rendered by
`job_brief_block` in `crates/dispatcher/src/exec.rs`. The level is `###`
deliberately: the block must not emit a sibling of `## Job Brief`, and since
the charset in [Decision 5](#decision-5-injection-safety) excludes `#`, no
value can promote itself to one.

```
---
## Job Brief
**{title}**

{description}

### Inputs
<untrusted_input>
service: web
image_tag: 4f9c1ab
</untrusted_input>
```

A job whose `Job.inputs` is empty emits **no** `### Inputs` subsection — not an
empty one — so the brief for every job that exists today is byte-identical to
what it is now. That is the prompt-side twin of
[absent means absent](#the-collision-story), and it is what keeps §4.3's
`cover_html` byte-identity test valid unchanged.

The choke-point property survives in its real form — *exactly one function
composes instance data into a prompt* — and now carries three declared,
validated fields instead of two. `cover_html` stays excluded. Delivered
identically to the work agent and every agent evaluator, so the evaluator judges
against the same target the author saw (the rule §4.3 already states for the
brief) — which, per
[the hard case](#does-an-evaluator-get-inputs-the-invariants-hard-case), is not
an accident of this option but the reason a work-only narrowing does not hold.

**Option C — template the prompt file (`{{ image_tag }}`). Rejected**, for the
same reason #310 rejected templating `description`: "A template language in a
ticket body is parameterization with none of the typing, declaration or
gate-safety … and it would be the hardest thing to remove once configs depend
on it." Worse here — the prompt file *is* the agent's instructions, and its
path is release-validated at `base_ref` as part of the type's contract.
Substituting into it lets an input change what the agent is told to do, which
is the backdoor this whole design exists to close.

## Decision 5: injection safety

A `command` job runs `run: ./.chug/tasks/deploy.sh`. The launch path builds
`bootstrap_cmd(&["sh", "-c", run])`, which itself produces
`sh -c "git clone … && cd /workspace && exec sh -c '<run>'"`
(`crates/container/src/lib.rs`). So `run` already crosses **two** shells. And
this repo's own `.chug/tasks/deploy.sh` ends with

```sh
ssh -i "$KEY_FILE" -p "$MINI_PORT" … "$MINI_HOST" "$REMOTE_UPDATE $SHA"
```

— an ssh remote command, which is a **third** shell, on another host, and one
where the value is concatenated unquoted. A rollback input flowing to that line
is the realistic worst case, and unlike a secret it is operator- or
schedule-supplied.

Four layers, in order of how much they carry:

1. **No interpolation path exists.** The dispatcher never writes an input into
   `run:`, into a container `cmd`, into an image name, or into a prompt file.
   There is no syntax for it (Decision 1). This is the layer that matters: the
   other three are for what the script does after receiving the value.
2. **A conservative default charset, validated at creation and re-validated at
   launch.** Every `string` and `enum` value must match
   `^[A-Za-z0-9._:/@+-]{1,256}$` — alphanumerics plus seven punctuation
   characters (`.` `_` `:` `/` `@` `+` `-`) and nothing else. Excluded:
   whitespace, newlines, quotes, backticks, backslash, and every shell
   metacharacter (`$`, `;`, `|`, `&`, `<`, `>`, parentheses, braces, `*`, `?`,
   `!`, `#`). What is kept covers the real shapes: `ghcr.io/org/img:sha`,
   `img@sha256:…`, `4f9c1ab`, `feature/x`. **`pattern` may only narrow this,
   never widen it** — the effective check is `charset AND pattern`, and that is
   the rule, not a convention.
   **What the charset does not close, named:** it keeps `-` and `/`, so it stops
   *metacharacter* injection but neither **argument injection** (a value of
   `--force` or `-o` arriving where a positional argument was expected) nor
   **absolute-path substitution** (`/etc/shadow` where a relative path was).
   Quoting does not help with either — `"$CHUG_INPUT_SHA"` is still one
   well-formed argv entry that happens to begin with a dash. That residual is
   precisely what layer 3 exists to cover.
3. **`pattern`/`values` as the real control for a specific input.** The
   rollback case declares `pattern: '^[0-9a-f]{7,40}$'`, and a value that
   reaches the ssh line is then a hex string by construction — which is what
   rules out the leading `-` and the leading `/` the charset alone permits.
   `service` is an `enum` over three names. The default charset is the floor for
   inputs that have no narrower shape; **a declared `pattern` or `values` is the
   control for any input whose value reaches an argv position**, and an input
   that reaches one without a declared narrowing is a review finding, not a
   tolerable default.
4. **Scripts quote their variables.** `"$CHUG_INPUT_SERVICE"`, and the ssh
   remote command should be built as `"$REMOTE_UPDATE $CHUG_INPUT_SHA"` only
   because layer 3 guarantees the shape. This layer is ordinary discipline and
   is explicitly *not* trusted alone — the charset means even an unquoted use
   cannot introduce a metacharacter.

**Inputs are identifiers, not prose.** That is the one-line summary of the
charset rule, and it is also why it does not squeeze anything: free text is what
`title` and `description` are for, and #310 Decision 10 already settled that a
ticket body is not a parameterization mechanism.

**For an agent job's prompt**, the same charset does the work: a value cannot
contain a newline, a `#`, or a backtick, so it cannot open a markdown section,
forge a `## Job Brief` header, or close a code fence. The `<untrusted_input>`
delimiter in Decision 4's block is defense in depth and a readability aid for
the model — and per [correction 3](#corrections-verified-against-the-tree) it
would be a **new** convention in this repo, not an existing one. Do not let a
delimiter carry weight the charset should: a delimiter is advisory to a model,
while a charset is checked.

## Decision 6: immutability and audit

**Inputs are supplied at creation, completed with declared defaults at the
Ready-transition, and immutable thereafter.** Editable while Draft like every
other field; completed exactly once by the write that first records `base_ref`
([the default rule](#when-a-default-becomes-a-value)); after that, frozen —
not on rework, not on a work retry, not on a claim, not on a later `base_ref`
update, not on an `Escalated → Work` Retry resolution.

Two writers of the field, both on the single-writer dispatcher path, and the
second only ever adds keys the first left absent. There is no third.

The argument is the audit trail, and it is the same argument `base_ref` makes.
`base_ref` pins *which config version* a job ran against; inputs pin *what it
acted on*. Together they are the complete answer to "what did this run do". If
inputs were mutable, cycle 1 could deploy `4f9c1ab` and cycle 3 deploy `a91f22c`
under one job id, and the record would be a lie about at least one of them. It
also follows from the single-writer rule: there is no second writer of job
state, and "the operator edits the target mid-flight" would be one.

Getting a different target is getting a different job — which costs one `POST`,
and is honest about what happened.

**Where the record appears:**

- **`Job.inputs`** — `BTreeMap<String, String>` (deterministic ordering, like
  `JobType::unknown`), `#[serde(default)]` so it is empty on old records. After
  the Ready-transition it is the *effective* set: supplied values plus resolved
  defaults, and nothing else.
- **`job-created`** carries the **supplied** inputs, so the §10.3 append-only
  event stream — "the primary audit log for all execution activity" — records
  what the originator asked for, beside the `factory` provenance field it
  already carries (§13.4) and the `schedule` one #310 proposes.
- **The Ready-transition event** (`job-released`, or `job-unblocked` for a job
  that was Blocked) carries the **effective** set, on the same transition that
  records `base_ref`. Reading the two events together answers both "what was
  asked for" and "what actually ran", and the difference between them is
  exactly the defaults — which is the whole reason to materialize them.
- **The UI** renders them on the job header (beside `base_ref`) and in
  `web/src/pages/NewJob.tsx`, which reads the type file to render one field per
  declared input: an `enum` as a select, a `string` as a text field with its
  `description` as help text and `pattern` client-validated.
- **The squash-merge commit body** gains an `Inputs: service=web
  image_tag=4f9c1ab` line above the agent's closing summary, mirroring how a
  batch's member list opens the body (`crates/dispatcher/src/eval.rs`).

That last one deserves a caveat the brief's framing invites and the tree
refutes: **the motivating cases produce no commit.** `deploy` is
`wrap_up: type: none` — eval-pass goes straight to Done, the branch is scratch
and is deleted unmerged, so there is no squash body at all. For deploy and
rollback the durable record is the §10.3 **event stream** (layer 1, "the primary
audit log for all execution activity") plus the job record itself — *not* git
history (layer 3). The commit-body line is worth adding because merge-mode jobs
will use inputs too, but it is not the answer to "a deploy's history says what
it deployed" — the event stream is.

## Decision 7: matrix / fan-out is excluded

Gap 1 of #308 names "No matrix, no dispatch inputs, no job-type params" in one
row. They are two features, and this doc ships one of them.

**Excluded, for four reasons:**

1. **Fan-out is a creation-time concern with no dispatcher surface.** N jobs
   with N input sets is N `POST .../jobs` calls. Nothing in the state machine,
   the scan loop, the merge queue or the launch queue needs to know they are
   related. A platform feature that a client `for` loop already implements is
   the definition of the wrong place to put it.
2. **Batches already occupy this conceptual slot, in the opposite direction.**
   §2.1 batches are N tickets → **one** branch, one evaluation, one merge. A
   matrix is one definition → **N** executions. Shipping both leaves two
   many-jobs primitives whose names differ by one letter of intent, which is
   exactly the "second, subtly-different answer" failure #310 spent its first
   decision avoiding.
3. **A matrix needs failure semantics the job model has no answer for.**
   Fail-fast or run-to-completion? One escalation or N? Does a `wrap_up: merge`
   matrix produce N squashes or one? Every answer touches §2.1, the most
   expensive surface in the tree, and none of them is implied by "rollback needs
   a SHA".
4. **The economics that justify GHA's matrix are absent.** A workflow run is
   heavyweight, so GHA amortizes it; a chuggernaut job's cost *is* its
   container, and N jobs cost what an N-way matrix costs. The saving would be
   configuration, not compute.

**The honest cost:** "deploy all three services" becomes three form
submissions. That is a genuine UX regression against a GHA matrix, and the
mitigation is client-side — a "create N" affordance in
`web/src/pages/NewJob.tsx` that submits one `POST` per value of a chosen enum
input. It needs no schema, no epoch, and no state-machine change, and if it is
never built the operator loses a few clicks, not a capability.

## Worked example

### The smallest real one: `rollback` for this repo

`.chug/jobs/deploy.yaml` today ships "the current main to prod" — one target,
no parameter, and it needs no inputs. Its rollback sibling is the minimum
useful consumer of this design and it lives here, not in a hypothetical repo:

```yaml
# .chug/jobs/rollback.yaml
name: rollback
display_name: Rollback
description: Ship a specific prior SHA to prod (ssh → Mini update.sh).
image: chuggernaut/agent:prod
min_dispatcher: 2                 # see Skew — the number is whatever the epoch is at implementation
inputs:
  - name: sha
    type: string
    required: true
    pattern: '^[0-9a-f]{7,40}$'   # a git SHA, and nothing else, reaches the ssh remote command
    description: The commit SHA to roll back to. Must already be built on the Mini.
work:
  type: command
  run: ./.chug/tasks/rollback.sh
  secrets: [MINI_DEPLOY_KEY]
eval:
  - name: health
    type: command
    image: chuggernaut/agent:prod
    run: ./.chug/tasks/deploy-health.sh
    stage: 0
    secrets: [DEPLOY_HEALTH_API_TOKEN]
wrap_up:
  type: none
resources:
  cpu: 1
  memory: 512Mi
  task_timeout: 30m
```

The script differs from `.chug/tasks/deploy.sh` in exactly one line — the SHA
comes from the input instead of `git rev-parse HEAD`:

```sh
# .chug/tasks/rollback.sh (differences from deploy.sh only)
SHA="$CHUG_INPUT_SHA"             # deploy.sh: SHA="$(git rev-parse HEAD)"
```

Everything else — the health gate, the secret scoping, `wrap_up: none`, the
self-restart reconciliation note — is `deploy.yaml` unchanged. That is the test
of whether this design is the right size: rollback costs one YAML file, one
input declaration, and one changed line of shell.

### The twelve: does the beacon case actually collapse?

`deploy|rollback × {web, worker, bot} × {dev, prod}` = 12. Parameterized:

```yaml
# deploy.yaml for one environment, parameterized over service
name: deploy-prod
min_dispatcher: 2                       # whatever the epoch is at implementation — see Skew
inputs:
  - name: service
    type: enum
    values: [web, worker, bot]
    required: true
work:
  type: command
  run: ./.chug/tasks/deploy.sh          # reads $CHUG_INPUT_SERVICE
  secrets: [PROD_DEPLOY_TOKEN]          # static, KV-validated, readable from the file
```

**It collapses partially — 12 → 4, not 12 → 1 — and the residue is
principled.** The rule that survives the arithmetic:

> **The type is the unit of authority. The input is the unit of target.**

- **`service` collapses.** Three services, same credentials, same gate, same
  script. This is the axis inputs were made for.
- **`environment` does not.** dev and prod are different secrets. A single type
  covering both must declare the union `secrets: [DEV_TOKEN, PROD_TOKEN]`, and
  then *every* run gets both — a strictly wider blast radius than the twelve
  files have today, and a direct regression against #308's own "secret blast
  radius" win. The alternative — deriving the secret name from the input
  (`${env}_DEPLOY_TOKEN`) — is [Decision 1](#the-classification-the-brief-asks-for)'s
  forbidden case: it defeats §2.2's KV-existence check and makes the blast
  radius unreadable from the file.
- **`deploy` vs `rollback` does not.** Rollback takes a required `image_tag`;
  deploy does not. Merging them means an optional input whose absence changes
  the meaning of the run, and a job record that no longer says plainly what
  happened. The action is what the type is *for*.

So: four types (`deploy-dev`, `deploy-prod`, `rollback-dev`, `rollback-prod`),
each parameterized over `service`. A 3× reduction, with the surviving axes
being precisely the two that carry authority and intent. If a future design
lands per-input secret scoping, `environment` becomes collapsible too — but
that is a secrets-model change, not an inputs one, and it should be argued on
its own.

## Skew: what a new field costs

Current state: `CONFIG_SCHEMA_EPOCH = 1` (`crates/types/src/version.rs`).

**Surface 1 — the job type's `inputs:` block.** It is a new *top-level* field,
so per §14.2 an N−1 dispatcher **tolerates** it: `JobType` drops
`deny_unknown_fields`, captures it into `JobType::unknown`, and emits a
`config-warning`. Tolerance is normally the good outcome. Here it is the
dangerous one:

- required-input validation does not run, so a job releases with no target;
- no `CHUG_INPUT_*` reaches the container.

For `.chug/tasks/deploy.sh`-shaped scripts (`set -eu`) an unset variable is a
loud failure — which works only because
[absent means absent](#the-collision-story): an unresolved input injects no key
at all, never an empty string, so `set -u` has something to catch. For a script
without `set -u`, an unset `$CHUG_INPUT_SHA` means `update.sh ` with an empty
argument — a wrong external action taken silently.
That is §14.2's "config ahead of binary becoming a gate quietly disabled",
applied to an effect rather than a check.

**Therefore the epoch must move, and the gate must be structural.** Bump
`CONFIG_SCHEMA_EPOCH` in the same commit as the parser change, and add a rule to
`JobType::validate`: **a non-empty `inputs:` requires `min_dispatcher >=` the
new epoch**, reported as an ordinary `FieldRuleError::Required`. This is the
same one-line technique [host-native execution](./309-host-native-execution.md)
proposes for `runtime.mode: host`, and for the same reason — `min_dispatcher`
is author-declared, so leaving it to authorship guarantees somebody forgets.
The crux is that the *gate itself* is N−1-legible: `min_dispatcher` is a field
today's dispatcher already parses and enforces
(`JobType::requires_dispatcher`, `crates/types/src/job_type.rs`). The N−1
binary captures `inputs:` into `unknown` and never sees it — but it does see
`min_dispatcher`, so `release::load_job_type` refuses the config
(`crates/dispatcher/src/release.rs`), release validation rejects, and a launch
that reaches it parks the job **Stalled** pre-Work with the skew diagnostic
(§14.2, `domain::release::SCHEMA_SKEW_FIELD`). Ahead of all that,
`.chug/tasks/ci.sh`'s config-skew gate fails the config's own CI against the
deployed epoch (§14.3). The new `validate()` rule exists only so the author
cannot omit the declaration in the first place.

**Sequencing with doc 1.** Do not hard-code the number in either doc:

| Order | Inputs bumps | Inputs requires | Host-native must then declare |
| --- | --- | --- | --- |
| Inputs first | 1 → 2 | `min_dispatcher >= 2` | 2 → 3, `min_dispatcher >= 3` (its doc's "1 → 2" becomes stale) |
| Host-native first | 2 → 3 | `min_dispatcher >= 3` | unchanged (1 → 2) |
| Same deploy generation | 1 → 2 once | `min_dispatcher >= 2` | `min_dispatcher >= 2` — one bump covers both |

Neither blocks the other, and neither should wait. Whoever lands second
re-derives the number; §14.3's gate reads the *deployed* epoch live, so a stale
declaration fails CI rather than shipping.

**Surface 2 — the `Job` record.** `Job` carries no `deny_unknown_fields` and
every added field uses `#[serde(default)]`, so `inputs` follows the established
additive pattern: an old record deserializes with an empty map. The
old-binary-reads-new-record direction is bounded to a dispatcher restart (there
is one dispatcher, and it is the sole writer of `jobs.*`), which is the safe
direction. Additive, no bump of its own.

**Surface 3 — the create request.** `CreateJobBody`
(`crates/dispatcher/src/handlers/jobs.rs`) has no `deny_unknown_fields`, so an
N−1 dispatcher receiving `inputs` in a `POST` body silently drops it. The
`api` crate forwards the body opaquely as `serde_json::Value`
(`crates/api/src/routes.rs`), so an N−1 `api` does not strip it either. The
`min_dispatcher` gate on surface 1 covers this: the job type that declares
`inputs:` is the same one an N−1 dispatcher refuses, so a silently-unparameterized
job never reaches release.

**Surface 4 — the web UI.** An N−1 build renders no input fields, so a required
input is simply absent and release validation rejects the job with
`inputs.{name}`. Loud, no wrong action. No coordination needed.

## Minimum useful version

Three slices, each independently shippable and useful. **Slice A is the
recommendation for "what ships first"** — it is the `rollback` case, end to end.

**Slice A — command jobs, in-repo rollback.**

1. `Input` / `InputKind` in `crates/types/src/job_type.rs`, `inputs:` on
   `JobType`, `deny_unknown_fields` on the nested block; field rules in
   `validate()` (name shape, `values` required for `enum` and disallowed
   otherwise, `pattern` for `string` only, `default` disallowed with
   `required: true`, a declared `default` itself satisfying the charset and its
   own `pattern`/`values`, `inputs_count_max`).
2. The default charset and the `charset AND pattern` rule as a pure function in
   `types`, shared by all three passes.
3. `Job.inputs: BTreeMap<String, String>`, `#[serde(default)]`; `inputs` on
   `CreateJobBody`/`CreateJobRequest` and the Draft `UpdateJobBody`; creation-time
   shape check (422).
4. Release-time semantic check in `domain::release` → `ValidationError` with
   `field: "inputs.{name}"`; Ready-transition re-check at `base_ref`;
   launch-time charset re-check as a `work::EntryFailure` variant.
5. **Default materialization** in the Ready-transition write, against the type
   loaded at the `base_ref` that same write records: fill every declared input
   the creator did not supply, add-only, under the never-overwrite assert. This
   is what makes `Job.inputs` the effective set for the audit surfaces in step 9.
6. Extend the reserved-`CHUG_`-prefix rule from secrets to **vars** in
   `static_errors` and `container_env` (the prerequisite from correction 2).
7. `CHUG_INPUT_*` injection in `container_env` for work, wrap-up and eval
   containers — one key per entry in `Job.inputs`, no key for an unresolved
   optional input, inserted last, with the collision assert.
8. `CONFIG_SCHEMA_EPOCH` bump + the `inputs` ⇒ `min_dispatcher` rule in
   `validate()`, in one commit.
9. `job-created` carries the supplied inputs and the Ready-transition event the
   effective set; `chuggernaut validate` covers the new block;
   `.chug/jobs/rollback.yaml` and `.chug/tasks/rollback.sh` as the first
   consumer.

**Slice B — agent jobs.** The `### Inputs` block in `job_brief_block`, delivered
to the work agent and every agent evaluator; the squash-body `Inputs:` line;
the create-form rendering and the job-header display in `web/`.

**Slice C — schedules.** `inputs:` on the schedule file, passed through #310's
origination path unchanged. Blocked on #310 landing, and on nothing here.

Deferred, in rough priority order: `bool`/`int`/list kinds; `/chuggernaut/inputs.json`
for structured values; a "create N" client-side fan-out affordance; per-input
secret scoping (the thing that would collapse the `environment` axis); batch ×
inputs.

## Contracts this changes

Per CLAUDE.md's contract-first rule for dispatcher work:

| Contract | Change |
| --- | --- |
| `Job.inputs` | New field, `BTreeMap<String, String>`, empty on old records. Written by exactly two paths: create/Draft-update (supplied values) and the Ready-transition that first records `base_ref` (declared defaults, add-only). **Immutable thereafter**, including across later `base_ref` updates |
| Invariant | The Ready-transition default fill only inserts keys absent from `Job.inputs`; it never overwrites a supplied value. Asserted at the write site |
| Invariant | For any job type, any job, and any two input maps, the `JobType` resolved by the release path is **equal**. Inputs never influence config resolution (tier-1 property test) |
| Invariant | Every `CHUG_INPUT_*` key inserted by `container_env` was absent beforehand, and no other source inserts a `CHUG_INPUT_*` key. Asserted at the insertion site |
| Invariant | No secret **or var** name may start with `CHUG_` — release-validation error, and filtered again at injection (extends the §5.3 rule from secrets to vars) |
| Invariant | Every injected input value matches the default charset. Checked at creation, at release, and again immediately before injection |
| Invariant | `name.to_uppercase()` is injective over declared input names (guaranteed by the `[a-z][a-z0-9_]*` name rule) |
| `JobType::validate` | New field rules for the `inputs:` block; and a non-empty `inputs:` requires `min_dispatcher >=` the new epoch |
| `ValidationError` | New `field` values `inputs.{name}`, produced at release and at Ready-transition |
| `work::EntryFailure` | New variant for a launch-time input violation; parks like `MissingKv` rather than launching |
| Bound | `inputs_count_max = 16` declared inputs per type; `input_value_len_max = 256` per value. Both hard errors |
| Golden trace | `job-created` (supplied inputs) → `job-released` (effective inputs, after default fill) → `job-started` → the work container env contains exactly the keys for inputs with a **resolved** value — no key for a declared optional input that has neither a supplied value nor a `default`. Plus: a missing-required-input release rejected with `inputs.{name}`, and a job with no inputs producing an eval container env byte-identical to today's |
| Epoch | `CONFIG_SCHEMA_EPOCH` +1, in the same commit as the parser change (§14.1) |

New modules get a doc header (accepts / emits / guarantees / spec §) and a
`MODULES.md` registry row, per the direction-of-travel rule; `.chug/tasks/ci.sh`
enforces the registry.

## What this doc does not decide

- **Matrix / fan-out.** Excluded with reasons
  ([Decision 7](#decision-7-matrix--fan-out-is-excluded)), not deferred
  pending a later opinion.
- **Per-input secret scoping** — the mechanism that would collapse the
  `environment` axis of the twelve deploy types. It is a change to the secrets
  model (§8.2), not to inputs, and it should be argued there.
- **Job outputs.** design-lifecycle.md states "a job's output is its structured
  result, and downstream jobs' inputs resolve to it". Wiring a *dependency's*
  structured result into a dependent's inputs is a real and attractive feature,
  and it is a different one: it needs a resolution syntax, a timing rule, and a
  failure story for a missing key. Named here so it is not mistaken for an
  omission.
- **The schedule file format.** #310 owns it; this doc contributes only the
  `inputs:` field's shape and semantics.
- **Which epoch number lands.** Only the rule that inputs require one, and how
  it sequences with doc 1.
- **Anything in #308's other categories.** Image builds and workload identity
  are doc 4.
