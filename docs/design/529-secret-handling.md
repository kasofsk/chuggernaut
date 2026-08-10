# Design #529 — Secret handling: the declarative model's edges, and the platform token's reach

Status: IMPLEMENTED IN PART — S1a has landed (job #545) and is observe-only,
and S2+M6 have landed together (job #547): the provider credential now reaches
an agent container on an inherited descriptor rather than in its environment,
and the kernel property that window rests on is asserted at every launch. M7 has
since measured the host path (job #549) and S2 has not been extended to it.
Everything else below is proposed.

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
§[3](#3-decision-1-what-the-measurement-says) is that measurement, and it changes
the answer.

**M2 — the one row §3 left unverified — was run in job #546 and it holds.** The
agent CLI authenticates from an inherited file descriptor with
`CLAUDE_CODE_OAUTH_TOKEN` absent from its environment, and the source is genuinely
consumed: a second launch on the drained pipe fails, and nothing lands on disk.
The `apiKeyHelper` fallback §[4](#4-the-options-for-decision-1)(d) named is **not**
a drop-in — it delivers an API key, and this platform's credential is an OAuth
token, which it rejects. Both results, their invocations and their limits are in
the [2026-08-10 correction](#correction--2026-08-10-job-546-m2-measured-the-fd-source-authenticates-and-apikeyhelper-will-not-take-this-credential),
which is what S2 should be built against.

Two of those rows are new in this revision and both were assumptions before. **M1d**
settles the premise every slice here rests on — that a credential in the agent
CLI's memory is out of the task's reach — by measuring it from a shell the CLI
spawned rather than inheriting the brief's word for it: it holds, and it holds
because of a **host sysctl this platform does not set**, which is why it now
ships a slice (M6) instead of a sentence. **M8** replaces "small and knowable"
with two numbers for the `global/agents` scope. Both are container-mode findings.

**M7 — the host path those two do not reach — was measured on `gumbo-air-0` and
both verdicts carry over.** A same-uid descendant reads another process's
environment there as it does under `/proc`, and raw memory stays out of reach:
`task_for_pid` refused an unsigned reader. That macOS refuses it on the caller's
code signature rather than on Yama's ancestor rule is macOS's documented gating
and **not** what this run separated — its control went unreported, so the denial
it measured is equally consistent with either mechanism. What the measurement
licenses, what it does not, and the one thing it leaves open (**M9** — whether a
task can drive an entitled system tool to do what its own code cannot) are in the
[2026-08-10 measurement](#measured--2026-08-10-job-549-m7-the-host-paths-equivalents--env-readable-task_for_pid-denied-and-why-sample-proves-nothing).

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
| `global/agents` is narrowed to what the agent CLI needs | `inject_platform_agent_secrets`, `exec.rs` | **No.** The *whole scope* still reaches every agent launch — S1a only logs, by name, what S1b would decline |
| A provider-credential name set exists in the tree | `PROVIDER_CREDENTIAL_NAMES`, [`crates/agent/src/lib.rs`](../../crates/agent/src/lib.rs), from `claude::CREDENTIAL_ENV_NAMES` | **Landed** (S1a). Nothing consults it as an exclusion yet |
| The platform's provider token is out of the task's reach | `credential_delivery`, [`crates/agent/src/claude.rs`](../../crates/agent/src/claude.rs) | **Narrowed, not closed** (S2, job #547), and **container launches only** — a host task is still env-delivered, which M7 now makes a choice rather than an unknown. A *window* instead of the process's lifetime; still no boundary, exactly as D5 says |
| The agent CLI will take that token from a withdrawable source instead | `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, the shipped CLI | **Yes, measured** (M2, job #546) — an inherited fd, read once, leaving no file and no env entry. **In use** since S2 |
| …and `apiKeyHelper` is the fallback if it will not | `--settings`, the same CLI | **No.** Measured: the helper's output is used as an API key and this credential is an OAuth token, rejected as `Invalid API key` |
| A credential in the agent CLI's *memory* is out of the task's reach | Yama `ptrace_scope`, the node's kernel; `credential_ptrace_assertion`, `crates/agent/src/claude.rs` | **True today, and not by this platform's doing — now asserted rather than assumed** (M6, job #547). Every agent launch reads both properties in the container's own view and prints the verdict; it never refuses |
| …and the same holds on a **host** node, by a mechanism the run did not separate | `task_for_pid`, macOS code-signing and entitlements | **True, measured, and asserted nowhere** (M7, job #549). Unsigned code got no task port; that this holds whatever its relation to the target is macOS's documented gating rather than M7's result, whose control went unreported. The launch-time assertion above reads `/proc` and so cannot see this property either way. Entitled system tools on the node are the open half (M9) |
| Credential-bearing payloads are kept out of argv | `claude_invocation`, [`crates/agent/src/claude.rs`](../../crates/agent/src/claude.rs) | **Landed**, with a test asserting it |
| The same reasoning is applied to the env | `credential_delivery`, `crates/agent/src/claude.rs` | **For the provider credential, yes** (S2). Declared `work.secrets` and project `vars` are still env-delivered — that is S3 |
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
  establishes**, so D3 holds exactly as far as M6 (assert it) holds.

  **The premise also holds on a host node, and naming its mechanism separately is
  the point** (M7). There `task_for_pid` is refused — measured, from a descendant
  — and macOS's gating on code signing says it would be refused whichever way it
  points, where Yama refuses a *descendant* specifically; M7's run reported no
  control, so that second clause is documented behaviour and not its result. Two
  mechanisms reaching the same verdict
  can stop agreeing: a node's sysctl set to `0` flips the container half and
  nothing about the host half, and an entitled path a task could drive (M9) would
  flip the host half and nothing about the container half. The platform asserts
  neither on the host path today, because M6's assertion reads `/proc`.

- **D4. What credential sources the agent CLI accepts is a measurement, and the
  measurement is now taken.** §[3](#3-decision-1-what-the-measurement-says)
  established the current behaviour and found a named fd-delivery source in the
  shipped bundle whose semantics it could not establish; M2 ran that source and it
  works, so S2 builds on a measured mechanism rather than a hoped-for one. The
  half of D4 that still stands is the discipline, not the doubt: the answer is a
  third party's behaviour at **one version**, so S2 ships an assertion at launch
  the way M6 does, not a comment.

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
| **S1a** | Log, by name, every `global/agents` name `inject_platform_agent_secrets` would decline under S1b's set — while still injecting all of them | **Landed** (job #545) — observe-only; one release, so S1b excludes nothing a run depends on |
| **S1b** | Narrow the injector from "every name under `global/agents`" to a platform-configured provider-credential name set, injecting nothing else | Proposed — after S1a; no schema field, no epoch, moves *reach* |
| **M2** | Measure whether the agent CLI authenticates from an inherited fd (`CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`) with no token in the env | **Landed** (job #546) — it does, proved by a real completion; the fallback (d) does **not** take this credential. [The record](#correction--2026-08-10-job-546-m2-measured-the-fd-source-authenticates-and-apikeyhelper-will-not-take-this-credential) |
| **S2** | Deliver the provider credential on an inherited fd — the container's own `sh -c` entrypoint reads the injected file into a pipe, unlinks it, and runs the bootstrap with the pipe's read end at fd 9 — and stop putting it in the launch env | **Landed** (job #547), container mode only. [The record](#landed--2026-08-10-job-547-s2-and-m6-the-credential-arrives-on-a-descriptor-and-the-property-it-rests-on-is-asserted) |
| **M6** | Assert `ptrace_scope` (and the absence of `CAP_SYS_PTRACE`) at launch instead of assuming it — the host-kernel property S2's window rests on (M1d) | **Landed** (job #547) — reported at every agent launch, never enforced; the fleet-policy half stays open |
| **M7** | Measure M1c/M1d's equivalents on the **host** node path, where there is no `/proc` and `task_for_pid` governs | **Measured** (job #549) — both carry over, the memory half by a mechanism the run did not separate; S2 is **not** extended to the host path here. [The record](#measured--2026-08-10-job-549-m7-the-host-paths-equivalents--env-readable-task_for_pid-denied-and-why-sample-proves-nothing) |
| **M9** | Measure whether a task's own code can drive an **entitled** system tool on a host node (`sample`, `vmmap`, `lldb` with a session that can authorize it) to extract a credential its own `task_for_pid` cannot | Proposed — the half M7 leaves open; it bounds how much D3's window is worth on a host node |
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

## Correction — 2026-08-10, job #546 (M2 measured: the fd source authenticates, and apiKeyHelper will not take this credential)

§[3](#3-decision-1-what-the-measurement-says)'s M2 row was a name-level reading of
a minified third-party bundle and said so. It has now been run. **The fd source
works**, and the fallback §[4](#4-the-options-for-decision-1)(d) named for the case
where it did not **does not take this platform's credential**. Both halves change
S2, so both are recorded here rather than in the row.

Taken inside this job's own work container — Debian 12, `aarch64`, `JOB_TYPE=design`,
`CHUG_PHASE=Work` — against the shipped `claude`, which reports **2.1.220**. §3's
rows were taken at 2.1.226; the difference is recorded rather than explained, and
it is the reason D4's surviving half asks S2 for a launch-time assertion.

### What the fd source expects, read before it was exercised

A wrong argument shape is not a negative result, so the resolver was read first.
`CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` is a **decimal file-descriptor number**:
the CLI `parseInt`s it, opens `/proc/self/fd/<n>`, reads at most 65536 bytes and
trims. A non-numeric value logs an error and resolves to no credential. On any
read failure — and when the variable is absent entirely — it falls back to a
hardcoded well-known path, `/home/claude/.claude/remote/.oauth_token`. The
resolver caches, so one successful read serves the process. Source precedence is
`apiKeyHelper` (third-party mode only) → `ANTHROPIC_AUTH_TOKEN` →
`CLAUDE_CODE_OAUTH_TOKEN` → the fd → the well-known file → `apiKeyHelper` →
a local profile → an interactive `claude.ai` login, so removing the env var is
what lets the fd be reached.

### The five runs

Every run is a real single-turn completion of the same prompt, judged on its
output and exit code, never on the process starting. Each launched under `env -i`
with exactly `PATH`, `HOME`, `CLAUDE_CONFIG_DIR` and whatever the row adds.

| # | Credential source | Result |
| --- | --- | --- |
| 1 | none (control) | `Not logged in · Please run /login`, exit 1 |
| 2 | `CLAUDE_CODE_OAUTH_TOKEN` in the env (control) | model output, exit 0 |
| 3 | **an inherited pipe at fd 9**, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR=9` | **model output, exit 0** |
| 4 | the **same fd**, relaunched after run 3 drained the pipe | `Not logged in · Please run /login`, exit 1 |
| 5 | `apiKeyHelper` via `--settings`, the helper printing the same token | `Invalid API key · Fix external API key`, exit 1 |

The invocation for run 3, with the token read out of `/proc/1/environ` and never
printed:

```sh
exec 9< <(printf '%s' "$TOK")
env -i PATH=... HOME=... CLAUDE_CONFIG_DIR=... \
  CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR=9 \
  claude -p '...' < /dev/null
```

**The token was genuinely absent from the new process's environment.** Read from
`/proc/<pid>/environ` of run 3's process while it was authenticating, it held four
names — `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CONFIG_DIR`, `HOME`,
`PATH` — and **zero** occurrences of the token's value.

**Run 4 is the withdrawal proof, and it is the one that matters.** The same fd,
after the CLI had consumed the pipe, authenticates nothing: the source is spent.
Nothing was left behind to make up for it — the well-known fallback path did not
exist before the runs or after them, and a value-scan of the CLI's whole config
directory found no copy.

### What a descendant can still reach, measured rather than argued

D3 claims the fd buys a *window* where the env buys a lifetime. That claim now has
the same treatment M1c and M1d gave the env, taken from a process the CLI itself
spawned (its own shell tool), which is the relation a task's code stands in:

- The spawned process does **not** hold fd 9 — the CLI does not pass it to
  children. (M1b's stripping of `CLAUDE_CODE_OAUTH_TOKEN` from a child's
  environment has an analogue here, and it is worth no more trust than M1b was.)
- It **can** reach the CLI's own fd through `/proc/<cli-pid>/fd/9`, exactly as
  M1c reads `/proc/<cli-pid>/environ` — the same `PTRACE_MODE_READ` gate, the same
  descendant direction, no permission in the way.
- Reading it returns **0 bytes**. The pipe was drained by the CLI and its writer
  had closed, so the path resolves to a credential that is no longer there.

So the residue is a reachable *handle* to nothing, which is what "window rather
than lifetime" has to mean if it is to mean anything. The race the design already
names is **not** measured: a task process that opens the fd *before* the CLI reads
it would drain the pipe, and how long that window is was not established.

### The trap S2 must assert against: `CLAUDE_CODE_REMOTE`

The same code path that reads the fd will **write what it read to disk** — at
`/home/claude/.claude/remote/.oauth_token`, mode `0600`, described in the bundle as
"for subprocess access" — and it does so if and only if `CLAUDE_CODE_REMOTE` is
set in the CLI's environment. Measured with a dummy string rather than the real
token: with the variable set, the file appeared at that path and mode with the
dummy's contents. The platform does not set it today (it is absent from this
container's `/proc/1/environ`). **If it ever were set, fd delivery would persist
the credential to a file no cleanup path deletes and S2 would buy nothing** —
which makes its absence a launch-time assertion for S2, on the same footing as M6.

### Why (d) is not the fallback §4 wrote it as

Run 5's failure is a *rejection*, not a non-read: the error moved from
`Not logged in` (run 1, nothing supplied) to `Invalid API key`, which is only
reachable once a credential has been obtained and sent. The helper ran and its
output was consumed — as an **API key**. This platform's `global/agents`
credential is an OAuth token, and the two are different headers. Sweeping the
bundle's settings surface for helper-shaped keys finds `apiKeyHelper` as the only
Anthropic-credential one (`otelHeadersHelper` and `proxyAuthHelper` are neither).

So (d) is available only to a deployment whose provider credential is an
`ANTHROPIC_API_KEY`. Whether it authenticates with one is **unmeasured**: no API
key exists in this container, and obtaining one is outside a measurement job. The
honest ordering therefore tightens rather than moves: **(c), now measured, is the
recommendation and no longer has a same-cost fallback behind it**; if (c) is
rejected for some reason this measurement did not surface, the next option is (e)
or (f), not (d).

### The seam S2 gets for free, and the limits of all of the above

This container's pid 1 is `sh -c 'claude -p "$(cat /chuggernaut/prompt.md)"
--settings /chuggernaut/agent-settings.json --output-format stream-json ...'`,
which confirms M4 from the process rather than from the source and hands S2 its
mechanism: the dispatcher cannot pass a file descriptor across the docker
boundary, but the entrypoint shell it already composes can open one from an
`InjectedFile`, unlink the file and exec `claude` with the number. No new
plumbing, and the unlink is what makes the file a delivery rather than a residue.

Stated limits, so the next reader does not over-read this:

- **Single-turn `-p` only.** The platform's real launch is a long
  `--output-format stream-json` session with MCP attached. The resolver caches and
  this credential class carries no refresh token by the CLI's own error text, so
  one read should serve the session — inference from the cache and the strings,
  not a measured long run.
- **Container mode only**, like M1a–M5. M7 is untouched by this.
- **One CLI version**, 2.1.220, and a third party's behaviour at one version is
  what M1b already warned this class of finding is.

## Landed — 2026-08-10, job #547 (S2 and M6: the credential arrives on a descriptor, and the property it rests on is asserted)

S2 and M6 shipped together, as §[4](#4-the-options-for-decision-1)'s ordering
requires: without M6 the slice asserts a bound whose mechanism it never checks.
Built on the [job #546 correction](#correction--2026-08-10-job-546-m2-measured-the-fd-source-authenticates-and-apikeyhelper-will-not-take-this-credential)
and on the seam that correction names — the container's own `sh -c` entrypoint —
not on a new plumbing layer.

### The mechanism, and the one place it deviates from the slice's wording

`ClaudeProvider::launch_config` ([`crates/agent/src/claude.rs`](../../crates/agent/src/claude.rs))
moves `CLAUDE_CODE_OAUTH_TOKEN` out of the composed launch env into a mode-0600
`InjectedFile` at `/chuggernaut/agent-credential`, sets
`CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR=9` (a number, never a credential), and
wraps the bootstrap script:

```sh
<ptrace assertion>; unset CLAUDE_CODE_REMOTE; \
{ cat /chuggernaut/agent-credential; rm -f /chuggernaut/agent-credential; } | \
{ exec 9<&0 0</dev/null; <clone && cd && exec claude …>; }
```

**The deviation: a pipe, not the injected file opened directly.** The slice was
written as "opens the injected file, unlinks it and execs `claude` with the fd
number". Implemented that way it would buy nothing: an unlinked *regular* file is
whole and re-readable behind `/proc/<cli-pid>/fd/9` for the CLI's entire life,
which is the M1c relation and therefore exactly the lifetime the env already had.
The pipe is what makes #546's measured residue — a reachable handle to **0
bytes** — the thing a task actually finds. The unlink still matters, and happens
in the same breath: it is what stops the file itself being a second copy.

**The exposure window, measured rather than claimed.** Two windows, because there
are two artefacts:

| Artefact | Open from | Closed by | Readable during it by |
| --- | --- | --- | --- |
| the injected **file** | the backend's put-archive, which runs after container create and **before** start ([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs)) | the entrypoint's first command — before the clone, before any task code exists | nothing inside the container (no process runs before the entrypoint); on the node, root and anything holding the docker socket, which [#517](517-docker-access-for-jobs.md) D1 already accepts |
| the **pipe** | that same first command | the CLI's first read of the descriptor | the platform's own bootstrap (`sh`, `git`) — the task's own code does not exist until the CLI spawns a tool, which is after it has authenticated |

So the honest statement is: **the file's window is the entrypoint's first
command, and the pipe's window ends at the CLI's first read** — where the env's
window was the whole task (M3). It is a window and not a boundary (D5), and the
race §[4](#4-the-options-for-decision-1)(c) names is unchanged: a task process
that could run before the CLI reads would drain the pipe. Nothing here measures
how long the pipe's window is in wall-clock terms; it spans the workspace clone,
which is seconds to minutes, and no task-controlled code runs inside it.

**What else the wrapper does.** `unset CLAUDE_CODE_REMOTE` closes the trap #546
found — the same code path writes what it read to
`/home/claude/.claude/remote/.oauth_token` if that variable is set — on the
image-`ENV` route as well as the composed-env route, which the env-side removal
alone would miss.

### M6: asserted, reported, and deliberately not enforced

`credential_ptrace_assertion` reads `/proc/sys/kernel/yama/ptrace_scope` and
`CapEff` from `/proc/self/status` **in the container's own view** (docs/reference/style.md
Tier 2 #7), tests bit 19 for `CAP_SYS_PTRACE`, and prints one
`chuggernaut: agent-credential:` line to stdout — which is the harvested
`stdout.log` and the live log. It uses shell builtins only (`read`, `case`,
arithmetic, `printf`), so it depends on nothing a project image might not carry,
and a malformed mask is filtered to `unknown` rather than reaching arithmetic
that would kill a POSIX shell outright.

**It reports and never refuses, and that is a decision with a reason.** Measured
before choosing: this job's own work container reports `ptrace_scope=1` and
`CapEff=00000000a80425fb` (bit 19 clear), and M1d measured `1` in job #545's
container. That is two containers, not a fleet sweep — the platform has no
node-level probe for either value, which is itself part of why M6 exists. Three
arguments against fail-closed, in order of weight:

1. **An unsatisfied node loses nothing relative to today.** §[4](#4-the-options-for-decision-1)(c)
   already says the slice degrades to (a) with extra steps when the sysctl is
   `0`. Refusing the launch would therefore trade *availability* for *no security
   gain over the status quo*.
2. **It is self-blocking.** Every agent job on that node fails, including the job
   that would fix the node. Command jobs — `deploy`, `rollback`, `ci` — are
   `work.type: command` and unaffected either way, which is what keeps the
   platform recoverable; but the repair path for a fail-closed agent fleet would
   be an operator, not the platform.
3. **Enforcement is a question this document explicitly leaves open** ("What this
   does not decide"). A slice that refused would settle it by implementation.

`docker_grant_refusal`'s precedent points the other way and does not transfer: it
refuses a *grant the job declared* and cannot safely have. Here the job declared
nothing; the platform is checking its own premise.

### The kill switch, and why a slice this wide has one

`CHUG_AGENT_CREDENTIAL_FD=0` in the dispatcher's environment reverts every agent
launch to env delivery. This changes how *every* agent container on the platform
authenticates, and the change is only exercised after a deploy — this job's own
evaluators ran on the old dispatcher, so the slice cannot self-test. The switch
makes the failure mode "set a variable and restart" rather than "release a
revert while agent jobs are broken".

### Deliberate scope limits

- **Container mode only.** A host task (`image: None`) keeps env delivery
  untouched: M7 has not measured the `/proc`-free path, and #529 forbids claiming
  the window there.
- **`ANTHROPIC_API_KEY` keeps the env route.** #546 measured the OAuth token's
  descriptor variable; the API key's sibling was seen in the bundle but never
  named or run, and a guessed variable name authenticates nothing.
- **Long sessions are still an inference.** #546's runs were single-turn `-p`;
  the platform's launch is a long `stream-json` session. The resolver caches and
  this credential class carries no refresh token, so one read should serve — but
  a pipe cannot be re-read, which is what the kill switch is for.
- **The dispatcher-side `AgentRunConfig.env` still carries the token.** It is an
  in-process struct that never becomes any container's environment; the provider
  owns the CLI's credential names and its descriptor variable, so the removal
  belongs there and the assertion is on `ContainerLaunchConfig.env`.

## Measured — 2026-08-10, job #549 (M7: the host path's equivalents — env readable, `task_for_pid` denied, and why `sample` proves nothing)

M7 asked whether M1c and M1d hold on the **host** node path, where there is no
`/proc` and `task_for_pid` governs. **Both hold**, and the memory half is
believed to hold for a different reason than it does in a container — believed,
because the run measured the denial and not the mechanism behind it, and the
distinction is the part of this section that must not be compressed into "same
answer". Job #547 left host tasks
on env delivery *because* this row was open
([its scope limits](#deliberate-scope-limits) say so in as many words); what the
answer licenses is at the end.

Taken by the operator on `gumbo-air-0` — uid 501, macOS 26.5.1 — on 2026-08-10
and quoted here; a work container has no Mac, so nothing below was re-run while
writing it. The shape mirrors M1c/M1d's: a parent process held a marker in its
environment and spawned a descendant — the relation a task's code stands in to
the agent CLI — and the descendant attempted the reads. A control repeating them
against a process the reader had forked itself was part of the run, **and its
outcome was not reported back**; every row below is the descendant's. That is a
real limit on what the denial can be attributed to, and it is stated again where
the attribution is made rather than left here. The tree facts this section
states are checkable and were checked, in
[`crates/agent/src/claude.rs`](../../crates/agent/src/claude.rs) and
[`crates/container/src/host.rs`](../../crates/container/src/host.rs).

### The reads

| Read | Result |
| --- | --- |
| `ps -Eww -p <target>` from a descendant | **the marker is visible** |
| `vmmap <target>` | rc 0, no marker in the output — it prints regions, not their contents |
| `sample <target> 1` | **rc 0, a real sample produced** |
| `lldb --batch -p <target>` | `error: attach failed: this is a non-interactive debug session, cannot get permission to debug processes` |
| `task_for_pid()` through `ctypes`, from ordinary unsigned Python | **`KERN_FAILURE` (5), port 0** |
| `mach_vm_read` scan for the marker | not reached — there is no task port to scan with |
| *control* — the same reads against a process the reader forked itself | **not reported back**; every row above is the descendant's |

### The environment is readable, exactly as M1c found

`ps -Eww` from a descendant returns the marker. So the motivation for S2 is not
a `/proc` artefact: on a host node an env-delivered credential is readable by
same-uid code for the whole life of the agent process, and M3 — a process cannot
un-publish its own environment — is not a Linux fact either. On this platform's
host nodes today the task and the CLI **are** the same login user, so "same uid"
is the ordinary case rather than a hypothetical; per-project users
([#537](537-per-project-users-macos.md)) are what would change it.

### Raw memory is not readable — by a mechanism this run did not separate

`task_for_pid` from unsigned code returns `KERN_FAILURE` with a null port, so the
`mach_vm_read` scan had nothing to run against. **The denial is measured. The
mechanism behind it is not**, and the difference matters, because the verdict
matching M1d is only interesting if the two verdicts can move independently:

- **Linux denies by direction, and M1d separated that.** Yama at
  `ptrace_scope=1` grants `PTRACE_MODE_ATTACH` only to an *ancestor*; M1d's
  control — the same shell attaching to a child it forked itself — **succeeded**
  while the read against the CLI failed, and that contrast is what makes
  directionality a finding rather than a guess.
- **macOS is expected to deny by the caller's provenance**, the task port being
  gated on code signing and entitlement, so unsigned code would be refused
  whichever way it points. **This run did not separate that**: it reports one
  denial, from a descendant, which is equally consistent with Linux's mechanism.
  Provenance is macOS's documented gating, not this run's result — and naming
  `taskgated` as the component enforcing it is an attribution the run cannot
  distinguish from any other entitlement check. The control M1d ran is what
  would settle it: `task_for_pid` against a child the reader forked itself,
  where success means direction is the discriminator here too and failure means
  it is not.

**They could diverge, which is the reason to name both.** A node whose sysctl is
set to `0` flips the container half and says nothing about the host half; an
entitled path a task could drive (M9, below) would flip the host half and say
nothing about the container half. A single sentence — "memory is out of reach on
both" — would hide two independent premises behind one verdict.

### `sample` succeeding proves nothing about a task's reach

This is the finding most worth recording, because an earlier reading of this same
run concluded *"memory is readable on macOS"* from `sample`'s rc 0 alone, and it
was wrong. `sample` is an Apple-signed binary carrying a task-port entitlement:
its success measures **Apple's signature**, not the caller's privilege. The
caller's privilege is the `task_for_pid` row, and that row is a denial.

That is the same class of error as two already on this corpus's record —
[#490](490-agent-work-on-a-mac.md)'s `simctl spawn` failure attributed to the
daemon's session when the argument was the cause (corrected in job #527), and
[#543](543-placement-granularity.md)'s correction 4, a control reported absent
because a closed finding was inherited rather than re-measured. In each, a
tool's outcome measured something *adjacent* to the claim and was read as the
claim.

**`lldb`'s refusal is not a kernel denial either**, and is recorded that way so a
later reader does not cite it as one. Its message names a non-interactive debug
session that cannot get permission — an authorization artefact of running the
attach over an SSH session, not a statement about what the kernel would allow a
session that could authorize. It is evidence of nothing in either direction.

### What is left unmeasured — M9, and the control

**The bigger one is M9.** Entitled tools are present on the node: `sample` and `vmmap` ran, and `lldb` is
installed and refused for a reason a session might satisfy. Whether a task's own
code can **drive one of them** to extract a credential its own `task_for_pid`
cannot is **not settled by this run**, in either direction:

- `vmmap` printed regions and not contents, which is a fact about `vmmap`'s
  output and not about what an entitled tool can reach.
- `sample`'s output is not reported as searched for the marker, so it is neither
  a positive nor a negative.
- `lldb` never attached, for the authorization reason above.

M9 asks it, and it is worth asking because it bounds what D3's window is worth on
a host node. Assuming the comfortable answer is the failure mode this whole
document exists to avoid; assuming the alarming one would understate a measured
denial. Neither is written here.

**The smaller one is the control**, above: one `task_for_pid` against a
self-forked child, which is what would turn "macOS denies by provenance" from
documented behaviour into a result of this run. It changes no verdict either way
— the denial against the CLI is measured whatever explains it — but it is the
line between the two mechanisms this section asks a later reader to track
separately, so whoever next has a Mac in hand should take it while they are
there.

### What it licenses for a host task, and what still blocks it

Both halves of the container argument carry over — the motivation (env readable
for the process's life) and D3's premise (the credential, once in the CLI's
memory, out of ordinary reach). And the mechanism has somewhere to live: a host
launch already runs its command through an `sh -c` wrapper it composes
(`supervised_cmd`) and already materializes `InjectedFile`s into the task's own
directory, so S2's pipe-and-unlink needs no new plumbing;
`credential_delivery` declines the host path on `image.is_none()` and nothing
else.

**Three things still block the extension, and M7 dissolves none of them:**

1. **M6's assertion is `/proc`-shaped, and on a Mac it would report the opposite
   of this measurement.** `credential_ptrace_assertion` reads
   `/proc/sys/kernel/yama/ptrace_scope` and `/proc/self/status`; on a host node
   neither exists, both fall to `unknown`, and the WARNING branch prints — telling
   an operator a task *can* read the credential out of the CLI, which M7 measures
   it cannot. A host extension needs an assertion shaped to the property that
   actually governs there, and M7 does not supply one: a code signature and an
   entitlement are not a value a shell reads out of a file.
2. **M9.** How much the window is worth on a host node is bounded by an answer
   nobody has.
3. **The wrapper carries a wire path.** `credential_fd_bootstrap` writes
   `/chuggernaut/agent-credential` into the script literally, while a host
   command resolves its `/chuggernaut` paths through `"$CHUG_HOST_CREDS"`
   (`claude_invocation_path`, [#322](322-macos-native-runtime.md) §2's rebase,
   which [#490](490-agent-work-on-a-mac.md)'s slice-5 record names for the `cmd`
   surface specifically). Mechanical, and it is why "stop declining on
   `image.is_none()`" is not the change.

**And one line of the code now gives a reason that has expired.**
`credential_delivery`'s own doc comment says it returns `None` for a host task
"whose `/proc`-free path #529 M7 has not measured" — which this record
falsifies. The behaviour is still right and the three blockers above are why;
only the stated reason is stale. This job is prose-scoped and cannot rewrite it,
so it is written down here: the code job that extends S2 to the host path
rewrites that sentence rather than inheriting it, and a reader who finds it
before then should read it as pointing at this section.

So M7 licenses a slice and is not one. Extending S2 to the host path is future
work carrying an assertion of its own to design, and nothing here does it.
