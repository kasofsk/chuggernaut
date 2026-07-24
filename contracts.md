# Dispatcher contracts — formalizing the interfaces

Companion to `NORTH-STAR.md` (target factoring) and `structure-assessment.md`
(current-state audit). This document answers: what are the high-level
interfaces inside the dispatcher, which contracts exist today (explicitly or
implicitly), and how do we extract, formalize, and enforce them so they can
guide future implementation — in Rust or Python — toward the north star.

## The starting fact: between dispatcher modules, there are no interfaces today

The dispatcher's ~24 files are not modules with interfaces — they are
**namespaces over one shared object**. Eleven files contain `impl Core` blocks
(`core`, `exec`, `eval`, `channel`, `fleet`, `launch_queue`, `cd`, `origin`,
`reconcile`, `scan`, `triage`), each with full mutable access to all of
`Core`'s state. `eval` calling into `exec` is not crossing a boundary; it is
one object talking to itself. So the task is not "formalize the existing
interfaces" — it is "create them," and the extraction work below is how we
find out what they should be.

The code is far from contract-free, though. There is a clear formality
gradient:

**Already formal (keep — these are the model):**

- `state.rs::assert_transition` — transition legality as a total function,
  zero I/O.
- `core.rs::set_state` — a genuine single funnel: "§2.1 guard, then KV, then
  memory," with terminal-state stamping done once there.
- The ports: `ContainerBackend`, `AgentProvider`, `store`'s typed accessors.
- The wire surface: each `Msg` variant carries a doc comment naming its
  `req.*` subject and spec section.

**Semi-formal:**

- The `Msg` enum itself (25 variants) is the *actual* high-level interface of
  the entire dispatcher: every input — HTTP-originated request, container
  exit, timer tick — becomes a `Msg`. It is a protocol in all but name: shapes
  are typed, but pre/postconditions and error semantics ("409 while an attempt
  is in flight") live in comments and `CoreError` conventions.

**Implicit and at-risk:**

- The invariants. "Terminal states are absorbing." "The queue holds only
  Ready jobs." "One attempt in flight per job." "rdeps is the inverse of
  deps." "Merge queue is depth-1 per project." These exist as comments,
  defensive code (`get_or_insert_with`), and assertions scattered through
  ~12k lines of integration tests — nowhere as a checkable statement.

## The three contract vocabularies

The decision-making process for a job decomposes into three kinds of
statements; each wants a different artifact.

### 1. Commands and events — what can happen

Split `Msg` conceptually into **commands** (external requests carrying a
`Reply` — claim, release, resolve, …) and **events** (facts: `TaskExit`, scan
tick, ingest arrival). For each variant: precondition (which states/conditions
it is valid in), postcondition (state written, effects emitted), and error
taxonomy. Most of this is already written in the doc comments — it needs
pulling into one place. Because `Msg` mirrors the wire, this doubles as most
of the NATS protocol spec.

### 2. Effects — what the system does about it

The missing vocabulary, and mechanically extractable: the ~460 `.await` sites
in `eval`/`exec`/`core` *are* the effect catalog. Classify each await into a
variant — `PutJob`, `LaunchContainer`, `SquashMerge`, `CreateTask`,
`PublishEvent`, `IssueCredentials`, … likely 20–30 total — and the real
interface between "deciding" and "the world" is enumerated. Once the Effect
enum exists, the north-star decider signature falls out:

```
decide_<phase>(view of state, event) → (transitions, [Effect])
```

Each phase decider (ready, work, eval, merge_gate, wrapup, escalation,
triage) becomes a named module with a named contract. **That is the
inter-module interface structure the dispatcher currently lacks: modules stop
sharing `&mut Core` and start exchanging values.**

### 3. Invariants — what must always hold

Harvest every "must"/"always"/"never" comment and defensive pattern into one
list, then make it executable: a `check_invariants(&CoreState)` function run
after every message in tests. This is cheap precisely because of the
single-writer design — all state lives in one place. It converts tribal
knowledge into a regression net, and it is the artifact that survives a
language change untouched: invariants are statements about *data*, not code.

## Mining intent from the existing code

- **The await sites → the Effect enum.** Mechanical classification; days, not
  weeks.
- **The tests are the richest contract corpus, in the wrong form.** Each
  integration test encodes "given this setup, when this happens, these states
  and artifacts result." Convert the highest-value scenarios into **golden
  decision traces**: data files (YAML) of
  `{initial state, incoming event, expected transitions, expected effects}`.
  Generate them by instrumenting the Rust dispatcher (log every `Msg` in,
  every `set_state`, every effect out during a test run) rather than writing
  them by hand. The traces are the keystone artifact: language-neutral,
  diffable, and they serve both futures — in Rust they pin behavior during
  decider extraction; in Python they are the conformance fixtures. One
  artifact, both roads.
- **The doc comments with spec § refs → per-module contract headers.** The
  discipline already exists; it needs a home and a completeness check (the
  MODULES.md registry rows should point at these).
- **Serialize the vocabulary itself.** `Msg`, the Effect enum, `JobState`,
  and the KV record shapes are all serde types — emit JSON Schema from them
  and the formal interface definition becomes a build artifact, not a document
  that drifts. (`NORTH-STAR.md` §2, applied inward.)

## The formalization ratchet — increasing strength

1. **Documented** — contract headers per module: accepts / emits /
   guarantees / spec refs. Cheap; do immediately.
2. **Type-enforced** — deciders take a read-only view of job/graph state, not
   `&mut Core`; `Core`'s fields go private and the `impl Core` sprawl shrinks
   as each phase migrates. (Python equivalent: `Protocol`s + frozen
   dataclasses — same shape, weaker teeth, which is why layers 3–4 matter
   more there.)
3. **Executable** — the invariant checker after every message; property tests
   (random legal command sequences never violate invariants — the
   single-writer loop makes these easy to drive); golden traces in CI.
4. **Cross-implementation** — the wire-level conformance suite and
   shadow-mode decision diffing (see the Python-rewrite assessment): a second
   implementation's deciders consume the same event stream read-only and
   their `[Effect]` output is diffed against what the live dispatcher did.

## The working rule that makes it stick

**Contract-first change rule:** any job touching the dispatcher must name the
contract it changes — a `Msg` pre/postcondition, an Effect, an invariant, a
trace. If a change cannot be expressed that way, the contract it needs does
not exist yet, and writing it becomes the first commit of the job. This rule
is also precisely what makes module-scoped agent jobs safe: **the contract is
the scope.**

## Sequencing (no big-bang)

1. **Invariant checker + contract headers** — days, pure gain, zero
   restructuring.
2. **Effect catalog** from the await sites.
3. **Trace-generation instrumentation**; goldens for the lifecycle tests.
4. **First decider extraction** (`merge_gate` or `escalation`) behind those
   traces.
5. **Schema emission** for the vocabulary types (`Msg`, Effect, records).

After step 4 there is one fully-formalized phase to serve as the template,
and each subsequent job can convert another.
