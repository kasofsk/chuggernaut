# Design #529 — Secret handling: the declarative model's edges, and the platform token's reach

Status: PROPOSED — nothing below is built, and S2 is gated on a measurement.

Written against the tree at `927067b` (2026-08-10). Every claim about current
behaviour was read out of the source named beside it rather than out of a
sibling design doc **or out of this job's own brief**; where the two disagree the
tree wins, and the disagreement is in
[Corrections](#corrections-verified-against-the-tree). Findings are marked
`M1a`–`M5` and each says where it was established: most inside **this job's own
work container** — a Debian 12 Linux container on the production fleet running
`claude` 2.1.226, `JOB_TYPE=design`, `CHUG_PHASE=Work` — and the rest out of
this repo's own source. The brief asked for a measurement rather than an assumption
about the agent CLI's credential sources;
§[3](#3-decision-1-what-the-measurement-says) is that measurement, one row of it
is deliberately **unverified**, and between them they change the answer.

Two of those rows are new in this revision and both were assumptions before. **M1d**
settles the premise every slice here rests on — that a credential in the agent
CLI's memory is out of the task's reach — by measuring it from a shell the CLI
spawned rather than inheriting the brief's word for it: it holds, and it holds
because of a **host sysctl this platform does not set**, which is why it now
ships a slice (M6) instead of a sentence. **M8** replaces "small and knowable"
with two numbers for the `global/agents` scope. Both are container-mode findings;
M7 records that the host path is unmeasured rather than implying it is covered.

## Current state

*This section is the mutable head: it is rewritten to current truth whenever
anything below it changes. Everything after the horizontal rule is append-only —
the argument and its dated corrections, never edited
([#415](415-knowledge-architecture.md) D2).*

| Fact | Where | State |
| --- | --- | --- |
| A level receives only the secret names **it** declared | `container_env`, [`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs) | **Landed.** Verified at all four call sites |
| Reserved-prefix names cannot be shadowed by a declaration | `reserved_env_prefix`, same file | **Landed** |
| The NATS user JWT is minted per task, TTL = the task's resolved timeout | `mint_creds`, [`crates/auth/src/nats.rs`](../../crates/auth/src/nats.rs) | **Landed** |
| The SSH key and certificate are minted per task at the same TTL | `ssh_credential_files`, `exec.rs` | **Landed** |
| A workload token is minted per container, TTL-capped | [`docs/spec.md`](../spec.md) §8.3, [#313](313-workload-identity-image-builds.md) | **Landed** (epoch 5, proved in job #430) |
| Declared secrets, project `vars` and `global/agents` carry a TTL | — | **No.** Injected verbatim; lifetime is rotation discipline |
| `global/agents` is narrowed to what the agent CLI needs | `inject_platform_agent_secrets`, `exec.rs` | **No.** The *whole scope* reaches every agent launch |
| The platform's provider token is out of the task's reach | — | **No.** Env-delivered, and the env is readable for the process's life (§[3](#3-decision-1-what-the-measurement-says)) |
| A credential in the agent CLI's *memory* is out of the task's reach | Yama `ptrace_scope`, the node's kernel | **True today, and not by this platform's doing.** Measured `1` in a work container (M1d); a node sysctl, unasserted — M6 |
| Credential-bearing payloads are kept out of argv | `claude_invocation`, [`crates/agent/src/claude.rs`](../../crates/agent/src/claude.rs) | **Landed**, with a test asserting it |
| The same reasoning is applied to the env | — | **No.** That asymmetry is what D3 names |
| Injected files are deleted at teardown | `remove` / `reclaim_credentials`, [`crates/container/src/host.rs`](../../crates/container/src/host.rs); `docs/spec.md` §3.1 | **Landed**, on both backends |
| Any artifact is redacted, ever | [`crates/store/src/artifacts.rs`](../../crates/store/src/artifacts.rs) | **No.** Zero redaction anywhere in the tree |
| File delivery has plumbing | `InjectedFile`, [`crates/container/src/lib.rs`](../../crates/container/src/lib.rs) | **Landed** — used by SSH and by #313 |
| File delivery has a *declaration* | [`crates/types/src/job_type.rs`](../../crates/types/src/job_type.rs) | **No.** That is S3, and it costs an epoch |

**The declarative core is done and this design does not touch it.** What is left
is four edges: one blanket grant that is wider than its purpose, one credential
whose delivery mechanism is the weakest one available, one class of value with
no lifetime bound and no mechanism that could give it one, and one durable sink
that no cleanup path reaches.

## Decisions

- **D1. Reach and lifetime are two axes, and every proposal below states which
  one it moves.** *Reach* is who can read a value while the task runs; *lifetime*
  is how long the value is worth stealing. A declaration bounds the task, never
  the code inside it — children inherit the environment, and an agent job exists
  to run project code — so nothing here makes a secret unreadable by the task it
  was given to. Conflating the two axes is how a design ends up asserting a bound
  it has no mechanism for, which is the defect this document exists to close.

- **D2. `global/agents` is narrowed by name, not by launch.** The brief's cheap
  half proposed narrowing *who receives* the grant. Measured, all three receivers
  — work agents, agent evaluators and forge-ingest triage — exec the same agent
  CLI and all three legitimately need a provider credential, so the launch axis
  buys nearly nothing. The actual over-grant is on the other axis: the injector
  lists the **entire scope** and injects every name in it. S1 — written
  throughout as one name for the observe-then-enforce pair **S1a** and **S1b** —
  replaces the scope listing with a platform-declared provider-credential name
  set.

- **D3. Within one uid, no delivery mechanism is a boundary — but the env is
  strictly the worst of them, for a reason that is not permissions.** A file at
  `0600` is readable by the task's own code, and so is a file descriptor, and so
  is anything else the same uid can reach. What is different about the
  environment is that it **cannot be withdrawn**: `unset` does not clear the
  kernel's view of a process's environment (M3), so an env-delivered credential
  is readable by anything that can read that process for the whole life of the
  process, no matter what the process does afterwards. A source the consumer
  reads once and the platform then removes has a *window* instead of a lifetime.
  That is a real reduction and it is not a boundary; say both.

  **This rests on one measured premise, and the premise names a mechanism.** The
  window is only smaller than the lifetime if the credential, once in the CLI's
  memory, is out of the task's reach. Measured (M1d), it is: Yama at
  `ptrace_scope=1` denies a descendant `PTRACE_MODE_ATTACH` on an ancestor, so
  `/proc/<cli-pid>/mem` is `EACCES` to the task while `/proc/<cli-pid>/environ`
  is not. **That is a host-kernel setting, not something this platform
  establishes**, so D3 holds exactly as far as M6 (assert it) holds, and only in
  container mode until M7 answers the host path.

- **D4. What credential sources the agent CLI accepts is a measurement, and the
  measurement is half-taken.** §[3](#3-decision-1-what-the-measurement-says)
  establishes the current behaviour exactly, and finds a named fd-delivery source
  in the shipped bundle whose semantics are **not** established. M2 is the
  experiment that settles it, and S2 is gated on M2 rather than assuming it.

- **D5. Decision 1 as written is satisfiable today only by a boundary this
  design does not build.** "The task must not be able to read the file holding
  it" needs either a uid boundary (deferred by #526, designed for projects by
  [#537](537-per-project-users-macos.md)) or a credential the task never holds
  at all (a proxy — §[4](#4-the-options-for-decision-1)(e)). S2 delivers the
  *window* reduction, honestly labelled as that. Naming this is the decision:
  the alternative is shipping S2 and calling decision 1 met.

- **D6. `vars` stays a side door, and gets a warning rather than a gate.** A
  secret placed in `vars` is worse than one placed in `secrets` in three
  measurable ways (§[7](#7-vars-is-a-side-door-and-it-is-worse-than-the-brief-says)),
  but no mechanical rule can tell a secret value from a var value — the same
  residual [#313 A5](313-workload-identity-image-builds.md#the-globalagents-hazard-stated-as-a-rule)
  names for `global/agents`. S5 warns on secret-shaped **names** at write time
  and never blocks.

- **D7. Artifacts are the durable leak, they are out of scope, and they are a
  row in the slice table rather than a footnote.** Decision 2's guarantee covers
  what the platform placed. It does not cover what a task echoed into
  `stdout.log`, what a tool logged into `session.jsonl`, or what a work container
  wrote into `output.tar.gz` — three kinds, not the two the brief names — and
  none of them is redacted anywhere. S4 is a design of its own.

- **D8. #313's minted-credential pattern generalises where the platform or a
  federated provider owns the downstream, which is none of the three untimed
  classes as they stand.** Two named secrets in this repo could move, by two
  mechanisms, **neither of which is #313**
  (§[6](#6-does-313-generalise)). Recording that is the result.

## Slices

| # | What | State |
| --- | --- | --- |
| **S1a** | Log, by name, every `global/agents` name `inject_platform_agent_secrets` would decline under S1b's set — while still injecting all of them | Proposed — observe-only; one release, so S1b excludes nothing a run depends on |
| **S1b** | Narrow the injector from "every name under `global/agents`" to a platform-configured provider-credential name set, injecting nothing else | Proposed — after S1a; no schema field, no epoch, moves *reach* |
| **M2** | Measure whether the agent CLI authenticates from an inherited fd (`CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`) with no token in the env | Proposed — a **measurement**, and S2's gate |
| **S2** | Deliver the provider credential by the narrowest source M2 establishes and stop putting it in the launch env; fall back to §[4](#4-the-options-for-decision-1)(d) if M2 fails | Proposed — gated on M2, ships with M6, moves *reach* |
| **M6** | Assert `ptrace_scope` (and the absence of `CAP_SYS_PTRACE`) at launch instead of assuming it — the host-kernel property S2's window rests on (M1d) | Proposed — ships **with** S2; without it S2 asserts a bound whose mechanism it never checks |
| **M7** | Measure M1c/M1d's equivalents on the **host** node path, where there is no `/proc` and `task_for_pid` governs | Proposed — a **measurement**; until it lands, S2's window claim is container-mode only |
| **S3** | A per-level file-delivery declaration for project secrets, over the existing `InjectedFile` plumbing, using the `{NAME}_FILE` convention `.chug/tasks/deploy.sh` already anticipates | Proposed — **costs an epoch bump**, moves *reach* |
| **S4** | Artifact redaction | Proposed — **out of scope here**; needs its own design, and D7 forbids implying it is covered |
| **S5** | A secret-shaped-name warning when a `var` is written | Proposed — advisory, never a gate |

Nothing here moves the *lifetime* axis, and that is a finding rather than an
omission: §[6](#6-does-313-generalise) is the argument that no mechanism
available to this platform shortens the lifetime of a forwarded secret, and
inventing a slice that claimed to would be exactly the defect
[#322](322-macos-native-runtime.md)'s open item recorded.

---

## 1. The four classes, re-read out of the tree

[#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host) enumerates
what the dispatcher puts into a launch env, and
[#322](322-macos-native-runtime.md)'s open item sorts them by whether they carry
a TTL. Both readings hold. Re-derived here from the source, with the call sites,
because a class list inherited rather than checked is how this document would
repeat the failure it is about.

| Class | Where it comes from | Bound |
| --- | --- | --- |
| NATS user JWT (`NATS_CREDS`) | `container_env` calls `mint_creds` with `creds_ttl` ([`crates/auth/src/nats.rs`](../../crates/auth/src/nats.rs)) | **Minted per task**, expiring at the task's resolved timeout |
| SSH key + certificate | `ssh_credential_files` issues a fresh key and cert at the same TTL | **Minted per task**, and delivered as files, not env |
| Workload tokens | `workload_delivery` per container, `min(task_timeout, token_ttl_secs, 3600s)` (`docs/spec.md` §8.3) | **Minted per container**, capped |
| Declared `work.secrets` / evaluator `secrets` / `wrap_up.secrets` | age-decrypted from KV and inserted verbatim | **None** |
| Project `vars` | plaintext KV, inserted verbatim | **None**, and see §[7](#7-vars-is-a-side-door-and-it-is-worse-than-the-brief-says) |
| `global/agents` | `inject_platform_agent_secrets`, whole scope | **None** |

**The declarative half is exactly as the brief describes it, and this is the
part that needs no work.** All four command launches route through
`command_launch_config` ([`crates/dispatcher/src/launch_queue.rs`](../../crates/dispatcher/src/launch_queue.rs)),
and each passes only its own level's list: `&job_type.work.secrets` at
`exec.rs`, `&evaluator.secrets` and `&job_type.wrap_up.secrets` at
[`crates/dispatcher/src/eval.rs`](../../crates/dispatcher/src/eval.rs). Nothing
inherits from another level, and `reserved_env_prefix` filters the `CHUG_` and
`JOB_` namespaces out of both the `vars` loop and the secrets loop before either
inserts, so no declaration can shadow a platform-composed value — which is what
makes the node-side allow-lists that match on `JOB_PROJECT` and `JOB_TYPE`
(`docs/spec.md` §3.1, [#517](517-docker-access-for-jobs.md)) trustworthy.

**One class is delivered by files already, and it is the oldest one.** The SSH
credential has been an `InjectedFile` at `0600` with an env var *pointing at it*
since long before #313 argued the pattern; `GIT_SSH_COMMAND` names the paths and
carries no secret. So "declare it, scope it, deliver it as a file, clean it up"
is not a proposal in this tree — it is the shape two of the three minted classes
already use. What has never been done is applying it to the *forwarded* classes.

## 2. Reach and lifetime, and why they must not be merged

The two axes answer different questions and have different mechanisms:

- **Lifetime** is shortened by minting: a value that expires is worth less when
  it leaks. Every mechanism on this axis needs the downstream's cooperation —
  something must be willing to issue and to verify a short-lived credential.
  This is #313's axis, and §[6](#6-does-313-generalise) sizes how far it reaches.
- **Reach** is narrowed by *not putting the value where things can read it*.
  This axis needs nothing of the downstream, which is why every slice in this
  document is on it.

The confusion the brief warns about is real and has a worked example in this
repo: `.chug/tasks/deploy.sh` receives `MINI_DEPLOY_KEY` as an env var and, in
its first ten lines of work, writes it to a `0600` tempfile so `ssh -i` can use
it. Anything reasoning about "the secret is in the environment" has already lost
the value by the time the ssh runs — it is in the environment *and* on the disk,
under a `trap` the platform does not own. That is decision 2 operating exactly
as decision 2 says it does, in the platform's own repo, and it is why no slice
here claims to bound what a task does with what it was given.

## 3. Decision 1: what the measurement says

The brief asks for the agent CLI's credential sources to be **established, not
assumed**, and says to name it as a measurement if it cannot be established from
a work container. It can be established, partly, and the part that can changes
the recommendation.

M1a–M1d and M3, and the bundle half of M2, were taken **inside this job's own
work container**, against the `claude` the platform actually launched and from a
shell that CLI spawned — which is precisely the relation a task's own code stands
in; M4's `--help` text came from the same container and its other half, like M5,
is a reading of this repo's source. Every one of them is a **container-mode**
finding and is labelled as one. None of them transfers to a host task by
argument: that path has no `/proc`, so both the read that succeeds here and the
read that fails here are open questions there (M7, below).

| # | Question | Verdict | What answered it |
| --- | --- | --- | --- |
| **M1a** | Is the platform token in the CLI process's environment? | **Yes** | `CLAUDE_CODE_OAUTH_TOKEN` appears in `/proc/<cli-pid>/environ` |
| **M1b** | Is it in the environment of a process the CLI spawns? | **No** | A full name-set diff between `/proc/<cli-pid>/environ` and a spawned shell's own environment differs by **exactly one** name — the token's. `NATS_CREDS` and every platform-composed `JOB_*` / `CHUG_*` value pass through unchanged. (This job type declares no secrets and no vars, so those two were not in the sample) |
| **M1c** | Can the task's own code read the CLI's *environment* anyway? | **Yes** | Reading `/proc/<cli-pid>/environ` **from that spawned shell** returns it. Same uid (both processes are uid 0), and `environ` is gated by `PTRACE_MODE_READ`, which Yama does not restrict at any scope — so the ptrace-scope setting M1d finds is not in the way of *this* read |
| **M1d** | Can the task's own code read the CLI's *memory*? | **No — and this is the premise D3 rests on** | `/proc/sys/kernel/yama/ptrace_scope` is **1**. From the spawned shell: `open("/proc/<cli-pid>/mem")` → `EACCES`, `PTRACE_ATTACH` → `EPERM`. The **same shell**, against a child it forked itself, gets `open` → OK and `PTRACE_ATTACH` → OK. Same uid in both directions, so the syscall is not seccomp-blocked and the denial is Yama's directionality: `mem` needs `PTRACE_MODE_ATTACH`, which under scope 1 requires the reader be an **ancestor** of the target, and the task's shell is a *descendant*. `CAP_SYS_PTRACE` is absent from `CapEff` (docker's default drop), so uid 0 does not bypass it |
| **M2** | Does the CLI accept the token from a source the platform can withdraw? | **UNVERIFIED — this is the measurement S2 is gated on** | The shipped bundle contains a credential-source resolver keyed on an env var named `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, with a sibling for the API key and a well-known-path fallback. This is a **name-level** reading of a minified third-party bundle: it establishes that the string is there and nothing about whether the path works |
| **M3** | Does a process removing a variable from its own environment remove it from what others can read? | **No** | A shell and a Python interpreter each unset a variable and then read `/proc/self/environ`: the value is still there in both. The kernel serves the memory region the process was started with, which `unset` and `unsetenv` do not touch |
| **M4** | Is the CLI already launched with a settings file the platform controls? | **Yes** | `claude_invocation` passes `--settings` at a fixed injected path, and `--bare`'s own help text names `apiKeyHelper` via `--settings` as an accepted Anthropic auth source |
| **M5** | Is argv already treated as a leak surface here? | **Yes** | The **module header** of [`crates/agent/src/claude.rs`](../../crates/agent/src/claude.rs) says argv "leaks into `ps`, `/proc/*/cmdline`, and crash reports"; `claude_invocation`'s own doc comment says the MCP payload "carries the NATS credential, which must never enter argv" and it is injected as a `0600` file for that reason; `assert_no_creds_in_argv` has three tests over it. `--settings` is likewise passed as a path to a fixed injected file |

**M1b is the surprise and it must not be over-read.** The CLI removes exactly
this one name from what it hands a child. That is a third party's behaviour at
one version, not a platform guarantee: it is not documented in `--help`, nothing
in this repo asks for it, and a version bump could withdraw it silently. It also
protects only the *direct child's environment*, which M1c shows is not the path
that matters.

**M1c is the finding.** The task's own code reads the credential out of the
parent CLI without needing a file, a permission, or the environment the CLI
handed it. So the brief's framing — *env is readable, so deliver by a file* — is
right about the conclusion and wrong about the reason: the file is not safer
because of permissions, it is safer because it can be **taken away**.

**M1d is what says "taken away" means anything, and it was very nearly assumed.**
Every slice that moves the provider credential out of the env rests on one
premise: that once the CLI has read the credential into its own memory, the task
cannot get it back out. The brief asserts it — *a child process cannot read its
parent's memory* — and asserting it unmeasured would have been this document
committing the exact defect it exists to close. Measured, the premise **holds in
container mode**, and the mechanism is nameable: Linux gates `/proc/<pid>/mem`
and `PTRACE_ATTACH` behind `PTRACE_MODE_ATTACH`, and Yama at `ptrace_scope=1`
grants that only to an **ancestor** of the target. The task's processes are all
descendants of the agent CLI, which is the one direction the setting denies. The
control experiment matters as much as the result: the same shell ptraces a child
it forked itself, so nothing here is a seccomp block or a missing capability
masquerading as a boundary.

**But it is a host-kernel property, not a platform guarantee, and the difference
is the whole caveat.** `ptrace_scope` is a node-wide sysctl that an operator or
a base image can set to `0`, and `CAP_SYS_PTRACE` is absent only because the
docker backend adds no capabilities and inherits the engine's default drop
([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs) sets
no `CapAdd`, no `Privileged` and no `SecurityOpt`). Either knob flips M1d, and if
it flips, the window in D3 collapses back to the process's lifetime and options
(c) and (d) stop buying what §[4](#4-the-options-for-decision-1) says they buy.
The platform does not currently read either knob. **So S2 carries M6: assert
`ptrace_scope` at launch rather than assume it** — a bound whose mechanism lives
in someone else's sysctl is exactly the kind this document refuses to assert
silently.

**The host-node path is not covered by any of this and is not measured.** M1a–M5
are container-mode findings. A host task ([#322](322-macos-native-runtime.md),
[#490](490-agent-work-on-a-mac.md)) runs the same CLI under a login user's uid on
macOS, where there is no `/proc` at all and cross-process memory reads go through
`task_for_pid`, restricted by a different mechanism entirely. Whether the
equivalent of M1c succeeds there and whether the equivalent of M1d fails there
are **both open** — M7 — and until they are answered, S2's window claim is
stated for container mode only.

**M3 is what makes that concrete, and it is the argument D3 rests on.** A
process cannot un-publish its own environment. So the exposure window of an
env-delivered credential is the *lifetime of the agent process* — the whole
task — and no cooperation from the CLI can shorten it. A credential the CLI
reads from a pipe and the platform closes is exposed for as long as it takes the
CLI to read it. Neither is a boundary against the task, and how much smaller the
second window is has not been measured; what M3 establishes is that the first one
cannot be made smaller at all.

**M5 is the asymmetry this design exists to name.** The tree already refuses to
put credentials in argv, for `/proc/*/cmdline`, with a test enforcing it. The
identical reasoning applied to `/proc/*/environ` is written down in
[#313 A3](313-workload-identity-image-builds.md#delivery-an-injected-file-not-an-env-var)
reason 2 — and has never been applied to the env, which carries strictly more
secrets than argv ever did.

### What still cannot be established from here, and how to establish it

M2 is the gate, and the experiment is small and bounded:

1. In a work container, run the CLI with `CLAUDE_CODE_OAUTH_TOKEN` **unset** and
   the token written to a pipe whose read end is inherited at a known fd, with
   the fd number in `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`. A run that
   produces model output authenticated from a withdrawable source.
2. Confirm the source is actually withdrawn: after the CLI has authenticated,
   the write end is closed and the token is present in no file, in no process's
   environment, **and in no process's memory the task can read** — the third
   clause being the one M1d supplies and the one an earlier draft of this
   document left implicit. Re-run M1d's two probes in that same container rather
   than citing this one, because it is a per-node setting.
3. Re-run the same shape with `apiKeyHelper` in the settings file the platform
   already injects (M4), as the documented alternative.
4. Record the CLI version, because M1b shows this class of behaviour is
   version-bound. A platform depending on it needs a startup assertion, not a
   comment.

**If M2 fails, S2 falls back to (d) below rather than to nothing.** A design that
proposed only the mechanism it hoped for would leave the failure branch
undesigned, which is the thing the brief asks this document not to do.

## 4. The options for decision 1

- **(a) Accept and document.** What the platform does today. Refused by the
  operator's decision 1; recorded because every option below is measured against
  it.
- **(b) A file at `0600`.** The brief calls this out and it is right to: the
  agent CLI runs as the task, so the task reads the file. On its own this
  changes nothing about reach. **Rejected as an answer**, but note it is not
  useless — combined with removal after read it becomes (c), and combined with a
  uid boundary it becomes a real one.
- **(c) A withdrawable source: an inherited fd, read once, closed.** Delivers
  the credential to the CLI's memory and leaves nothing behind — no env entry
  the kernel keeps serving (M3), no file on disk, no argv. The task can still
  read the fd during the window, and a task that starts before the CLI has read
  it can win the race. **What makes the residue actually residue is M1d**: with
  `ptrace_scope=1` the task cannot recover the value out of the CLI's memory
  afterwards, so "read once and closed" is a window rather than a rename of the
  same lifetime. On a node where that sysctl is `0`, (c) degrades to exactly (a)
  with extra steps — which is why it ships with M6 rather than on trust.
  **Recommended, contingent on M2, and asserting M6 at launch.**
- **(d) A node-side helper: `apiKeyHelper` invoking a command the daemon
  provides.** Keeps the durable credential off the task's filesystem and out of
  its env; the task can still invoke the helper for as long as it runs. This is
  a genuine gain — what leaks is *use while the task is alive* rather than *a
  token that outlives it* — and it converts nothing on the lifetime axis unless
  the helper can return something bounded, which for this provider it cannot
  (§[6](#6-does-313-generalise)). It shares (c)'s dependency on M1d — what the
  helper returns lands in the CLI's memory like anything else — so it ships with
  M6 too. **The fallback if M2 fails**, and it uses M4's already-injected
  settings file, so its plumbing cost is one JSON key.
- **(e) A model-traffic proxy in the worker daemon.** The CLI is pointed at a
  local endpoint that the daemon serves, and the daemon holds the credential and
  attaches it. This is the **only shape that satisfies decision 1 as written**:
  the token is in no file and no environment the task can reach, because it is
  in another process the task cannot read. It is also much the most expensive —
  the daemon acquires an authenticating HTTP surface, streaming, and the
  per-task authentication problem (any container on the node can reach a daemon
  port, so the proxy needs to know which task is calling, and the obvious
  credential for that is in the caller's env). **Named as the target if the
  guarantee is wanted unconditionally; not this design's slice**, and it should
  not be started before M2, because (c) may make it unnecessary for most of the
  value.
- **(f) A uid boundary.** [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)(b)
  for a Linux host node, [#537](537-per-project-users-macos.md) for macOS.
  **Not re-litigated here** — #526 decided it and #537 designs it — but it must
  be named, because it is the thing that turns (b), (c) and (d) from window
  reductions into boundaries. Every option above is a same-uid palliative until
  one of those lands.

**The honest ordering:** (c) if M2 holds, else (d), both carrying M6; (e) or (f)
is what actually closes it; and S2 must be described as the window reduction it
is, in the commit message and in this document's head, or the next reader will
inherit decision 1 as met.

**What M1d changed about this ordering, recorded because it nearly went the
other way.** Had the measurement come back the other way — `ptrace_scope=0`, or
the container carrying `CAP_SYS_PTRACE` — then (c) and (d) would both reduce to
"removes the zero-effort read": still worth doing, because reading
`/proc/<pid>/environ` is a one-line shell command and scraping a process's heap
is not, but a change in *effort* rather than in *reach*, and (e) would become the
only option that changes the answer at all. That branch is not this tree's
branch, so (c) keeps its recommendation — but the ranking is downstream of a
sysctl, which is the reason M6 is a slice and not a note.

## 5. The cheap half, and why it is a different cut than the brief proposed

`inject_platform_agent_secrets` lists **every** name under `global/agents` and
inserts each into the launch env, with `entry().or_insert()` so a declared secret
of the same name wins. Its three callers are the work agent launch (`exec.rs`),
the agent evaluator (`eval.rs`) and forge-ingest triage
([`crates/dispatcher/src/forge_ingest/triage.rs`](../../crates/dispatcher/src/forge_ingest/triage.rs)).
Command containers deliberately receive none of it.

**The brief proposes narrowing the receiver list, and measured, that buys
little.** All three receivers exec the same agent CLI, and the CLI needs a
provider credential to do anything at all — an evaluator that cannot authenticate
is not a cheaper evaluator, it is a broken one. Triage is the only receiver whose
need is arguable and it is a full agent run creating jobs from forge events
(`docs/spec.md` §13); it needs the credential for the same reason.

**The over-grant is on the name axis, and it is the one #313 A5 already named as
a hazard.** The injector's contract is "the agent CLI's own plumbing", but its
implementation is "the whole bucket prefix". Nothing stops the scope from
holding more than the provider credential, and
`chuggernaut admin secret copy --to global/agents` is a documented one-line
operation
([`crates/cli/src/admin.rs`](../../crates/cli/src/admin.rs)) whose whole purpose
is to put things there. The platform already knows the names — the settings page
serves them to platform admins via `platform_config_get`
([`crates/api/src/routes.rs`](../../crates/api/src/routes.rs)) — so an operator
can see the gap today and has no mechanism to close it.

**S1: inject only the names the provider needs.** A platform-level configured
set, defaulting to the credential names the agent providers actually read, with
everything else under the scope **not** injected and the decline logged by name.
Reach shrinks from "every secret an operator ever copied into the scope, in every
agent container on the platform" to "the credential the CLI reads". No schema
field, no epoch, no wire change — the set is platform config, not project config,
so no `.chug/jobs/*.yaml` moves and no `min_dispatcher` question arises.

**S1 is a widening of a filter that already exists, which is most of why it is
cheap.** The injector already declines one name class unconditionally: a name
beginning `RESERVED_SECRET_PREFIX` — `CHUG_`, defined in
[`crates/dispatcher/src/forge_ingest/origin.rs`](../../crates/dispatcher/src/forge_ingest/origin.rs)
— is skipped on both the `SecretStore` and raw-bucket paths. So the shape "list
the scope, skip what does not belong, insert the rest" is the code that is there;
S1 replaces a prefix test with a membership test and adds the decline log.

**M8 — what the default set is, and what is under the scope today.** Two
numbers, because "small and knowable" is an assertion everywhere else in this
document is a measurement:

- **The default set is `CLAUDE_CODE_OAUTH_TOKEN` and `ANTHROPIC_API_KEY`,
  and the platform reads neither name itself.** Swept for it: no file under
  `crates/agent/` reads any provider credential from the environment — the CLI
  does, and the platform only forwards. So there is **no in-tree list to derive
  the default from**, and S1 must introduce the first one. `ANTHROPIC_API_KEY`
  earns its place in the default not from the launch path but from
  [`crates/api/tests/config_settings.rs`](../../crates/api/tests/config_settings.rs),
  which uses it as the worked example of a `global/agents` name. Exactly one
  provider is implemented — `ClaudeProvider`; `CodexProvider`
  ([`crates/agent/src/codex.rs`](../../crates/agent/src/codex.rs)) is a stub with
  a `TODO` and no launch path — so the set is small today, and the mechanism is
  what keeps it honest when a second one lands and brings its own names.
- **The scope contributed exactly one name to this job's own launch, at
  2026-08-10.** Measured, not assumed: `/proc/1/environ` in this work container
  is the launch env the dispatcher composed (pid 1 is the `sh -c claude …`
  entrypoint), and it holds 34 names. Every one is accounted for by the image's
  own `ENV` (the toolchain paths), by the platform's composed values (`JOB_*`,
  `CHUG_*`, `NATS_*`, `REPO_URL`, `BASE_BRANCH`, `GIT_SSH_COMMAND`,
  `CHANNEL_ROLE`, `CLAUDE_CONFIG_DIR`) or by the shell — leaving
  `CLAUDE_CODE_OAUTH_TOKEN` as the sole `global/agents` contribution. This job
  type declares no `secrets:` and no `vars`, so nothing masked the sample.

**So S1's measured blast radius on this deployment today is zero, and that is a
reason to sequence it rather than a reason to skip the sequencing.** One name in
the scope means the enforcing version excludes nothing and breaks nothing *now*;
it also means the number is one operator `admin secret copy --to global/agents`
away from being wrong, and the design cannot see the scope of any other
deployment. **S1 therefore ships in two steps**: first the decline log while
still injecting everything, so a release's worth of real launches proves the
configured set is complete against whatever an operator has actually put there;
then the exclusion. The observation step costs one log line and is the only way
to learn that some run depends on a name nobody wrote down — an MCP server
credential, a provider base-URL override — before the enforcing step removes it.
An operator can already enumerate the scope by hand, since `platform_config_get`
([`crates/api/src/routes.rs`](../../crates/api/src/routes.rs)) serves the
`global/agents` names to platform admins; what they cannot do today is find out
which of those names anything *reads*, and that is what the decline log adds.

The residual, stated: this makes the *supported* over-grant impossible and
leaves the unsupported one — an operator can still name their extra secret
`ANTHROPIC_API_KEY`. That is the same residual #313 A5 records for cloud
identities, resolved the same way: the mechanism has no path there, and no
mechanism can tell one opaque string from another.

## 6. Does #313 generalise?

[#313](313-workload-identity-image-builds.md) half A replaced a stored cloud key
with a minted, TTL-bounded token, proved end to end against a real provider
(job #430), deployed at epoch 5. Asking whether that generalises is the right
first question, and the answer is narrow.

**What it needs of a downstream: the downstream must accept a token it did not
issue, on the strength of a signature it can verify.** #313's whole hard part is
[A4](313-workload-identity-image-builds.md#a4-the-public-reachability-problem-the-crux)
— an STS validating our signature needs to fetch our JWKS — and the mechanism it
built is an OIDC issuer. So the pattern reaches exactly the downstreams that
speak a federated token exchange, and no others.

Measured against what this repo actually declares — **two** distinct secret
names, across six declarations in `.chug/jobs/`:

| Secret | Downstream | Can it be minted? |
| --- | --- | --- |
| `MINI_DEPLOY_KEY` | an sshd on a Mac the platform ships to | **In principle, and not by #313.** The platform already runs an SSH CA that mints per-task certificates at the task's timeout (`crates/auth/src/ssh.rs`). Adopting it needs that host to trust the CA — a change on a machine outside this repo, and an operator action, not a slice |
| `DEPLOY_HEALTH_API_TOKEN` | **the platform's own API** | **Yes, and not by #313 either.** The platform is both issuer and verifier here; `crates/auth/src/jwt.rs` already exists. A task-scoped, timeout-bounded bearer token is the same shape §7.4 already applies to NATS and SSH |
| `global/agents` provider credential | a third-party API, via a third-party CLI | **No.** The CLI's documented direct-mode credential sources are a long-lived key or an OAuth token (M4); nothing federated. Routing model traffic through Bedrock or Vertex would make it a *cloud* credential and therefore #313-shaped, but that is a platform-level decision about where inference runs, far outside this design |

**So: the pattern generalises where the platform or a federated provider owns
the downstream, and that is none of the three untimed classes as they stand.**
Both adoptable cases are the platform talking to itself or to its own hardware,
by mechanisms it already has and that are *not* #313. Everything genuinely
third-party — which is what a project's `secrets:` list is for — has no
short-lived form reachable this way, and a project's third-party API key may have
no short-lived form at all. That is a result, not a failure, and it is why the
slice table moves nothing on the lifetime axis: **no bound is asserted, because
no mechanism exists to assert one with.**

## 7. `vars` is a side door, and it is worse than the brief says

The brief says nothing stops a secret living in `vars` and no secret rule would
cover it. Both true, and the tree makes the asymmetry sharper than "plaintext":

1. **It is stored unencrypted.** `secrets.*` values are age ciphertext
   ([`crates/store/src/secrets.rs`](../../crates/store/src/secrets.rs));
   `vars.*` is plaintext KV, and `docs/spec.md` §1.5's bucket table gives it no
   TTL — "Plaintext config; permanent", the same row the encrypted bucket gets
   without the encryption.
2. **Its values are returned to callers.** `SecretStore::list` is names-only by
   contract; `VarStore::list` ([`crates/store/src/vars.rs`](../../crates/store/src/vars.rs))
   returns names *and* values, on the stated grounds that "vars are not
   sensitive".
3. **They are therefore displayed.** The project settings route in
   `crates/api/src/routes.rs` puts every var's value in its JSON, so a secret in
   `vars` is rendered in the operator UI to anyone with project access.

**D6: accept, and warn on the name.** A gate is not available — the platform
cannot distinguish a secret value from a var value, and a hard refusal on a
name heuristic would reject legitimate names (`GITHUB_API_URL` is not a secret)
while missing every secret with an innocuous name. What is cheap and honest is
S5: when a var is written whose **name** matches a secret-shaped pattern
(`*_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `*_CREDENTIAL`), say so at the
write, name the difference the three points above describe, and write it anyway.
The value of this is entirely that the three asymmetries are not discoverable
from the UI — a var and a secret look identically like a name/value pair on the
settings page.

## 8. File delivery: the plumbing is free, the declaration is not

The brief is right that no new machinery is needed. `InjectedFile` carries the
SSH credentials and #313's tokens today; the docker backend writes contents into
the container; `HostBackend::remove` deletes exactly the paths a launch recorded,
and `reclaim_credentials` empties the injected tree the moment the command
returns, sparing only the agent state directory the harvest still needs
(`crates/container/src/host.rs`). Decision 2's guarantee is therefore checkable
on both backends: that function for the host path, and `docs/spec.md` §3.1's
"Container lifecycle ends in removal" for the other.

**The convention is already chosen, in the tree, by the consumer that would use
it.** `.chug/tasks/deploy.sh` reads: *"If a future injection form hands us a file
path in `MINI_DEPLOY_KEY_FILE` instead, honour it directly"*, and its resolution
branch prefers that variable over the inline value. So S3's delivery contract is
`{NAME}_FILE` naming a path, the value at `0600` under the injected tree — the
`GIT_SSH_COMMAND` shape, one consumer already written against it, and a
migration where a task script works under either form.

**What S3 actually costs is an epoch bump, and there is no way around it.** The
declaration must be per-level, because that is what makes it scoped, and `work`,
`Evaluator` and `wrap_up` all carry `#[serde(deny_unknown_fields)]`
(`crates/types/src/job_type.rs`) while the top-level struct deliberately does
not. So an N−1 dispatcher hard-rejects a job type carrying a new nested field —
or a re-typed `secrets:` — and parks per `docs/spec.md` §14.2. That is #313 A5's
skew argument arriving at the same place for the same reason, and it is the
whole reason S3 is a separate slice from S1 and S2 rather than riding with them.

**Env stays the default (decision 3).** `docs/spec.md` §8.2 states env injection
normatively — "the dispatcher decrypts values at job launch and injects them as
env vars" — so S3 amends that sentence to describe two forms with env as the
default, and every existing declaration keeps meaning exactly what it means
today. A rewrite of every consumer is not on the table and this design does not
propose one.

## 9. Artifacts: the leak that outlives every cleanup path

Three artifact kinds persist past the task under their own retention
(`crates/store/src/artifacts.rs`): `Stdout`, `SessionTranscript`, and — the one
the brief does not name — `Output`, the archive a work container leaves at a
fixed path ([#362](362-binary-artifacts.md)), which lives in its own bucket on
its own clock and is the sink a task most easily writes to deliberately.

**There is no redaction anywhere in this tree.** Swept for it: the only
redaction-shaped code is a trace fixture masking non-determinism and a cron
bitmask. A task that echoes a secret, a script run under `set -x`, or a tool
that logs a request header produces a durable record that every cleanup path
in §[8](#8-file-delivery-the-plumbing-is-free-the-declaration-is-not) leaves
alone by construction — those paths delete what the platform *placed*, and an
artifact is what the task *produced*.

**This is out of scope by decision 2 and it is S4, a row rather than a
footnote.** Two things must be said about it and then not blurred:

- It is **not covered** by decision 2's promise, and no wording anywhere should
  let it read as covered. Decision 2 is a statement about placement and
  teardown; an artifact is neither.
- It is **not obviated by S1/S2/S3**. Narrowing `global/agents` and moving the
  provider token out of the env reduces what a task can accidentally echo,
  because there is less in the env to echo; it does nothing at all about a
  declared project secret the task legitimately holds and then prints.

S4 needs its own design because the hard parts are not implementation: what to
match on (the platform knows the exact values it injected, which is a much
stronger position than pattern-matching for key shapes), where to apply it
(at harvest, which is one place, versus at write, which is not the platform's),
what to do about a partial match across a chunk boundary, and what a redaction
costs on a transcript of the size [#490](490-agent-work-on-a-mac.md) measured —
317,723 and 462,085 bytes on two real runs — arriving over `copy_file_chunk`.
None of that is decided here.

## 10. The wire: one good property and one honest limit

Secrets are age-decrypted in the dispatcher and, **on the worker-proxied path**,
reach the node as part of a launch request over **core NATS request-reply** (a
node the dispatcher drives through a local Docker endpoint has no hop at all):
`WorkerClient` calls
`request_timeout` ([`crates/store/src/worker.rs`](../../crates/store/src/worker.rs),
[`crates/store/src/lib.rs`](../../crates/store/src/lib.rs)), which is
`client.request()` under a deadline, not `publish_event`. **Nothing lands in a
JetStream stream**, so a launch payload has no durable copy, no replay and no
retention — worth stating as a *good* property, because the same crate's event
path does persist and the difference is deliberate.

The limit: the scheme is `nats://` and not `tls://` — read out of this
container's own `NATS_URL` — so a launch payload's confidentiality on the wire
rests on the tailnet. That is the same trust boundary
the whole platform already rests on — `deploy/prod/README.md` serves the API over
Tailscale and says so — and this design neither strengthens nor weakens it. It is
recorded so that a later reader does not discover it while reasoning about
something else.

**One reader inside the trust boundary is already accepted and not reopened.**
The docker backend passes the launch env to the engine as the container's `Env`
([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs)), so
anything holding the node's docker socket can `docker inspect` a *concurrent*
task and read its secrets. [#517](517-docker-access-for-jobs.md) D1 accepts
exactly this escalation, in more detail than this paragraph, and the constraints
on this job forbid reopening it. Noted only because S2 narrows it slightly as a
side effect — a provider credential that is not in the env is not in `inspect`
output either — and a slight narrowing of an accepted risk is not a mitigation of
it.

## What this does not decide

- **Whether the agent CLI can authenticate from a withdrawable source.** M2.
  Everything about S2's mechanism is downstream of it, and the fallback is
  designed (§[4](#4-the-options-for-decision-1)(d)) precisely so the answer can
  be "no".
- **Whether the daemon should proxy model traffic** (§[4](#4-the-options-for-decision-1)(e)).
  It is the only shape that satisfies decision 1 as written, and it should not be
  started before M2.
- **Whether the window S2 buys exists on a host node.** M7. M1d is a
  container-mode measurement and the host path has no `/proc`, so nothing here
  transfers to it by argument. S2 on a host node should not be described as
  buying what it buys in a container until M7 answers.
- **Whether the platform should *enforce* `ptrace_scope` rather than assert it.**
  M6 proposes asserting at launch and failing loudly. Refusing to launch on a
  node whose sysctl is `0` — or setting it from the node modules
  ([#372](372-chug-node-modules.md)) — is a fleet-policy question this design
  raises and does not settle.
- **Any uid boundary.** #526 decided the current tenancy and #537 designs
  per-project users; this document only records that they are what would turn its
  slices into boundaries.
- **What redaction matches on, or where it runs.** S4.
- **Whether the platform's own API should issue task-scoped bearer tokens**
  (§[6](#6-does-313-generalise)). The finding is that it could; whether the one
  secret it would retire is worth the mechanism is a separate call.
- **Rotation.** Every forwarded secret's lifetime is rotation discipline today
  and remains so; nothing here proposes a rotation schedule, and asserting one
  without a mechanism to enforce it is the defect this document is about.
- **Whether `global/agents` should exist at all.** S1 narrows what rides it. A
  design that removed the blanket grant would have to say how an agent evaluator
  in a project that has never seen a provider credential authenticates, and that
  is a bigger question than this brief.

## Corrections (verified against the tree)

1. **The brief says the cheap half is narrowing *who receives* `global/agents`;
   measured, that cut buys almost nothing.** All three callers of
   `inject_platform_agent_secrets` exec the agent CLI and all three need a
   provider credential. The over-grant is that the injector lists the entire
   scope rather than the names the provider reads, so D2 cuts on the name axis
   instead. The brief's underlying point — that exposure can shrink before any
   mechanism changes — survives; only the cut moves.

2. **The brief says artifacts are two kinds; there are three.** `ArtifactKind`
   holds `Stdout`, `SessionTranscript`, `Output` and `TranscriptMissing`. The
   brief names the first two; `Output` — a work container's own archive, on its
   own retention clock — is the third and is the one a task writes to
   deliberately. `TranscriptMissing` is a marker and carries no task content.

3. **The brief's file-versus-env framing attributes the difference to the wrong
   property.** It is right that permissions are not a boundary within one uid.
   But the reason a file beats the env is not permissions at all: M3 measures
   that a process cannot remove a variable from what others read of it, so an
   env-delivered credential is exposed for the process's whole life while a
   withdrawable source is exposed for a window. Stating it as a permissions
   question would make `0600` look like the fix, which is what the brief itself
   warns against.

4. **`claude` already strips the platform token from the environment of
   processes it spawns (M1b), and this does not help.** A name-set diff between
   the CLI process and a shell it spawned differs by exactly one name. The task's
   code reads the value out of the CLI process anyway (M1c). Recorded because a
   reader who measured only the child's environment would conclude decision 1
   is already met.

5. **The `{NAME}_FILE` convention S3 needs is already chosen in the tree.**
   `.chug/tasks/deploy.sh` prefers `MINI_DEPLOY_KEY_FILE` over the inline value
   and its comment names the future form explicitly. S3 should adopt that
   spelling rather than invent one, and the estimate should reflect that one
   consumer is already written.

6. **#313's pattern reaches none of this repo's declared secrets.** Sized rather
   than asserted: `.chug/jobs/` declares two distinct secret names. Both are
   adoptable in principle and **neither by #313** — one by the platform's own SSH
   CA (needing a trust change on a machine outside this repo), one by the
   platform issuing to its own API. The brief anticipated the answer "only where
   the consumer speaks OIDC"; the measured answer is narrower still, because the
   two cases that *can* move are cases where the platform is its own downstream.

7. **The reasoning S2 needs is already in the tree and already enforced —
   against argv.** `crates/agent/src/claude.rs`'s **module header** cites
   `/proc/*/cmdline` and `assert_no_creds_in_argv` enforces it (M5); `#313 A3`
   reason 2 makes the identical argument for `/proc/{pid}/environ` and nothing
   acts on it. This design is less a new argument than the application of an
   existing one to the surface that carries more secrets. *(An earlier draft
   attributed the `/proc/*/cmdline` sentence to `claude_invocation`'s doc
   comment; it is the module header. That function's doc comment makes the same
   point in its own words — the MCP payload "carries the NATS credential, which
   must never enter argv".)*

8. **The brief's "a child process cannot read its parent's memory" is true on
   this tree, and true for a reason the brief does not give.** It is not a
   property of the parent/child relation — it is Yama's `ptrace_scope=1` denying
   a **descendant** `PTRACE_MODE_ATTACH` on an **ancestor** (M1d), plus docker's
   default drop of `CAP_SYS_PTRACE`. Stated the brief's way it reads as a
   guarantee of the process model; stated correctly it is a node sysctl any
   operator can set to `0`, at which point the recommended slice degrades to
   removing a convenience. That is the difference between a bound and a bound
   with a mechanism, which is the distinction this whole document is about — so
   the correction carries a slice (M6) rather than a caveat.

9. **The measurement that establishes M1d also corrects M1c's stated reason.**
   An earlier draft explained the successful `/proc/<cli-pid>/environ` read by
   saying the shell is a descendant, "so neither ownership nor a ptrace-scope
   restriction stands in the way". The direction is backwards: scope 1 is
   precisely a restriction *on* descendants. The read succeeds because `environ`
   is gated by `PTRACE_MODE_READ`, which Yama does not restrict at all, while
   `mem` is gated by `PTRACE_MODE_ATTACH`, which it does — the two reads land on
   opposite sides of one distinction, and getting the reason wrong would have
   predicted `mem` readable and sunk the recommended slice.
