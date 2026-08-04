# Design #313 — Workload identity (OIDC issuer) and image build/push

Status: IMPLEMENTED IN PART — half A's slices S1 (job #410), S2 (job #411) and
S5 (job #412) shipped; S3, S4 and S6 are open and half B is still a design.

**Amended 2026-08-04 (job #409), against the tree at `f1e3b41`.** The operator
has taken half A's four open decisions. They are recorded in
[Decisions taken](#decisions-taken-2026-08-04) and marked in the sections that
argue them, with every rejected option and its reasoning left standing — the
rejections are why the decisions are defensible, and
[A4](#a4-the-public-reachability-problem-the-crux)'s rejected options now carry
an explicit trigger for revisiting them. Three factual claims were
corrected, and the epoch bump is re-derived from
[`crates/types/src/version.rs`](../../crates/types/src/version.rs): **4 → 5**,
not the `1 → 2` [A5](#skew-this-field-costs-an-epoch-bump) inherited from its
siblings. Half B is untouched beyond
[correction 2](#corrections-verified-against-the-tree)'s scope note and the
consequence it has for slice S7. **No code changed** — slices S1–S5 are separate
jobs, and this amendment exists so they can cite the document safely.

The status token is `IMPLEMENTED IN PART` from S2 onward, per
[`docs/design-docs.md`](../../docs/design-docs.md)'s vocabulary — some slices
merged, and the qualifier says which. Nothing a job container sees has changed
yet: S1 generates a keypair, S2 is a library nothing calls, and S5 serves two
documents on a loopback bind, so the first slice with an operator-visible
effect is S4.

Written against the tree at `d7ebfae`. Every claim about current behavior below
was read out of [spec.md](../../spec.md) and the source in this repo; where the
brief or a sibling doc disagrees with the tree, the tree wins and the
disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree). Every claim about Google
Cloud's Workload Identity Federation was fetched from current provider
documentation and is cited inline — see
[Verification of the provider claims](#verification-of-the-provider-claims).

Doc 4 of 4 — and the last — extracting implementable specs from
[design #308](./308-gha-port.md). Its category D (image build and push) and its
taken decision "Chuggernaut becomes an OIDC issuer" are the motivation.

**Three of the four sibling docs have since shipped** — this paragraph said the
opposite when the document was first written, and the amendment corrects it.
[#310 scheduled jobs](./310-scheduled-jobs.md),
[#311 job inputs](./311-job-inputs.md) and
[#293 worker capacity](./293-worker-capacity.md) all read `Status:
IMPLEMENTED`; `inputs:` is a real field on `JobType`
([`crates/types/src/job_type.rs`](../../crates/types/src/job_type.rs)) with
[`.chug/jobs/rollback.yaml`](../../.chug/jobs/rollback.yaml) as its shipped
first consumer at `min_dispatcher: 2`.
[#309 host-native execution](./309-host-native-execution.md) is the only sibling
still `PROPOSED`, and even its `runtime:` epoch is already frozen in the tree as
`RUNTIME_SCHEMA_EPOCH = 4`. So the live dependency below is on **#309 alone** —
named where it is real in [Sequencing](#sequencing-and-what-ships-first) (slice
S11) and in [B1](#b1-the-build-mechanism)'s `NodeCapabilities` note. Everything
this document needs from #310, #311 and #293 is a mechanism available today.

Related: [spec.md](../../spec.md) §1.1 (job types, per-container secret
scoping, the config root), §2.2 (release validation's three passes), §3.1
(fleet, worker daemon, node-local build caching, worker self-refresh), §4.1
(container env), §4.2/§4.3 (injected files, the job brief), §5.3 (the reserved
`CHUG_` prefix), §6.3 (events), §7.1 (JWT RS256), §7.4 (per-job credentials),
§8.2 (age-encrypted secrets), §10.1 (container isolation), §10.2 (secrets
discipline), §10.3 (audit trail), §12.1 (platform init; private keys mounted at
runtime, never in NATS KV), §12.3 (admin CLI), §14 (config and version skew),
Appendix: Infrastructure Summary, Appendix: Deferred;
[deploy/prod/README.md](../../deploy/prod/README.md) §4 (R2 backups), §5a/§5b
(tailnet vs public exposure), §6 (image builds on the node);
[STYLE.md](../../STYLE.md) (Tier 1 no host-daemon reach, Tier 2 #2 asserts, #3
bounds, #4 naming, #6 tests; Tier 3 simplicity, single writer);
[CLAUDE.md](../../CLAUDE.md) ("the evaluation gates ARE the CI"; per-consumer
forge); [testing.md](../../testing.md); [crates.md](../../crates.md).

## Problem

Two halves that are one question — **how does a job get a credential, and what
may it do with it** — asked at two different boundaries.

**Half A.** A ported deploy job (#308 category C) must authenticate to Google
Cloud. beacon does this keylessly today through GitHub Actions' OIDC token and a
`workload_identity_pool_provider` bound to
`https://token.actions.githubusercontent.com` with a `kasofsk/beacon` repository
claim condition. That binding cannot be reused: chuggernaut is not GitHub
Actions, its tokens have a different issuer, and the repository claim does not
exist in them. The operator has taken the decision — chuggernaut becomes an OIDC
issuer and a **second** provider is registered alongside beacon's existing one.
What is undecided is the token, the key, the lifetime, the scoping, and the one
thing that is not a code question at all: a cloud STS must be able to validate
the signature, and `gumbo-mini-0` is reachable only over the tailnet
(`deploy/prod/README.md` §5a).

**Half B.** Five beacon workflows build and push container images. Job
containers have no docker socket, no docker-in-docker, and no registry auth —
and, verified below, this repo has **no container registry at all**: it builds
its own images node-locally and never pushes them anywhere. #308 left the
mechanism open as D1 (scoped socket) versus D2 (rootless builder) and said the
tiebreaker was whether host-native nodes land. That reasoning does not
survive #309, which is why this doc opens with it.

## Corrections (verified against the tree)

Five claims that shaped the brief or a sibling doc do not survive contact with
the source. Each moves an argument.

1. **The reserved `CHUG_` prefix is spec §5.3, not §11.** §11 is "Mobile /
   PWA". The rule lives in §5.3 (linked-origin credentials) and is enforced in
   `crates/dispatcher/src/release.rs` and again at injection in
   `crates/dispatcher/src/exec.rs`. [#311 correction
   1](./311-job-inputs.md#corrections-verified-against-the-tree) found the same
   thing. The stale `spec §11` citation also sits in a comment in
   `.chug/jobs/deploy.yaml`; fixing it is a docs job's, not this one's.

2. **This repo has no container registry and pushes no images.** `grep -rn
   "docker push"` over the tree returns nothing. `deploy/prod/build-worker.sh`
   and `deploy/prod/worker-refresh.sh` build `chuggernaut/worker`,
   `chuggernaut/agent` and `chuggernaut/agent-rust` **on each node** with
   `DOCKER_BUILDKIT=1 docker build`, tagged locally and consumed locally; the
   §3.1 worker self-refresh exists precisely so each node rebuilds its own
   copies rather than pulling them. The Appendix: Infrastructure Summary lists
   "Image registry | Harbor or Zot | ECR, GCR, GHCR" as an infrastructure row
   with no default chosen. So half B's registry half is **not** a code gap in
   this repo — it is an infrastructure decision nobody has taken. That matters
   for [B2](#b2-registry-auth-falls-out-of-half-a): "registry auth falls out of
   half A" is true, and there is still no registry for it to fall out into.

   **Scope note, added 2026-08-04.** The sentence "this repo has no container
   registry at all" is true of **chuggernaut** and misleading as a statement
   about the **port**, which is what half B is for. Per the operator's
   2026-08-04 beacon inspection — `~/beacon` is not checked out in this
   workspace, so this is relied on **secondhand** and marked as such, the same
   way [#361](./361-per-run-placement.md) and
   [#362](./362-binary-artifacts.md) mark theirs — beacon carries **six
   composite actions** under `.github/actions/`
   (`build-push-{bot,web,worker}` and `deploy-{bot,web,worker}`) that
   authenticate to GCP and push to **Artifact Registry**. So the target has a
   registry; this repo is the side that does not.

   The sequencing consequence: **S7 ("operator: a registry chosen and
   provisioned") is *plausibly* already satisfied** by that Artifact Registry.
   Plausibly, not "is" — nobody has checked here whether the existing repository
   is the one a chuggernaut build job should push to, whether it is in the same
   GCP project as the workload identity pool, or whether its IAM is authored for
   a second writer. S7 stays on the slice table as an operator item; what
   changes is that its likely content is "confirm and grant", not "choose and
   provision".

3. **Image building is already a worker-daemon operation, not a container
   capability.** §3.1's "worker self-refresh" is a `refresh { sha, tag }` op on
   `req.worker.{node}.>`: the daemon fetches the build context itself over the
   ssh front and runs the three `docker build`s locally
   (`deploy/prod/worker-refresh.sh`). The tree therefore already holds the
   precedent half B needs — *the daemon builds; job containers never do* — and
   it is a stronger starting point than either of #308's two options.

4. **`ContainerLaunchConfig` has no bind-mounts and no security options.**
   `crates/container/src/lib.rs` defines it as `{ image, cmd, env, files,
   cpu_limit, memory_limit, node }`. The single bind that exists anywhere is
   added **worker-side** (`crates/container/src/docker.rs`: `binds:
   cache_dir.map(...)` for `CACHE_MOUNT_PATH`), which §3.1 calls "a node
   property added worker-side, not a launch input". Consequence: #308's D2
   (rootless buildkit, which wants relaxed seccomp/apparmor) cannot be expressed
   as an ordinary launch today, and any socket for D1 must be a node-side
   decision. Both options are further from "just configure it" than #308
   implies.

5. **A raw docker socket on a chuggernaut node reaches the platform's own
   credentials, not merely other projects' caches.** `deploy/prod/build-worker.sh`
   runs the daemon as `docker run -d --restart=always --name chug-worker -v
   /var/run/docker.sock:/var/run/docker.sock -v
   $HOME/chuggernaut-worker/keys:/data/keys:ro …`. Anything holding that socket
   can `docker inspect chug-worker`, learn the host path of `/data/keys`, and
   bind it into a container of its own — which yields `worker.creds` (the node's
   NATS credential) and `worker_git`. `worker.creds` subscribes
   `req.worker.{node}.>`, and §3.1 states that a launch request carries "prompt,
   per-job credentials, harness config" **inline**. So the socket does not just
   grant node root; it grants the ability to receive other jobs' minted
   credentials. #308 D1's stated cost ("effectively root on that node") is
   accurate and understated.

---

## Decision 0: the #308 vs #309 contradiction, resolved

The brief is right that both cannot stand as written. They resolve **without a
carve-out**, because #308's premise is wrong and #309's rule is not the thing in
the way.

**#308 §D says:** "Both options dissolve if host-native nodes land (section H):
a host node has a real docker daemon the way gumbo does, and the question stops
being interesting."

**#309 §10 says:** "host tasks do not get the docker socket … A job type that
needs one (#308 category D's image-build case) is a node-side allow-list entry,
never a job-type field the platform honors on request. Deciding that case is doc
4's, not this one's."

Read together, #309 did not forbid the mechanism — it forbade one *shape* of it
(a job-type field the platform honors on request), named the shape it would
accept (a node-side allow-list), and deferred the decision here. The genuine
conflict is narrower than the brief frames it, and it is entirely with #308's
"dissolves". That premise fails on three verified counts:

1. **The gumbo analogy does not transfer.** gumbo is beacon's runner and
   beacon's alone. A chuggernaut host node is single-tenant *for host work* by
   #309 §10's `WORKER_HOST_PROJECTS` rule — but the same node's **container**
   fleet runs everything the fleet places on it, and the docker daemon the host
   task would reach is that fleet's daemon. The blast radius includes every
   other project's running containers, their injected secrets, and — per
   [correction 5](#corrections-verified-against-the-tree) — the node's own NATS
   credential.
2. **"A real docker daemon" answers the wrong half.** Half B is two problems:
   *may I build* and *may I push*. Host-native changes where a builder may run
   and what cache it may keep. It changes nothing about who may push, because
   push is an authentication question — which is half A. #308 §D's "dissolve"
   claim would at best dissolve one of the two.
3. **There is nothing to push to** ([correction
   2](#corrections-verified-against-the-tree)). A host node with a warm buildx
   cache and no registry produces an image that exists on exactly one machine —
   which is fine for the platform's own self-refresh and useless for a
   deployable artifact.

> **Resolution: #309's socket rule stands unamended, and #308 §D's "both
> options dissolve if host-native nodes land" is retracted. Image builds are
> neither a host-mode capability nor a socket in a job container. They are a
> node-provided build service, reached through a narrowed API, on a pinned
> builder node — available in container mode today and independent of whether
> #309 ever lands.**

That resolution has a pleasant consequence for sequencing: half B stops
depending on #309 entirely. #308's ordering makes phase 6 (image builds) depend
on "2 or 4"; after this decision it depends only on 4 (this doc's half A) plus
an operator standing up a registry.

The mechanism is specified in [B1](#b1-the-build-mechanism); the rest of half B
follows from it.

---

# Half A — workload identity

## Decisions taken, 2026-08-04

Half A opened four questions and recommended an answer to each. The operator
took all four, as recommended, on 2026-08-04. They are recorded here so a slice
can cite one line rather than re-read a section, and marked again at the section
that argues them.

**The rejected options stay in this document, unedited.** They are not history
to be pruned: each is the reasoning that makes its decision defensible, and
[D1's trigger](#what-would-change-d1) is a live condition a future reader is
expected to check rather than re-argue.

| # | Decision | Argued in | Rejected, and why it stays |
| --- | --- | --- | --- |
| **D1** | **An uploaded JWK set and no public endpoint.** Register the provider with `--jwk-json-path` and `--issuer-uri https://chug.kasofsk.xyz` — a stable identifier we control, not a URL anyone fetches. No Funnel, no tunnel, no relay, **no inbound path to `gumbo-mini-0`** | [A4](#a4-the-public-reachability-problem-the-crux) | Tailscale Funnel, a path-scoped Cloudflare Tunnel, and an R2 static relay stay priced as the answer *if* a second cloud reopens the requirement — see [What would change D1](#what-would-change-d1) |
| **D2** | **`workload_identities: [name]`, a named reference.** Each name resolves against a project-scoped `cloud-identities.{owner}.{project}.{name}` record | [A5](#named-reference-or-inline-cloud-configuration) | Option A (inline `audience`/`service_account` in the job-type file) is rejected: it has nothing to validate against, so a typo fails inside a container rather than at release. A5's "do not ship both readings" stands |
| **D3** | **The issuer emits `workload` as a claim**, computed dispatcher-side, not assembled cloud-side in CEL | [A1](#sub-versus-custom-claims) | The CEL mapping is editable cloud-side; the claim is not. The recorded cost is accepted as stated: adding a component to the composite later is a token change **and** a re-tag of every existing IAM binding |
| **D4** | **The exchanged token impersonates a service account**, mirroring beacon's current posture. `service_account` is therefore **populated** in the `cloud-identities` record, not optional-in-practice | [A5](#a5-per-container-scoping), [B2](#b2-registry-auth-falls-out-of-half-a) | Direct `principalSet`-to-resource binding — no SA in the middle — is rejected *for now*: beacon's existing bindings grant to a service account, so the direct form would mean re-authoring them rather than adding a second provider beside them. It is the cheaper posture once no GHA-era binding is left |

D4 is the one decision this document did not previously frame as a question, so
state its consequence plainly: `service_account` moves from "optional field in a
record whose schema is undecided" to a field every record populates, and
[A6](#a6-audit)'s audit row set is unchanged — the impersonated SA is visible in
Google's audit log, joined to ours by `jti`.

## A1. What a workload token identifies

The claim set **is** the security boundary: an attribute condition on the cloud
side can only assert what the token carries. So the question is not "what would
be nice to log" but "what must a cloud-side policy be able to say no to".

The policy the operator named — *only `deploy`-type jobs of `kasofsk/beacon` may
impersonate the deployer SA* — decomposes into three facts: **which project**,
**which job type**, and (this doc adds) **which container within the job**. The
last one is not decoration. [A5](#a5-per-container-scoping) makes the
*declaration* per container, matching how `secrets:` already works — but a
declaration the cloud cannot see buys nothing there: without a `container`
claim, a work container's token and an evaluator's token of the same job type
are byte-indistinguishable to the STS, so "only the work container may push"
is not expressible in any IAM binding. The declaration scopes who gets a token;
the claim is what lets the cloud act on that scoping.

### `sub` versus custom claims

Two hard constraints from the provider side, both verified:

- `google.subject` — the mapped subject — is capped at **127 bytes**, and
  exceeding it is a hard error at exchange time.
- An attribute condition is a CEL expression capped at **4096 characters**, and
  Google's guidance for the GitHub Actions case is that a condition is
  mandatory, not optional ("To help protect against spoofing threats, you must
  use an attribute condition").

So `sub` must hold something **short, stable and policy-relevant**, and anything
volatile belongs in custom claims. A `sub` containing the job seq would be
unusable in a policy: an IAM binding cannot name a number that changes every
job.

> **`sub` = `project:{owner}/{project}:type:{job_type}`.**
> For the motivating case that is `project:kasofsk/beacon:type:deploy` — 34
> bytes, comfortably inside 127, and directly bindable as a
> `principal://…/subject/…` member.

Custom claims, and why each earns its place:

| Claim | Example | Why it is in the token |
| --- | --- | --- |
| `project` | `kasofsk/beacon` | The tenancy boundary; the one claim a condition must always assert |
| `job_type` | `deploy` | The authority boundary — #311's rule "the type is the unit of authority" applies exactly |
| `container` | `work`, `eval:health`, `wrap_up` | The per-container scoping boundary ([A5](#a5-per-container-scoping)); mirrors the existing `CHUG_EVALUATOR` env stamp (§4.1) |
| `workload` | `kasofsk/beacon:deploy:work` | The **composite policy key**: the three above joined, so a cloud-side binding is one string comparison rather than CEL assembly |
| `job_seq` | `4211` | Audit join key; never a policy input (it changes every job) |
| `task_id` | `4213` | Audit join key; the same id §6.3 events already carry |
| `phase` | `Work`, `Evaluation`, `WrapUp` | Diagnostic; redundant with `container` and cheap |
| `jti` | a UUIDv4 | Replay attribution ([A6](#a6-audit)) |

**Taken 2026-08-04 ([D3](#decisions-taken-2026-08-04)).** `workload` is computed
**by the issuer**, not by a cloud-side attribute mapping.
The alternative — map `attribute.workload = assertion.project + ':' +
assertion.job_type + ':' + assertion.container` in CEL — works, and it puts the
definition of the policy key
in the terraform of every consumer that federates with us. Emitting it as a
claim means one definition, in one place, testable as a pure function in the
dispatcher, and a mapping that is a straight copy. The cost is honest: adding a
component to the composite later is a token change *and* a re-tag of every
existing binding, whereas the CEL version could be edited cloud-side. Given that
the composite's components are exactly the three boundaries above, that
inflexibility is a feature.

Deliberately **not** in the token: `base_ref`, the branch name, the commit SHA,
the evaluator's `stage`, the job description. Each is either mutable within a
job (so a policy resting on it is a policy resting on nothing) or free text (so
it is an injection surface into a CEL expression). Free text never enters a
claim — the same argument [#311 Decision
5](./311-job-inputs.md#decision-5-injection-safety) makes for its input charset.

### The cloud-side shape this enables

```hcl
# The second provider, alongside beacon's existing GitHub Actions one.
attribute_mapping = {
  "google.subject"       = "assertion.sub"
  "attribute.project"    = "assertion.project"
  "attribute.job_type"   = "assertion.job_type"
  "attribute.workload"   = "assertion.workload"
}
attribute_condition = "assertion.project == 'kasofsk/beacon'"
```

and the binding that expresses the operator's sentence:

```text
principalSet://iam.googleapis.com/projects/{n}/locations/global/
  workloadIdentityPools/{pool}/attribute.workload/kasofsk%2Fbeacon:deploy:work
```

The provider-level `attribute_condition` is the tenancy fence and the binding is
the authority fence — two independent checks, which is the shape Google's own
GitHub Actions guidance recommends (a `repository_owner` condition on the
provider, a `repository` attribute on the binding).

## A2. The signing key

§12.1 already generates a JWT RS256 keypair (`jwt_private.pem`,
`jwt_public.pem`) and `crates/auth/src/jwt.rs` signs platform sessions with it.
Reusing it would cost nothing to build.

**Recommendation: a separate issuer keypair** (`oidc_private.pem` /
`oidc_public.pem`), generated by `chuggernaut init` alongside the existing ones.
(§12.1 lists four; `crates/cli/src/keygen.rs` generates five — see below.)

The precedent is already in the tree and it was created for this exact reason:
`crates/cli/src/keygen.rs` generates a **second age keypair**
(`age_artifacts.key`) distinct from the §8.2 secrets key, with the comment that
the API must decrypt artifacts while the secrets key "stays dispatcher-only" —
a second key minted because a second consumer needed a different boundary. The
same argument applies here, three ways:

1. **Token-confusion is a real class, and the mitigation is free.** A platform
   session JWT (`crates/auth/src/jwt.rs` `Claims { sub, kind, project_roles,
   platform_admin, iat, exp }`) and a workload token differ only by their claim
   set. Sharing a signing key means any validator bug that ignores `aud` or
   `kind` turns one into the other. A cloud STS validating our tokens does not
   know what `kind: user` means and has no reason to reject it. Separate keys
   make the cross-domain forgery unrepresentable rather than merely
   unimplemented.
2. **The rotation schedules are incomparable.** Rotating the session key
   invalidates every live session — an outage-shaped event. Rotating the issuer
   key is, under [A4](#a4-the-public-reachability-problem-the-crux)'s
   recommendation, a terraform apply. Coupling them means the cheaper rotation
   inherits the more expensive one's blast radius, and in practice neither
   happens.
3. **Publishing the public half is a decision about one key, not two.** A JWKS
   is public data by design, but "the key our sessions are signed with is
   published at a stable URL" is a fact worth not creating for free.

Where the halves live, exactly mirroring the split that already exists for
`jwt_public.pem` (`spec.md` Appendix: Infrastructure Summary: "The JWT public
key is also mounted into the API layer for token verification; all other private
keys are dispatcher-only"):

- **`oidc_private.pem` — dispatcher only**, mounted at runtime, **never in NATS
  KV** (§12.1's rule, unchanged and not bent by anything here).
- **`oidc_public.pem` — mounted into the API layer** too, because the API is
  what would serve `/.well-known/jwks.json` if the operator ever exposes it
  ([A4](#a4-the-public-reachability-problem-the-crux)). It is public data; it
  needs no protection beyond integrity.

Algorithm: **RS256**, matching §7.1 and the existing keygen, because it is what
every WIF implementation accepts without argument and what `jsonwebtoken`
already does in this tree. A `kid` (RFC 7517 §4.5) is optional in the abstract
and required here — the 8-key upload allowance is what makes overlapping
rotation cheap, and a multi-key set without `kid`s forces the verifier to try
each. It must be stable across restarts, so derive it from the public key's
SHA-256 thumbprint
rather than minting a random one, so the same key always yields the same `kid`
and a re-run of `init` on existing files is genuinely idempotent, as §12.1
requires.

**Fixed by slice S1: that thumbprint is the RFC 7638 JWK thumbprint**
(SHA-256 over the canonical `{"e","kty","n"}` JSON, base64url without padding),
not a digest of the raw SubjectPublicKeyInfo — `auth::oidc::kid_from_public_pem`,
pinned to RFC 7638 §3.1's published example as a known-answer test. Both forms
are stable; only this one is reproducible by a JWKS consumer holding the
published JWK, which is the party that must agree with us about the id. S5's
JWKS route and S2's token header both take the `kid` from that function rather
than recomputing it.

**The rejected option, honestly:** reusing `jwt_private.pem` is one fewer key to
generate, mount, back up (`deploy/prod/backup-r2.sh`) and lose. For a
single-tenant platform issuing tokens to one cloud, the confusion risk is small
and the operational saving is real. It is rejected because the cost of the
second key is a dozen lines in `crates/cli/src/keygen.rs` and one more mount,
and because §12.1's "skip if files already exist" makes adding a key to an
initialized platform a no-drama operation.

## A3. Lifetime, audience, replay, and delivery

### Lifetime

The provider constraint, verified: the token's `exp` must exceed its `iat` by
**at most 24 hours**. That is a ceiling, not a target.

§7.4 already answers "how long is a per-task credential valid" for the two
credentials that exist: "At each container launch, the dispatcher issues two
short-lived credentials valid for `task_timeout`", implemented as `creds_ttl` —
the resolved task timeout — threaded into both `container_env` and
`ssh_credential_files` in `crates/dispatcher/src/exec.rs`.

> **TTL = `min(resolved task_timeout, oidc_token_ttl_secs_max)`, with
> `oidc_token_ttl_secs_max` defaulting to 3600.**

One rule for all three credentials is worth more than a bespoke one, and the
alternative — a fixed short TTL with in-flight re-minting — needs a channel that
does not exist (a running container cannot ask the dispatcher for a fresh
credential; §7.4's per-job NATS permissions carry no such subject) and would be
new surface for a problem the cap already bounds. The cap is required by
STYLE.md Tier 2 #3 and keeps us an order of magnitude under the provider's 24h
rule even for a job type that sets `task_timeout: 12h`.

The residual, named: a `deploy` job with `task_timeout: 30m`
(`.chug/jobs/deploy.yaml`) holds a 30-minute token. In practice it is exchanged
once in the first seconds for a cloud access token; the OIDC token then sits
unused in a file for 29 minutes. That is a wider window than strictly necessary
and it is the price of not building a refresh channel. If it ever matters, the
narrowing is to make the TTL a per-identity field ([A5](#a5-per-container-scoping))
rather than to invent re-minting.

### Audience

`aud` is the full provider resource name, which is GCP's default and what
`--allowed-audiences` overrides. Verified limits: at most **10** audiences per
provider, each at most **256 characters**.

> **One token per (container, declared identity), each with exactly one
> audience. Never a multi-audience token.**

A multi-audience bearer token is a token that any one of its audiences can
replay at another. Since the token is minted per container at launch and a
container declaring two identities simply receives two files, there is no reason
to build one.

### Replay

Say the honest thing first: **this is a bearer token, and nothing prevents
whoever holds it from using it within its lifetime.** The controls are
containment, not prevention:

- **`aud`** pins it to one provider, so a token leaked out of the platform is
  useless at any other federation endpoint.
- **`exp`** bounds the window, per the TTL rule above.
- **The attribute condition and the IAM binding** bound what it can *become*: a
  stolen `work`-container token for `kasofsk/beacon:deploy` can impersonate
  exactly the SA that binding names and nothing else. This is the control that
  actually matters, and it is why [A1](#a1-what-a-workload-token-identifies)
  spends its effort on the claim set.
- **`jti` plus the audit record** ([A6](#a6-audit)) makes a replay *attributable*
  after the fact. Not a prevention; say so.

**What stops a token leaking from one container into another's use** is that
each container gets its own, minted at its own launch, with its own `container`
claim — there is no shared token, no job-scoped token, and nothing inherited.
Two containers of the same job hold different tokens whose claims differ, and a
cloud-side binding on `attribute.workload` distinguishes them.

### Delivery: an injected file, not an env var

`crates/dispatcher/src/exec.rs` already ships the exact pattern in
`ssh_credential_files`: the per-job SSH key is delivered as a
`container::InjectedFile` at `/chuggernaut/ssh/id` with `mode: 0o600`, the cert
beside it at `0o644`, and an env var (`GIT_SSH_COMMAND`) that *points at* them.
No secret in the env.

> **Two injected files per granted identity, plus one env var:**
>
> - `/chuggernaut/cloud/{identity}/token` — the JWT, `mode: 0o600`
> - `/chuggernaut/cloud/{identity}/adc.json` — the external-account credential
>   config naming the audience and `credential_source.file`, `mode: 0o644` (it
>   contains no secret)
> - `GOOGLE_APPLICATION_CREDENTIALS` — set to the `adc.json` path **only when
>   exactly one identity is granted**. With more than one it is not set at all,
>   and the script names the path it wants: a silent "first one wins" would make
>   which credential a build used depend on map ordering.

Three reasons this beats an env var, in order of weight:

1. **Every Google client library already reads this shape.** The
   external-account/ADC flow is `credential_source: { file: … }`; `gcloud`,
   `google-auth`, and `docker-credential-gcr` all consume it with no glue. An
   env-delivered raw JWT would need a shim in every build script.
2. **`/proc/{pid}/environ` is readable by any process of the same uid.** That is
   already noted as the sharp edge of #309 §8 for host mode; it is also true of
   any sibling process inside a container. A file at 0600 is not a boundary
   against the task's own code — nothing is — but it keeps the credential out of
   the one place that leaks by default into `ps`, crash dumps and child
   processes.
3. **It sidesteps the env-namespace question entirely.** `container_env`
   already composes platform variables, `CHUG_*` stamps, vars and secrets into
   one `HashMap` with an implicit precedence order (vars inserted before
   secrets, so a same-named secret wins), and
   [#311](./311-job-inputs.md#decision-4-delivery-via-one-reserved-env-namespace)
   counts those four and is proposing a fifth. Not adding a sixth is worth
   something.

The one env var that *is* added, `GOOGLE_APPLICATION_CREDENTIALS`, is a path,
not a credential, and is vendor-named rather than `CHUG_`-prefixed on purpose:
its whole value is that unmodified tooling finds it.

## A4. The public-reachability problem (the crux)

A cloud STS must validate our signature. The standard path is `issuer_uri` plus
a publicly fetchable `/.well-known/openid-configuration` and JWKS. The tree says
that path is closed today, and says it twice: `deploy/prod/README.md` §5a serves
the platform over **Tailscale Serve** (tailnet-only) and states outright that
"**`funnel`** would expose it publicly — do not use it here"; and the native api
binds `127.0.0.1:8080` (`deploy/prod/run-api.sh`). At the time of writing there
was no `.well-known` route in `crates/api/src/lib.rs` and no occurrence of
`jwks` anywhere in the tree; slice S5 has since added both routes
(`crates/api/src/oidc.rs`, spec §6.7) and changed **nothing** about the bind or
what is exposed, which is exactly the split
[What ships as code regardless](#what-ships-as-code-regardless) draws.

### The option that removes the requirement

**Verified, and it is the recommendation.** Google Cloud's OIDC workload
identity pool providers accept an **uploaded JWK set** instead of fetching one:

- `gcloud iam workload-identity-pools providers create-oidc` takes
  `--jwk-json-path`, "an optional file containing jwk public keys", in RFC 7517
  format.
- Google's own guide for self-hosted Kubernetes clusters uses exactly this path
  and states: *"The cluster doesn't need to be accessible over the internet."*
- The provider-configuration guide states that JWKS access may be provided by
  auto-discovery *or* by uploading the file directly, and caps the local upload
  at **8 keys**.
- `--issuer-uri` remains **required** in both cases. It is an identifier that
  must equal the token's `iss` — not necessarily a URL anyone fetches.

> **Decided 2026-08-04 ([D1](#decisions-taken-2026-08-04)): register the
> provider with an uploaded JWK set and `--issuer-uri
> https://chug.kasofsk.xyz` — a stable identifier we control, not a URL anyone
> fetches. Ship no public endpoint.**

The issuer URI is fixed to that string by the decision, and slice S6's terraform
must use it verbatim: it is the token's `iss`, so changing it later invalidates
every registered provider at once. It is the hostname
[`deploy/prod/README.md`](../../deploy/prod/README.md) §5b already names for the
Cloudflare Tunnel — deliberately the same name, so the identifier stays right
whether or not that tunnel is ever pointed at a JWKS path.

This is the whole crux dissolved: no Funnel, no tunnel, no relay, no inbound path
to `gumbo-mini-0`. The costs are real and small, and both are **operator**
costs rather than code:

- **Rotation becomes a terraform apply.** Uploading a new key is
  `providers update-oidc --jwk-json-path`, per consumer provider. The 8-key
  allowance makes overlapping rotation trivially affordable — publish both, wait
  out the longest token TTL (≤1h by [A3](#a3-lifetime-audience-replay-and-delivery)),
  retire the old. It does mean an emergency key revocation is a cloud-console
  action, not a service restart. Say that plainly to whoever owns the terraform.
- **The upload is not validated at create time.** Google's guide warns: "The
  command doesn't validate the cluster's JWKS. If the JWKS is malformed or
  expired, subsequent authentication attempts might fail with an error message
  `Error connecting to the given credential's issuer`." That error names the
  issuer and is therefore actively misleading when the real fault is a bad
  upload. Put it in the runbook.
- **It is a per-provider fact, so it does not generalize for free.** Uploaded
  JWKs were verified for **GCP only**. Whether any other federation endpoint we
  might later want (AWS, Azure, a registry's own OIDC) accepts a key set rather
  than fetching one was not checked and must not be assumed. So this
  recommendation buys GCP, and its scope is exactly that.

### The options it displaces, weighed anyway

Because the third bullet above means a second cloud *might* reopen this, all
three are priced.

**Tailscale Funnel on the JWKS paths only.** *For:* zero new infrastructure —
Tailscale is already the transport. *Against:* `deploy/prod/README.md` §5a
explicitly rules Funnel out for this host, and Funnel's unit of exposure is a
host/port, not a path prefix, so "only the JWKS" would require a second
listener serving only those two routes. Doing that properly is a small
purpose-built process — at which point option 3 is simpler and safer.

**Cloudflare Tunnel with a path-scoped ingress.** *For:* §5b already documents
`cloudflared` on this Mini, and its `ingress` rules match on hostname **and**
path, so a rule for `/.well-known/*` on a dedicated hostname routed to
`localhost:8080` with a `http_status:404` catch-all is a genuinely narrow
exposure. *Against:* the tunnel process becomes load-bearing for cloud auth
(if it is down, no job can authenticate); §5b's Cloudflare Access policy must
be scoped *off* those two paths, since an STS cannot complete an SSO challenge —
which is precisely the kind of "one exception in an allow-everything-else
policy" that goes wrong quietly. This is the best option if a public endpoint is
ever actually required.

**A static relay (recommended over the other two if a public endpoint is ever
needed).** JWKS and discovery are static, public, integrity-only documents.
Publish them as two objects behind a custom domain and point `issuer-uri` at
that domain. `deploy/prod/README.md` §4 already provisions **Cloudflare R2** for
backups, so the credential, the tooling and the account exist. *For:* zero
inbound path to the Mini, nothing to keep running, and the availability of cloud
auth stops depending on the platform being up. *Against:* the published set is
now a thing that can go stale independently of the key on disk — so key rotation
must publish before it signs, and that ordering wants a check (a startup assert
that the fetched JWKS contains the live `kid`, warned about loudly rather than
fatally). Also `iss` then names a bucket domain rather than the platform, which
is honest but reads oddly in an audit log.

### What would change D1

Stated as triggers, so a future reader can check them rather than re-argue the
section. The three options above are priced precisely so that hitting a trigger
is a *choice among them*, not a redesign.

- **A second federation endpoint enters the picture** — AWS, Azure, or a
  registry with its own OIDC. This is the live one: the third bullet above says
  uploaded JWKs were verified for **GCP only and must not be assumed**, and
  nothing since has widened that. If the new endpoint insists on fetching a JWKS,
  D1 does not cover it and the choice is the Cloudflare Tunnel (best if a public
  endpoint is genuinely required) or the R2 relay (best if availability of cloud
  auth must not depend on the platform being up). Do not re-argue Funnel:
  [`deploy/prod/README.md`](../../deploy/prod/README.md) §5a rules it out for
  this host on its own grounds.
- **Google withdraws or deprecates `--jwk-json-path`.** The one capability this
  decision leans hardest on. [Verification of the provider
  claims](#verification-of-the-provider-claims) already says to re-read the
  provider docs before the terraform is written; that re-read is the check.
- **The number of consumer providers stops being small.** Rotation is a
  per-provider `providers update-oidc`, so its cost is linear in providers. At
  one or two that is a terraform apply; at a dozen it is a fleet operation, and a
  fetchable JWKS starts paying for itself.
- **An emergency key revocation is needed and the cloud-console round trip is
  too slow.** D1 accepts that revocation is an operator action rather than a
  service restart. If that latency is ever exercised in anger, it is evidence,
  not an argument — record it.

Nothing about the *code* changes under any of these: the discovery and JWKS
routes ship regardless (next section) and the `issuer-uri` string is the same
either way. Hitting a trigger is an infrastructure decision, which is exactly
why D1 was affordable to take.

### What ships as code regardless

**Serve `/.well-known/openid-configuration` and `/.well-known/jwks.json` from
the api, unauthenticated, and leave them unexposed.** They are a few dozen lines
over a public key the api would already hold ([A2](#a2-the-signing-key)), they
are the only part of this that *is* a code change, and shipping them turns "a
second cloud needs a reachable issuer" from a design question into an operator
choosing one of the three options above. The `issuer-uri` registered in
terraform is the same string either way.

**Separate the two plainly, because the brief asks for it:** the endpoints are
code; **exposing them is infrastructure and an operator action**, and under this
recommendation it is an action nobody has to take.

### The honest fallback: a service-account key in the age secret store

It works today, with **zero** new code. `chuggernaut admin secret set` puts the
JSON key in age-encrypted NATS KV (`crates/store/src/secrets.rs`), the job type
names it under `work.secrets`, and `container_env` decrypts and injects it — the
exact mechanism `.chug/jobs/deploy.yaml` uses for `MINI_DEPLOY_KEY`, whose
script writes the injected value to a 0600 tempfile (`.chug/tasks/deploy.sh`).
A build script would write the SA key the same way and `gcloud auth
activate-service-account --key-file`.

**It is not recommended, and the reason is not purity.** A long-lived SA key is
the credential class WIF was adopted to remove: it does not expire, it cannot be
scoped by job type or container beyond which job type declares it, its
compromise is silent, and rotating it is a manual operation across two systems.
Since [the inline JWK set](#the-option-that-removes-the-requirement) removes the
blocker that would have justified it, the bridge is unnecessary.

The one case where it is defensible: **half B ships before half A** because the
WIF provider registration is blocked on someone else's terraform. Then one
narrowly-scoped, calendar-rotated SA key for one job type, with a dated note in
the job-type file saying what retires it, is a reasonable trade for not blocking.
It must be deleted in the commit that lands the provider, not "later".

## A5. Per-container scoping

Secrets are already scoped per container and nothing is inherited: `work.secrets`,
each `eval[].secrets` (whose doc comment in `crates/types/src/job_type.rs` reads
"the only container they reach; not inherited from `work.secrets`"), and
`wrap_up.secrets`. `container_env` is called per launch with exactly the
declared list for that container.

> **A cloud identity is declared exactly where a secret is, with exactly the
> same non-inheritance rule.**

```yaml
work:
  type: command
  run: ./.chug/tasks/build-image.sh
  secrets: [MINI_DEPLOY_KEY]
  workload_identities: [gcp-artifact-writer]
eval:
  - name: health
    type: command
    run: ./.chug/tasks/deploy-health.sh
    stage: 0
    secrets: [DEPLOY_HEALTH_API_TOKEN]
    # no workload_identities: this evaluator gets no cloud credential
```

### Named reference, or inline cloud configuration?

**Option A — inline:** `workload_identities: [{ audience: "//iam.googleapis.com/…",
service_account: "deployer@…" }]`. *For:* the blast radius is entirely readable
from the job-type file, which is the property
[#311 Decision 1](./311-job-inputs.md#the-classification-the-brief-asks-for)
protects when it forbids computed secret names. *Against:* it puts cloud
resource names and project numbers in the repo, it has nothing to validate
against (a typo'd audience fails at exchange time inside the container), and
re-pointing a project at a different cloud project becomes a commit in every job
type.

**Option B (taken 2026-08-04, [D2](#decisions-taken-2026-08-04)) — a named
reference to a project-scoped record**, admin-
managed exactly as secrets are (CLI-only; there is no HTTP route for secrets in
`crates/api/src/lib.rs`, and `crates/cli/src/admin.rs` owns `SecretCmd`). Store
at `cloud-identities.{owner}.{project}.{name}` holding `{ audience,
service_account, token_ttl_secs? }` — **not** secret data, so it can live
beside vars rather than under age.

`service_account` lost its `?` with [D4](#decisions-taken-2026-08-04): the
exchanged token impersonates a service account, mirroring what beacon's existing
bindings already grant to, so every record populates the field rather than
leaving it optional-in-practice. The alternative D4 rejects — binding the
`principalSet` directly to the resource, with no SA in the middle — would make
the field genuinely optional and is the cheaper posture eventually; it is
rejected *now* because adopting it means re-authoring beacon's bindings instead
of adding a second provider beside them, which is a larger blast radius than
this work is buying.

The readable-from-the-file property survives: `workload_identities:
[gcp-artifact-writer]` is a static name in a reviewed file, exactly as
`secrets: [MINI_DEPLOY_KEY]` is. What you must look up is *what the name
unlocks*, which is equally true of the secret today. What Option B buys that
Option A cannot:

- **Release validation catches a typo.** §2.2's rule for secrets — "Every secret
  named in `secrets:` (`work.secrets` and per-evaluator) has an entry in the
  `secrets.*` KV bucket" — extends verbatim to a `cloud-identities.*` bucket, and
  `crates/dispatcher/src/release.rs` already emits the shaped per-name error
  (`secret '{name}' is not set`, `crates/dispatcher/src/release.rs:193`). A
  misdeclared identity fails at release with a fixable message rather than
  inside a build container 40 minutes later.
- **The cloud coordinates are operator data.** Rotating a pool, moving to a new
  GCP project, or re-pointing at a staging provider is an `admin` command, not a
  job that has to pass CI to change a string.

Both are defensible; do not ship both readings. **Option B is the one taken**,
and Option A stays above as the reasoning that makes the choice checkable — its
"readable from the job-type file" property is real, and the paragraph before
this one is why it survives the named reference.

### The `global/agents` hazard, stated as a rule

`inject_platform_agent_secrets` (`crates/dispatcher/src/exec.rs`) lists **every**
secret under the reserved `global/agents` scope and inserts each into the env of
every agent container — work agents (`exec.rs`) and agent evaluators
(`crates/dispatcher/src/eval.rs`) alike, with `entry().or_insert()` so declared
secrets win on collision. It is the platform's one blanket grant, and it exists
for provider credentials (the agent CLI's own plumbing), which is why command
containers deliberately never receive them.

> **Rule: a cloud identity is never expressible as a secret, and therefore can
> never ride the `global/agents` grant.**

This is the structural reason [A5](#a5-per-container-scoping) puts identities in
their own declaration with their own KV namespace rather than reusing
`secrets:`. Had they been secrets, `chuggernaut admin secret copy --to
global/agents --name GCP_KEY` — a documented, one-line operation
(`crates/cli/src/admin.rs`) — would hand a cloud credential to every agent
container in every project on the platform. Nothing in the current code would
object. Keeping the mechanisms disjoint makes that command incapable of
expressing the mistake, which is worth more than a warning in a doc.

The residual, named honestly: this does not stop someone putting a raw GCP
service-account key into `global/agents` as an ordinary secret. Nothing
mechanical can — the platform cannot tell a JSON blob apart from another JSON
blob. What it does mean is that the *supported* mechanism has no path there.

### Skew: this field costs an epoch bump

`work`, `Evaluator` and `wrap_up` are nested blocks and carry
`#[serde(deny_unknown_fields)]` (`crates/types/src/job_type.rs`; the top-level
struct deliberately does not). So an N−1 dispatcher reading a job type with
`work.workload_identities` **hard-rejects the config** and, per §14.2, parks
every job of that type.

That is the correct failure — a silently-dropped credential declaration would
mean a job that runs without the credential it declared, hitting an
authentication error deep inside a build — but it forces the same
discipline #309 and #311 both land on:

> Bump `CONFIG_SCHEMA_EPOCH` in the **same commit** as the parser change, and add
> a `validate()` rule that a non-empty `workload_identities` requires
> `min_dispatcher >=` the new epoch, reported as an ordinary
> `FieldRuleError::Required`.

**The number, re-derived 2026-08-04 against
[`crates/types/src/version.rs`](../../crates/types/src/version.rs):**

> **The bump is `4 → 5`**, with a frozen `WORKLOAD_IDENTITY_SCHEMA_EPOCH = 5`
> beside the three constants already there.

This paragraph previously said "currently `1`" and cited #309 §3 and #311 as
both proposing `1 → 2`. All three claims are stale. #309 §3 has since been
corrected (it names `3 → 4` after job #401) and so has #311's opening (it names
`4`), but **#311's own Skew section still reads `CONFIG_SCHEMA_EPOCH = 1` and
its phasing table still carries `1 → 2` cells** — so a slice author who reads
that section rather than this one will inherit the same stale number.
Correcting #311 is a separate docs job; the tree is the authority, and it holds
`CONFIG_SCHEMA_EPOCH = 4`, with
`INPUTS_SCHEMA_EPOCH = 2` (#311 slice A), `SCHEDULE_INPUTS_SCHEMA_EPOCH = 3`
(#311 slice C, job #376) and `RUNTIME_SCHEMA_EPOCH = 4` (#309 §3 / #373,
job #401) frozen behind it. A5's own instruction was that whichever of the three
docs lands last re-derives the number; this is that re-derivation, and the rule
it followed is the one this section always stated — **read the constant, do not
copy a number out of a brief or a sibling doc.**

The frozen per-feature constant is not optional decoration. §14.2 says
explicitly that those constants, "not `CONFIG_SCHEMA_EPOCH`, are where a reader
finds which epoch bought what", and each is frozen so a later bump for an
unrelated feature never retroactively raises what an existing config must
declare. `workload_identities` gets `WORKLOAD_IDENTITY_SCHEMA_EPOCH` for exactly
that reason, and `JobType::validate` compares against it rather than against
`CONFIG_SCHEMA_EPOCH` — the precedent the other three exist to set.

Two things this does **not** change. §14.3's gate reads the *deployed* epoch
live, so a declaration that is stale by the time a slice lands fails that
slice's own CI rather than shipping — the re-derivation above is a convenience
for the author, never the enforcement. And if another epoch-spending change
ships in the same deploy generation, one bump still covers both; slice S3 should
re-read the constant at implementation time exactly as this paragraph just did.

## A6. Audit

§10.3 names the NATS event stream as "the primary audit log for all execution
activity", and §10.2 is unambiguous that plaintext credentials never reach task
records, logs or event streams. A JWT is a bearer credential. So:

> **The token is never recorded. Its identity is.**

At mint time the dispatcher records, on the task record (`tasks.*` KV) and on
`task-started` (§6.3, the event that fires once the launched task reaches
Running):

| Field | Example | Why |
| --- | --- | --- |
| `identity` | `gcp-artifact-writer` | Which declaration was honored |
| `audience` | `//iam.googleapis.com/projects/…/providers/chuggernaut` | Which provider it was valid at |
| `sub` | `project:kasofsk/beacon:type:deploy` | The policy identity presented |
| `workload` | `kasofsk/beacon:deploy:work` | The composite policy key |
| `jti` | a UUIDv4 | The join key (`uuid` is already a workspace dependency, used in `crates/dispatcher`) |
| `expires_at` | RFC 3339 | The window |

The fields are **optional and omitted** when a task minted nothing, following
§6.3's own precedent for the channel-post origin fields ("All three are optional
for back-compat … old consumers render it exactly as before"). A job type
declaring no identities therefore emits today's event unchanged.

**`jti` is the field that earns the design.** Google Cloud Audit Logs record the
STS exchange and the impersonated identity on their side; our record says which
job, type, container and task minted the token that was exchanged. Joining the
two answers "which chuggernaut job did this cloud action" without either system
needing to know about the other — which is the question an incident actually
asks, and one that a service-account key cannot answer at all.

The mint itself is a §7.4-shaped launch-path concern, so it goes where the
existing two credentials go: composed in `crates/dispatcher/src/exec.rs`, on the
single-writer path, once per container launch. Per STYLE.md Tier 2 #2 (assert
negative space), assert at the injection site that no identity file is written
for a container whose resolved declaration is empty — the inverse of #311's
collision assert, and the one that would catch an inheritance bug.

---

# Half B — image build and push

## B1. The build mechanism

[Decision 0](#decision-0-the-308-vs-309-contradiction-resolved) rules out both
of #308's framings as stated. Four candidate shapes, priced against the tree.

**B-I — raw docker socket bound into a pinned job type's containers (#308 D1).**
*For:* it is the cheapest thing that works and it reuses the node's BuildKit
cache unchanged. *Against:* [correction
5](#corrections-verified-against-the-tree) — the socket yields `docker inspect
chug-worker`, the host path of its `:ro` key mount, and from there the node's
NATS credential and git key; §3.1 puts other jobs' per-job credentials **inline**
on the subjects that credential subscribes. It also breaks §10.1's "No host
volume mounts" and narrows §3.1's single documented bind exception, which is
justified there by carrying *no job state*. Rejected.

**B-II — a rootless builder (buildkit/kaniko) inside an ordinary job container
(#308 D2).** *For:* strictly better isolation than GHA offers, and no platform
rule to break. *Against:* two verified costs #308 does not price. First,
`ContainerLaunchConfig` carries no security options ([correction
4](#corrections-verified-against-the-tree)), and rootless buildkitd in a
container generally needs relaxed seccomp/apparmor — so this is a launch-path
schema change, not a job-type change. Second, the local cache becomes a
**registry** cache, and [correction
2](#corrections-verified-against-the-tree) says there is no registry: the option
whose cost is "you now need a registry round-trip" is being weighed in a repo
that would have to stand up the registry first. (kaniko additionally should not
be picked up new; treat buildkit as the only live variant.) Rejected as the
first move; it remains the right answer for a fleet that later wants builds on
arbitrary nodes.

**B-III — a build op on the worker daemon.** The `refresh` precedent
([correction 3](#corrections-verified-against-the-tree)) generalized: the
dispatcher asks the daemon to build; the daemon fetches the context and runs
`docker build` with fixed flags. *For:* the best isolation of the four — project
code supplies a Dockerfile and a context and nothing else; no socket enters any
job container; the daemon controls the tag namespace and the flags. *Against:*
it needs a new dispatcher-side effect and a second execution lifecycle inside
the daemon (a build is not a task, so its timeout, log tail, cancellation and
crash recovery are all new code paths that duplicate what the task machinery
already does). That is a lot of platform surface for something the next option
gets most of.

**B-IV (recommended) — a narrowed docker API endpoint, bound node-side into
containers of an allow-listed (project, job type) on a pinned builder node.**
The daemon runs a filtering proxy in front of its socket. Deny-by-default, with
a permit list of exactly what a build needs: `POST /build`, `POST /session` (the
BuildKit session the `DOCKER_BUILDKIT=1` client opens alongside the build — omit
it and builds fail confusingly), `POST /images/{name}/push`, `POST
/images/{name}/tag`, and `GET /images/{name}/json`. Denied: everything under
`/containers`, `/exec`, `/volumes`, `/networks`, `/swarm`, and `/info`. Job
containers of the allow-listed type get that proxy's socket bound in at
`/var/run/docker.sock`; every other container gets nothing, as today.

*Why it is the recommendation:*

- **It is #309 §10's prescribed shape verbatim** — "a node-side allow-list
  entry, never a job-type field the platform honors on request" — so it resolves
  the contradiction by *complying* with the rule rather than carving it out.
- **It is dispatcher-ignorant and schema-free.** The bind is added worker-side,
  which is exactly the mechanism §3.1 already blesses for `WORKER_CACHE_DIR`
  ("a node property added worker-side, not a launch input"). No wire field, no
  `ContainerLaunchConfig` change, no epoch bump. Half B costs the dispatcher
  nothing.
- **It removes B-I's blast radius specifically.** The lethal capability in
  correction 5 is `POST /containers/create` with a bind — denied. `POST /build`
  gives arbitrary code execution *in a build container*, which is the same class
  of thing every job container already is, not host root. BuildKit's dangerous
  entitlements (`security.insecure`, `network.host`) are off unless the daemon
  is started with `--insecure-entitlement`, which it must not be.
- **It keeps the cache for free** — see [B3](#b3-build-cache).

*What it costs, honestly:*

- **A proxy is a filter, and filters are only as good as their rule list.** The
  docker API is large and grows; a permitted verb that later gains a
  container-creating side effect re-opens B-I. Pin the proxy image by digest,
  keep the allow-list deny-by-default, and treat it as security-relevant
  configuration — a node-config review item, not a convenience.
- **Cross-project cache poisoning is real.** A build can write to a BuildKit
  cache mount another project's build later reads. Mitigate with per-project
  cache `id`s, which `deploy/prod/Dockerfile.worker` and
  `deploy/prod/Dockerfile.agent-rust` already demonstrate (`id=chug-worker-target`,
  `sharing=locked`). Do not pretend the shared cache is a boundary.
- **It is invisible to config, so a mis-placed job fails at the build command.**
  A job type needing the builder that lands on a node without the allow-list
  gets `Cannot connect to the Docker daemon` — loud, but late, and diagnosable
  only from the container log. The interim answer is `placement.node` (§3.1),
  which ships today. The durable answer is #309's `NodeCapabilities`: add
  `builder` to the advertised set and let `choose_placement` filter, so an
  unbuildable fleet says `NoCapacity("no node advertises builder")` instead. That
  is a real dependency on #309 landing — for the *diagnostic*, not for the
  mechanism.

**If the proxy's allow-list proves too coarse, escalate to B-III**, not to B-I.
The two share the "the node builds, the container asks" model; only the
narrowness of the ask differs.

## B2. Registry auth falls out of half A

GHA's keyless push is: exchange the OIDC token for a cloud access token, then
install a docker credential helper. The equivalent here is the same shape with
half A's file in place of GitHub's token endpoint:

```sh
# .chug/tasks/build-image.sh, sketch
gcloud auth login --cred-file="$GOOGLE_APPLICATION_CREDENTIALS"   # external account
gcloud auth configure-docker "${REGION}-docker.pkg.dev" --quiet
docker build -t "$IMAGE:$SHA" .
docker push "$IMAGE:$SHA"
```

Two properties worth stating because they are not obvious:

- **The credential stays with the client, not the daemon.** `docker push` sends
  registry auth in the request (`X-Registry-Auth`); the daemon performs the
  network call using credentials the *container* supplied. So B-IV's proxy shape
  is compatible with per-job registry credentials — the build node never holds a
  standing push credential, which is the whole point of half A.
- **The IAM binding is per (project, job type, container)** by
  [A1](#a1-what-a-workload-token-identifies), so "only the `build-image` job
  type of `kasofsk/beacon`, and only its work container, may push to this
  repository" is expressible cloud-side. A build's push rights and a deploy's
  impersonation rights are separate bindings on separate `attribute.workload`
  values.

**And the registry still has to exist.** Per [correction
2](#corrections-verified-against-the-tree), this repo has none and the spec's
Infrastructure Summary leaves the row open. It is **operator work with no code
in it**, and half B cannot ship without it.

What changed on 2026-08-04 is the likely size of that work, not its nature:
beacon already pushes to **Artifact Registry** from six composite actions
(secondhand, per correction 2), which is the same registry "to match where the
WIF work already points" was pointing at anyway. So the operator item is
probably "confirm the repository and grant a second writer" rather than "choose
and provision" — but it has not been confirmed here, and half B stays blocked on
it either way.

## B3. Build cache

The node-local cache that ships today is narrower than "a cache mechanism":
`WORKER_CACHE_DIR` (`crates/worker/src/config.rs`) is one host directory,
bind-mounted at one fixed container path (`CACHE_MOUNT_PATH = "/cache/sccache"`,
`crates/container/src/docker.rs`) into every container the node launches, with
`RUSTC_WRAPPER`/`SCCACHE_DIR` injected worker-side
(`crates/worker/src/daemon.rs`). §3.1 justifies it as the one permitted
bind-mount exception precisely because it "carries **no job state** … safe to be
empty/cold, and never affects correctness".

**Does it generalize to a buildx cache? It does not need to** — and that is the
strongest single argument for B-IV over B-II.

With the build running on the node's own daemon, the build cache is **BuildKit's
cache, which already exists on these nodes, is already exercised on every
deploy, and already has an eviction policy**:

- `deploy/prod/Dockerfile.worker` and `deploy/prod/Dockerfile.agent-rust` use
  `RUN --mount=type=cache` for the cargo registry and target with per-image `id`
  and `sharing=locked`.
- `deploy/prod/build-worker.sh` and `deploy/prod/worker-refresh.sh` run builds
  under `DOCKER_BUILDKIT=1` and prune with `docker builder prune -f
  --keep-storage 15GB`.
- `deploy/prod/README.md` §6 records why: a cold Rust image build was ~10
  minutes, and the cache mounts cut a SHA bump to the changed crates.

So the answer to "does the node-local cache mechanism generalize" is: the
`WORKER_CACHE_DIR` **mechanism** does not — [#309
§9](./309-host-native-execution.md#9-environment-and-state) reaches the same
conclusion for `~/.gradle`-shaped caches, and for the same reason (content
addressing and no-job-state are the properties that made the sccache bind safe,
and a buildx cache satisfies neither cleanly across tenants). But the **need**
evaporates under B-IV, because the cache lives on the daemon side of the proxy
and was already there.

Two obligations follow, both required by STYLE.md Tier 2 #3 (everything is
bounded):

- **Per-project cache ids** in project Dockerfiles, so two projects cannot
  collide or poison one another's mounts.
- **The existing `--keep-storage` prune must cover the new volume.** A builder
  node serving project builds will grow its cache far faster than the platform's
  own three images do; `deploy/prod/worker-refresh.sh` already refuses to build
  below a disk-free floor and says so loudly, which is the behavior to preserve
  rather than replace.

## B4. Tagging discipline, and the rollback handle

The #308 survey found two beacon workflows pushing `:latest` with no SHA tag,
leaving no rollback handle. That defect must not be ported.

> **A build job pushes an immutable, content-identified reference first, and
> moves any mutable alias only after that push succeeds.**

Concretely:

1. The build job pushes `{repo}:{sha}` — the git SHA of the tree that was built.
   Required, and the only thing it pushes.
2. It records the resulting `{repo}@sha256:{digest}` as the task's `structured`
   result (§6.3: `task-completed` "includes `pass` and `structured` where
   applicable"), so the audit record names the bytes rather than a name that can
   be moved.
3. **Moving `:latest` / `:prod` is a different job**, taking the digest as an
   input.

Step 3 is the part worth arguing, because the tempting shape does not exist.
`wrap_up.run` — the post-merge hook — is **valid with `type: merge` only**
(`crates/types/src/job_type.rs`), and a build job is deploy-shaped: its effect is
external and its branch is scratch, so it is `wrap_up: type: none` and has no
wrap-up hook to move an alias in after the gate. Moving the alias inside the
*work* script instead would put it before the gate, which defeats the ordering
this section exists to establish.

So the alias move is its own job type — and it is the same job as rollback with
a different name, which is the tell that the factoring is right. #311's rule
applies verbatim: the type is the unit of authority (pushing a build and
repointing production are different authorities, and under
[A5](#a5-per-container-scoping) they hold different cloud identities), the input
is the unit of target.

**Enforcement is the house pattern, not a platform rule.** The platform does not
know what a build is and should not learn: per CLAUDE.md "the evaluation gates
ARE the CI", the build job type carries a stage-0 command evaluator that asserts
the pushed digest resolves in the registry, and the promote job type carries one
that asserts the alias now points at the digest it was given.
`.chug/jobs/deploy.yaml`'s stage-0 `health` evaluator is the shape to copy — one
gate that alone decides whether the external effect was good, with `wrap_up:
type: none`.

**Rollback and promote are the same job type**, built on
[#311](./311-job-inputs.md)'s `inputs:` — shipped, not pending, and already
carrying [`.chug/jobs/rollback.yaml`](../../.chug/jobs/rollback.yaml):

```yaml
inputs:
  - name: image_digest
    type: string
    required: true
    pattern: '^sha256:[0-9a-f]{64}$'
    description: The image digest to roll back to. Must already exist in the registry.
```

Two things line up here that are worth noting rather than rediscovering. #311's
default input charset is `^[A-Za-z0-9._:/@+-]{1,256}$` and its own examples name
`ghcr.io/org/img:sha` and `img@sha256:…` as shapes it must admit — so an image
reference passes without widening anything. And #311 Decision 5 layer 3 asks
that any input reaching an argv position carry a declared narrowing; a digest
pattern is exactly that, and it is strictly better than a tag pattern because a
tag is mutable and a digest is not. **Prefer a digest input over an `image_tag`
input** wherever the registry gives one: rolling back to a tag rolls back to
whatever that tag points at *now*.

---

## What is code and what is not

The brief asks for this split plainly, so here it is in one place.

| Item | Kind |
| --- | --- |
| Issuer keypair in `chuggernaut init`; mount paths ([A2](#a2-the-signing-key)) | **Code** |
| Token minting, claims, TTL cap, injected files ([A1](#a1-what-a-workload-token-identifies), [A3](#a3-lifetime-audience-replay-and-delivery)) | **Code** |
| `workload_identities:` field, field rules, epoch bump, `min_dispatcher` rule ([A5](#a5-per-container-scoping)) | **Code** |
| `cloud-identities.*` KV records + `chuggernaut admin` commands | **Code** |
| Release-validation existence check for declared identities | **Code** |
| `/.well-known/openid-configuration` + JWKS routes on the api ([A4](#a4-the-public-reachability-problem-the-crux)) | **Code** |
| Audit fields on the task record and launch event ([A6](#a6-audit)) | **Code** |
| Docker API proxy + node allow-list + builder pin ([B1](#b1-the-build-mechanism)) | **Node configuration** (worker-side; no dispatcher code) |
| Registering the second WIF provider, uploading the JWK set, the attribute condition and IAM bindings | **Operator / terraform** |
| Rotating the issuer key (re-upload per provider) | **Operator / terraform** |
| **Deploying** a dispatcher at epoch 5 before any config declares `min_dispatcher: 5` (§14.3, slice S3d) | **Operator** — ordering, not a choice |
| **Exposing** the discovery/JWKS endpoints publicly | **Infrastructure — and not required** under [D1](#decisions-taken-2026-08-04) |
| Confirming the container registry ([B2](#b2-registry-auth-falls-out-of-half-a)) | **Operator** — half B is blocked on it, and it is *plausibly* already satisfied ([correction 2](#corrections-verified-against-the-tree)) |

## Sequencing, and what ships first

**Half A is independently useful without half B, and half B is not useful
without half A.**

Half A alone unblocks #308 category C for anything cloud-touching — a deploy
that calls a GCP API, a terraform plan job, a job that reads a bucket — none of
which involve an image build. It also retires the "no OIDC issuer" row from the
[#308](./308-gha-port.md) gap table (rank 6) **once it ships**, and, per
[A4](#a4-the-public-reachability-problem-the-crux), does so without the infra
prerequisite that gap assumed. Ranks 6 and 11 stay open until then; this
document takes decisions, it does not close gaps.

### Not every category-C deploy needs half A (beacon, secondhand)

Same 2026-08-04 operator inspection, same caveat: `~/beacon` is not in this
workspace, so nothing here re-derives it.

Of [#308 §C](./308-gha-port.md#c-deploys-16-workflows)'s sixteen deploys, **four
need nothing from this document**: creator and homepage, dev and prod. They
deploy to **Cloudflare** and authenticate with a plain `CF_API_TOKEN` through
`wrangler` — an ordinary long-lived API token, which is exactly what
[A5](#a5-per-container-scoping)'s `secrets:` mechanism already carries, scoped to
the work container. Those four are **portable on today's mechanisms**, ahead of
S1, and #308 §C's own "secret blast radius" win applies to them without any of
half A: beacon sets `CF_API_TOKEN` as a workflow-level `env`, and
`.chug/jobs/deploy.yaml`'s per-container scoping is strictly narrower.

The remaining twelve reach GCP and do need half A.

> **The measurement trap, recorded because this repo keeps paying for it:** those
> twelve authenticate **inside the composite actions**, not in the workflow files.
> A grep of `.github/workflows/` alone finds the `wrangler` calls and misses the
> `google-github-actions/auth` step, so it **understates** the dependency —
> half A looks optional when it is load-bearing for three quarters of the
> category. Any survey of what a port needs must read `.github/actions/` too.

Two residual uncertainties, named rather than smoothed over. #308 §C's own
enumeration ("deploy/rollback × {web, worker, bot} × {dev, prod}, plus creator,
homepage, and nats") does not add to sixteen however it is read, so the
4-versus-12 split rests on the inspection rather than on arithmetic anyone can
redo here. And the `nats` deploy's authentication was not reported; it is
counted with the GCP twelve because that is the conservative reading — being
wrong that way costs a redundant identity declaration, being wrong the other way
costs a deploy that fails at authentication.

Half B without half A means a standing registry credential on the builder node
or in the secret store — the regression half A exists to prevent. So the order
is forced.

Slice labels below are `S{n}` deliberately — the `A`/`B` labels above name
*sections* of this document, not units of work.

| Slice | Content | Depends on |
| --- | --- | --- |
| **S1** | Issuer keypair in `crates/cli/src/keygen.rs`; mounts; `kid` from the public-key thumbprint | — |
| **S2** | Token minting + claim assembly as a **pure function**, unit-tested; no I/O. **Shipped** as [`crates/auth/src/workload.rs`](../../crates/auth/src/workload.rs) — see [Where S2 landed](#where-s2-landed) | S1 |
| **S3** | `workload_identities:` on `work`/`eval[]`/`wrap_up`; field rules; **epoch bump `4 → 5` + frozen `WORKLOAD_IDENTITY_SCHEMA_EPOCH = 5`** + the `min_dispatcher` rule in one commit; `cloud-identities.*` KV + admin CLI + release-validation check | S2 |
| **S3d** | ***Deploy:* a dispatcher carrying epoch 5 reaches prod.** Not a code slice and not optional — see the ordering note below | S3 |
| **S4** | Injection at launch (two `InjectedFile`s + `GOOGLE_APPLICATION_CREDENTIALS`), audit fields, the empty-declaration assert | S3 |
| **S5** | Discovery + JWKS routes on the api (unexposed). **Shipped** as [`crates/api/src/oidc.rs`](../../crates/api/src/oidc.rs) (spec §6.7); the bind is untouched | S1 |
| **S6** | *Operator:* register the provider with the uploaded JWK set (`--issuer-uri https://chug.kasofsk.xyz`, [D1](#decisions-taken-2026-08-04)); attribute condition; one IAM binding to the impersonated SA ([D4](#decisions-taken-2026-08-04)); prove it with a trivial `gcloud` command in a work container | S4, **S3d** |
| **S7** | *Operator:* a registry **confirmed** — plausibly already satisfied by beacon's Artifact Registry ([correction 2](#corrections-verified-against-the-tree)); what is left is confirming the repository, its GCP project, and IAM for a second writer | — (runs in parallel with S1–S6) |
| **S8** | *Node config:* proxy + allow-list + `placement.node` pin on one builder node | [Decision 0](#decision-0-the-308-vs-309-contradiction-resolved) only |
| **S9** | A real `build-image` job type: SHA tag, digest recorded, digest-resolves evaluator | S6, S7, S8 |
| **S10** | A `promote` / `rollback` job type keyed on a digest input — [#311](./311-job-inputs.md) slice A shipped, so S9 is the only live dependency | S9 |
| **S11** | Fold `builder` into #309's `NodeCapabilities` so placement filters instead of failing at the build command | [#309](./309-host-native-execution.md) P2 |

**The ordering S3d exists to make unmissable.** S3 spends an epoch, so per
`spec.md` §14.3 the merge-time gate compares a config's declared
`min_dispatcher` against the **deployed** dispatcher's epoch and fails the
config's own CI with "requires dispatcher >= X; deploy first or gate it". So:

1. **S3 itself merges safely.** Nothing in `.chug/jobs/` declares
   `min_dispatcher: 5` — the highest today is `2`
   (`.chug/jobs/rollback.yaml`) — so the bump commit trips no gate.
2. **No config declaring `min_dispatcher: 5` can merge until a dispatcher
   carrying epoch 5 is deployed.** That includes S6's proving job type and S9's
   `build-image`, both of which declare `workload_identities` and therefore must
   declare the epoch.

This is the same shape as the two prior bumps: job #376 spent `2 → 3` and
job #401 spent `3 → 4`, whose commit message records the identical constraint ("a deploy carrying it is the prerequisite for any config
later declaring `min_dispatcher: 4`"). §14.3's gate is the first line of defense
and §14.2's runtime park is the fallback — a config that reaches a launch ahead
of the binary parks every job of its type with `config_schema_skew` rather than
running it without the credential it declared.

S2 being a pure function matters more than it looks: it is the piece a reviewer
must be able to read as a whole, and per STYLE.md Tier 2 #1 and
[contracts.md](../../contracts.md) it belongs on the decider side of the split,
with the mint's I/O (reading the KV record, writing the files) on the effect
side. Test placement per [testing.md](../../testing.md): claim assembly, the TTL
cap, the `sub` length bound and the field rules are pure → **tier 1**; the
launch round trip (declared identity → two files present with the right modes,
undeclared → no files) is **tier 2**.

### Where S2 landed

`crates/auth/src/workload.rs`, not `crates/domain`. A mint is a credential
construction rather than a lifecycle decision: it needs the issuer key and
`jsonwebtoken`, and the pure core resolves neither by construction — the
boundary guard forbids a `domain → auth` edge outright — so the alternative was
either widening that crate's dependency floor or splitting the claim set from
the token it exists to produce. §7.4's other two per-task credentials are minted
in the same crate, and the `kid` comes from `auth::oidc` next door. The decision
this sits beside — *which* identities a container is granted — is the part that
stays on the decider side, in S3 and S4.

What S4 calls, and what it must not undo:

- `WorkloadTokenSigner::new(private_pem, public_pem, issuer)` then
  `mint(&request, now)` → a `MintedWorkloadToken`: `token()` (the credential,
  with no `Debug`/`Display`/`Serialize` route out) and `audit()` (the A6 row
  minus `identity`, which names the declaration and is the caller's).
- `WorkloadTokenRequest` carries only typed identity fields; `audience` is one
  `String`, so a multi-audience token is unrepresentable. `task_timeout_secs`
  and `token_ttl_secs_max` feed the A3 rule, with `TOKEN_TTL_SECS_MAX_DEFAULT =
  3600` and a cap above `PROVIDER_TTL_SECS_MAX` refused rather than clamped.
- `SUBJECT_BYTES_MAX = 127` is a named error, never a truncation, and every
  claim component clears `types::inputs::check_value_charset` plus a ban on the
  `:` and `/` the composites join on.

## Contracts this changes

Per CLAUDE.md's contract-first rule for dispatcher work.

| Contract | Change |
| --- | --- |
| `JobType` | New `workload_identities: Vec<String>` on `work`, each `eval[]` and `wrap_up`; nested blocks keep `deny_unknown_fields`, so this is a **breaking** config change |
| `JobType::validate` | Existence-shape rules for the new field; a non-empty declaration requires `min_dispatcher >= WORKLOAD_IDENTITY_SCHEMA_EPOCH` |
| Epoch | `CONFIG_SCHEMA_EPOCH` **4 → 5** in the same commit as the parser change (§14.1), plus a frozen `WORKLOAD_IDENTITY_SCHEMA_EPOCH = 5` beside `INPUTS_`/`SCHEDULE_INPUTS_`/`RUNTIME_SCHEMA_EPOCH` (§14.2). Re-derived 2026-08-04; slice S3 re-reads the constant at implementation time |
| Deploy ordering | A dispatcher at epoch 5 must be **deployed** before any config declaring `min_dispatcher: 5` can merge (§14.3). Slice **S3d** |
| Release validation | A declared identity must have a `cloud-identities.{owner}.{project}.{name}` record — same shape and message as the existing missing-secret error |
| `cloud-identities` record | `{ audience, service_account, token_ttl_secs? }`; `service_account` is populated, not optional ([D4](#decisions-taken-2026-08-04)) |
| Invariant | Workload identities are **never** secrets and never ride the `global/agents` grant. Structural: disjoint KV namespaces and no `secret copy` path |
| Invariant | Nothing is inherited — a container receives exactly the identities its own block declares. Asserted at the injection site (no file written for an empty declaration) |
| Invariant | Exactly one audience per minted token; no multi-audience token is representable |
| Bound | `oidc_token_ttl_secs_max` (default 3600), and the minted TTL is `min(resolved task_timeout, cap)` — never above the provider's 24h `exp − iat` rule |
| Invariant | The token value never reaches a task record, an event, a log or git (§10.2). Only `identity`/`audience`/`sub`/`workload`/`jti`/`expires_at` are recorded |
| §12.1 | One more generated keypair (`oidc_private.pem` / `oidc_public.pem`); private dispatcher-only and never in NATS KV; public also mounted into the api |
| New api routes | `GET /.well-known/openid-configuration`, `GET /.well-known/jwks.json` — unauthenticated, public-data-only, unexposed by default |
| Golden trace | A job type declaring no identities produces a container file set and env byte-identical to today's — the feature is off, not merely unused |

New modules get a doc header (accepts / emits / guarantees / spec §) and a
`MODULES.md` registry row per the direction-of-travel rule; `.chug/tasks/ci.sh`
enforces the registry.

## What this makes wrong elsewhere

- **`spec.md` §12.1 and the Appendix: Infrastructure Summary** enumerate four
  generated keypairs, and are *already* one behind:
  `crates/cli/src/keygen.rs` also generates `age_artifacts.key`.
  [A2](#a2-the-signing-key) makes six. Fix the omission and the addition in the
  same edit. The Appendix's identity row ("JWT (RS256) + SSH CA + NATS KV")
  gains an issuer.
- **`spec.md` §7.4** says "the dispatcher issues **two** short-lived
  credentials" per launch. With a declared identity it issues three.
- **`spec.md` §10.1's "No host volume mounts"** already has one documented
  exception (§3.1's build cache); [B1](#b1-the-build-mechanism) adds a second,
  node-side and allow-listed. §10.1 should name it rather than leave the rule
  reading as absolute.
- **`.chug/jobs/deploy.yaml`** cites `spec §11` for the reserved `CHUG_` prefix;
  it is §5.3 ([correction 1](#corrections-verified-against-the-tree)).
- **[#308](./308-gha-port.md) §D and §H.2** — "both options dissolve if
  host-native nodes land" and "dissolves the image-build question" — are
  retracted by [Decision 0](#decision-0-the-308-vs-309-contradiction-resolved).
  #308's ordering row for phase 6 ("2 or 4") becomes "4, plus a registry".

## Verification of the provider claims

The brief asks that the inline-JWK capability be verified rather than assumed.
It was, on 2026-07-29, against Google's current documentation:

- [`gcloud iam workload-identity-pools providers
  create-oidc`](https://cloud.google.com/sdk/gcloud/reference/iam/workload-identity-pools/providers/create-oidc)
  — `--jwk-json-path` is "an optional file containing jwk public keys" in RFC
  7517 format; `--issuer-uri` is required; `--allowed-audiences` accepts at most
  10 values of at most 256 characters; `--attribute-condition` is a CEL
  expression of at most 4096 characters; the attribute mapping must include
  `google.subject`.
- [Configure Workload Identity Federation with
  Kubernetes](https://cloud.google.com/iam/docs/workload-identity-federation-with-kubernetes)
  — the canonical uploaded-JWKS flow; "The cluster doesn't need to be accessible
  over the internet"; and the warning that the command does not validate the
  uploaded JWKS, so a malformed or expired set surfaces later as `Error
  connecting to the given credential's issuer`.
- [Configure Workload Identity Federation with other identity
  providers](https://cloud.google.com/iam/docs/workload-identity-federation-with-other-providers)
  — JWKS access by auto-discovery *or* direct upload, capped at 8 locally
  uploaded keys; `exp` must exceed `iat` by at most 24 hours.
- [Configure Workload Identity Federation with deployment
  pipelines](https://cloud.google.com/iam/docs/workload-identity-federation-with-deployment-pipelines)
  — the GitHub Actions reference mapping and the guidance that an attribute
  condition is mandatory against spoofing; the `principal://` and
  `principalSet://` member forms.
- The 127-byte cap on the mapped `google.subject` is documented in Google's
  Workload Identity Federation troubleshooting guidance, which names the error
  and recommends `assertion.sub.extract(...)` as the workaround. This doc's
  `sub` is 34 bytes for the motivating case, so the workaround is not needed —
  but a project or type name long enough to approach the cap would need it, and
  a bound check on the composed `sub` belongs in the pure minting function.

Re-read these before the terraform is written: provider limits move, and the one
this design leans hardest on (uploaded JWKs) is the one whose removal would
reopen [A4](#a4-the-public-reachability-problem-the-crux) entirely.

## What this doc does not decide

- **Which cloud beyond GCP.** The claim set is generic OIDC and the endpoints
  are standard. Only GCP's uploaded-JWKS capability was verified here; whether
  AWS's or Azure's federation can be handed a key set rather than fetching one
  was **not** checked, so do not plan on it either way. [D1](#decisions-taken-2026-08-04)
  is taken on that scope, and a second cloud is the first trigger under
  [What would change D1](#what-would-change-d1) — pick from the three exposure
  options priced there rather than re-arguing them.
- **The `cloud-identities` record's full schema.** Only that it is
  project-scoped, admin-managed, non-secret, existence-checked at release, and —
  since [D4](#decisions-taken-2026-08-04) — populates `service_account`.
- ~~**Which epoch number lands.**~~ **Decided:** `4 → 5`, with
  `WORKLOAD_IDENTITY_SCHEMA_EPOCH = 5` frozen beside the other three, re-derived
  2026-08-04 from `crates/types/src/version.rs`
  ([A5 Skew](#skew-this-field-costs-an-epoch-bump)). What is still not decided is
  whether another epoch-spending change shares the deploy generation, in which
  case one bump covers both and S3 re-reads the constant.
- **The docker API proxy's exact allow-list.** Deny-by-default with `POST
  /build` and push/tag permitted is the shape; the verb-by-verb list is node
  configuration to be reviewed as security-relevant, not prose to be fixed here.
- **Whether beacon's five build workflows port one-for-one.** #308's own advice
  applies — re-read them against the registry that gets chosen, and expect the
  two `:latest`-only pushers to be fixed at port time rather than reproduced.
- **Anything about host-native execution.** [Decision
  0](#decision-0-the-308-vs-309-contradiction-resolved) removes half B's
  dependency on it; it does not amend
  [#309](./309-host-native-execution.md) in any other way.
