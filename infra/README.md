# infra — terraform roots, and the operator runbook for them

Chuggernaut's terraform lives here because config travels with the project repo
(CLAUDE.md). **The operator applies. Nothing in CI and no job ever runs
`terraform apply`** — a gate that can mutate cloud state is a gate that can
mutate cloud state when it is wrong.

| Root | What it owns |
| --- | --- |
| [`gcp-proof/`](./gcp-proof) | A workload identity pool and OIDC provider **inside the existing, shared `daekon-ai` project**, and the two-service-account/two-bucket fixture that the `gcp-proof` job type climbs (design #313 half A, slice S6) |

There are no beacon resources here and there never will be: beacon's
`kasofsk/beacon:infra/gcp-workload-id/` is operator-owned and lives in beacon's
own repo — the `{repo}:{path}` form is deliberate, because a bare path implies
this tree. This
root's attribute condition names `kasofsk/chuggernaut`, so beacon's eventual
provider is a **separate resource** that this one cannot grow into by accident.

---

## The mirror is public: the `.gitignore` here is a disclosure boundary

**Everything that reaches `main` in this repo is on the public internet within
about five minutes.** The GitHub mirror `kasofsk/chuggernaut` is **public** —
re-verified 2026-08-04, and re-runnable:

```sh
gh repo view kasofsk/chuggernaut --json visibility,isPrivate
# {"isPrivate":false,"visibility":"PUBLIC"}

# or, with no `gh` and no credential of any kind — which is itself the answer:
git ls-remote https://github.com/kasofsk/chuggernaut | head -1
```

The launchd agent installed by
[`deploy/prod/chug-mirror-install.sh`](../deploy/prod/chug-mirror-install.sh)
runs `git push mirror main:main --force-with-lease` every `INTERVAL` seconds,
default **300** ([`deploy/prod/README.md`](../deploy/prod/README.md) §3). It
pushes **only `main`**, so a job branch is not itself mirrored — but a job whose
evaluation passes merges to `main` with no human in the loop, so the gap between
an agent's `git commit` and publication is that merge plus one tick of the
timer. Nothing reviews the diff for *disclosure* in between. The tree elsewhere
calls the mirror **read-only** and warns that direct pushes are overwritten;
that is a correctness warning about losing your work, and it is not this one.

**So the ignore rules covering `infra/` are not tidiness — they are the only
thing standing between a value and publication.** There is no secret scan in
[`.chug/tasks/ci.sh`](../.chug/tasks/ci.sh) or
[`.githooks/pre-commit`](../.githooks/pre-commit): the gates are fmt, clippy,
tests, the module registry, duplication, the comment lint and doc lint, and not
one of them reads a file looking for a credential.

(Whether the mirror *should* be public is the operator's call and is not decided
here. This section records what is true today.)

### What [`.gitignore`](../.gitignore) excludes, and why each one is there

| Pattern | Why |
| --- | --- |
| `infra/**/terraform.tfvars`, `infra/**/terraform.tfvars.json`, `infra/**/*.auto.tfvars`, `infra/**/*.auto.tfvars.json` | Variable files — where `billing_account` and every other value this repo cannot know is written (§1). Four spellings because those are the four names terraform loads automatically |
| `infra/**/.tokens/` | An earlier orphaned experiment kept **live plaintext secrets** under this name; it stays ignored so it cannot come back by habit |
| `infra/**/*.tfstate`, `infra/**/*.tfstate.*` | State holds every value in plaintext, sensitive or not. This root's state is remote (§1), but a `terraform init` run before the backend is configured writes a local one |
| `infra/**/tfplan` | A saved plan carries the same values as state, plus the pending diff |
| `infra/**/.terraform/` | Provider plugins and the resolved backend configuration |
| `infra/**/.terraform.lock.hcl` | **Not a secret.** Ignored because a lock file generated on someone else's platform would fail your `init` (§1) |
| `infra/**/jwks.json` | **Not a secret either** — a JWK set is public data ([#313 A4](../docs/design/313-workload-identity-image-builds.md)). Ignored because it is fetched from the running platform, never authored (§2 step (c)) |

The last two rows matter as much as the first five: treating every ignored file
as a secret is how the list stops being read, and the list is the boundary.

### A new root adds its own exclusions before its first `git add`

Every pattern above names a **filename**, and a second root inherits protection
only for the names it happens to reuse. A root applied with
`-var-file=prod.tfvars`, or one that fetches a credential to some name other
than `jwks.json`, matches nothing here and is tracked the moment someone types
`git add infra/`.

So the exclusion goes in **before** the file exists, and the check is one
command:

```sh
git check-ignore -v infra/<root>/<file>   # silent + exit 1 means it is NOT ignored
```

There is no gate that would catch it afterwards, and there is no clean undo: a
later commit deleting the file leaves it in history, and even rewriting `main`
and force-pushing leaves the blob fetchable from GitHub by its SHA. **A value
that has been pushed is rotated, not reverted.**

---

## `gcp-proof` — proving the boundary against ourselves first

Half A was unexercised when this root was written, and this root is what
exercised it — job #430 climbed the whole ladder and reported `VERDICT PASS`
([the retraction below](#the--is-literal--and-this-is-a-retraction)). The first
consumer of a workload-identity path should not be a live project's deploy path,
so this proves it against **this platform**, where a wrong answer costs a report
rather than a deploy.

What the terraform declares, and why the shape is asymmetric:

- One pool, one provider, `issuer_uri = https://chug.kasofsk.xyz` over an
  **uploaded** JWK set (#313 D1 — nothing fetches the issuer, so no inbound path
  to `gumbo-mini-0` is opened).
- **`gcp-proof-granted`** — impersonatable by the proof's principalSet, and the
  only reader of the **`granted`** bucket.
- **`gcp-proof-unreachable`** — **no** impersonation binding at all, and the only
  reader of the **`denied`** bucket.

Both buckets hold a `canary.txt`. That is deliberate: if the `denied` bucket were
empty, the negative rung would be refused with a *not found* and would have
proved nothing about the binding. The ladder treats a not-found refusal as a
failure for exactly that reason.

### The project is shared, and this root no longer owns it

This root originally created its own `chug-wif-proof` project with
`deletion_policy = "DELETE"`. It cannot: the billing account
`01C6FD-E6129C-3CE232` is at its Cloud billing project quota, so attaching a new
project fails at the first resource with `Error 400: Precondition check failed`.
The operator's decision (2026-08-04) was to use the **existing `daekon-ai`**
project rather than raise the quota or detach something. `mod.tf` therefore reads
the project with `data "google_project" "proof"` — the id is validated at plan
time and `.number` is still available for the principalSet member, which needs
the project **number**, never the id.

**This is a real loss against #313's design intent.** The point of a throwaway
project was that proving the boundary could not damage anything: teardown was one
command with no collateral, and a mistake was bounded by the project itself.
`daekon-ai` is **not disposable** and has other tenants — beacon's `gcp-org` root
creates it and the `claude-readonly` service account in it, and neither is managed
here. What follows from that:

- **`terraform destroy` removes only what this root manages** — the pool, the
  provider, the two service accounts, the two buckets and their objects, and the
  IAM bindings this root created. Everything else in `daekon-ai` survives, and
  must.
- **The API enablements are deliberately left behind.**
  `google_project_service.apis` sets `disable_on_destroy = false`. That was
  hygiene when the project was ours; it is now load-bearing. A destroy that
  disabled `storage.googleapis.com` project-wide would break bystanders that never
  asked this root for anything.
- **A future root in this repo must not assume it can create or delete
  project-scoped things freely in `daekon-ai`.** Name resources so a collision with
  another tenant is impossible, prefer additive IAM members over policy
  replacement (`google_project_iam_member`, never `google_project_iam_policy`),
  and never take a `deletion_policy`/`force_destroy` on anything shared.

`terraform.tfvars` is not needed at all now — there is no value this repo cannot
know. `project_id` and `bucket_prefix` are still overridable; bucket names are
globally unique, so a collision means overriding `bucket_prefix` and nothing else
changes.

### The `/` is literal — and this is a retraction

The IAM member is:

```text
principalSet://iam.googleapis.com/projects/{PROJECT_NUMBER}/locations/global/workloadIdentityPools/{pool}/attribute.workload/kasofsk/chuggernaut:gcp-proof:work
```

The `/` inside the project component is **literal**. It is the *member* that is
load-bearing, not any particular encoding of it: a member that matches nothing
**applies cleanly and grants nothing**, and the failure is open-looking — no
error at apply time, and an error at use time that names the *issuer* rather than
the member. `terraform output principal_set` prints the exact string that was
applied; compare it before suspecting anything else.

**What this section used to say was wrong.** Until job #428 it asserted the
opposite — that the `/` "must be percent-encoded as `%2F`" and that a literal `/`
grants nothing — and `mod.tf` encoded it accordingly. That was written as
established fact without checking the working binding in the operator's other
repo (the [#415](../docs/design/415-knowledge-architecture.md) M1 class: a
present-tense claim about behaviour, asserted rather than checked). The failure
mode it described is real; the direction was backwards.

The evidence for the literal form:

- Job #427 reached rung 3b with the `%2F` member and was refused with
  `403 Permission 'iam.serviceAccounts.getAccessToken' denied`. **Rung 3 passed
  in the same run**, so the JWK set, the issuer, the audience and the attribute
  condition are all correct — only the binding fails to match.
- The operator's **working** GitHub-Actions binding, in production, is
  `~/beacon/infra/gcp-workload-id/mod.tf:34` and reads
  `.../attribute.repository/kasofsk/beacon` — a literal `/`. That repo is not in
  this workspace; this is the operator's 2026-08-04 inspection relied on
  *(secondhand)*, on the same terms as #361 and #362.
- Google's own Workload Identity Federation guidance for GitHub Actions uses the
  same literal form (`attribute.repository/octo-org/octo-repo`).

**This is now settled by observation, not just argument.** Job #430 climbed every
rung against the applied literal member and reported `VERDICT PASS` — rung 3b
included, and 3b *is* the binding.

### #429 did not disprove it — that reading was a mis-attribution

Between the apply and #430 there was **job #429**, which was refused at rung 3b
with the literal member already in place. It was read at the time as the literal
form failing too. It was not: a freshly written workload-identity binding takes
minutes to take effect, and #429 ran inside that window. **#430 changed no
terraform** and passed.

So a 3b refusal and a wrong member look identical from inside the ladder, and the
first suspect for a *fresh* binding is time, not encoding. **Do not re-run the
`%2F` experiment on the strength of #429.** The remaining hypothesis, if a 3b
refusal ever outlives propagation, is that
`roles/iam.workloadIdentityUser` does not carry
`iam.serviceAccounts.getAccessToken` and the binding needs
`roles/iam.serviceAccountTokenCreator` instead — one hypothesis at a time, or the
next run tells you nothing.

---

## 1. Prerequisites the operator creates once

### The state bucket — and where it must not live

`gcp-proof/mod.tf` has a `backend "gcs"` block pointing at bucket
`chuggernaut-tfstate`, prefix `gcp-proof`. A root cannot create the bucket that
holds its own state, so it is created **by hand, once, out of band**, and stays
unmanaged by terraform for good.

It lives in **`terraform-backend-456523`** — a long-lived project that exists to
hold terraform state and nothing else, and that no root in this repo manages.

**The rule, not just the value: state lives outside everything the root writing
it manages.** A second root added here inherits the rule — give it a new
`prefix` in the same bucket, not a bucket of its own.

> **Never create it inside `daekon-ai`.** This root writes both proof buckets with
> `force_destroy = true` (§7), and a state bucket sitting beside them is one
> mistaken `bucket_prefix` or one stray `terraform state rm` away from being the
> thing that gets emptied. Nothing warns you at `init` time, because a
> `backend "gcs"` block names a bucket and a bucket name says nothing about the
> project it sits in. The rule held when this root owned a disposable project and
> holds harder now that it does not: state lives outside everything the root
> writing it manages, and outside the blast radius of that root's mistakes.

That split is the standard bootstrap, not a local quirk. Beacon does the same
with one hand-created `beacon-tfstate`: six roots (`gcp-org`, `gcp-app/prod`,
`gcp-workload-id`, `gcp-firebase`, …) share it and differ only by `prefix`, and
none of them manages it. (Beacon's roots are operator-owned and live in beacon's
own repo, not here.)

### Creating it

```sh
gcloud storage buckets create gs://chuggernaut-tfstate \
  --project=terraform-backend-456523 --location=US \
  --uniform-bucket-level-access --public-access-prevention
gcloud storage buckets update gs://chuggernaut-tfstate --versioning
```

Then bound the version history — this repo keeps the **10 most recent noncurrent
versions**:

```sh
cat > lifecycle.json <<'JSON'
{"rule": [{"action": {"type": "Delete"},
           "condition": {"numNewerVersions": 10, "isLive": false}}]}
JSON
gcloud storage buckets update gs://chuggernaut-tfstate --lifecycle-file=lifecycle.json
```

These are `gcloud storage` spellings (the `gsutil` equivalents differ), written
against Google's documented surface rather than a locally installed SDK — no
container in this repo ships `gcloud`, so `gcloud storage buckets create --help`
on the machine you run it from is the authority if a flag is rejected.

**Why versioning.** The GCS backend takes a lock, but it keeps **no history**.
An apply interrupted or truncated part-way can leave state that does not match
reality, and `terraform state rm`, a wrong `import` and a mistaken `destroy` all
have no undo of their own. Object versioning *is* that undo: the previous
generation of the `default.tfstate` object under the `gcp-proof/` prefix is
still there to restore.

**Why a lifecycle rule, and why by count.** Versions otherwise accumulate
without bound. A count (`numNewerVersions: 10`) bounds the history no matter how
often this root is applied; a 90-day noncurrent expiry — the other reasonable
choice — would instead leave a rarely-applied root with nothing to roll back to
after a quiet quarter. State objects are kilobytes either way, so this is
hygiene, not cost.

**Why uniform access and public-access-prevention.** Terraform state routinely
holds sensitive values in plaintext. This particular state honestly does not
hold much: the uploaded JWK set is public data
([#313 A4](../docs/design/313-workload-identity-image-builds.md)), and the rest is
service-account emails, bucket names and the project number. Set both flags
anyway — the habit must not be re-decided per state file, and the next root to
share this bucket may hold more.

A different bucket name is fine — `terraform init -backend-config=bucket=<name>`.

### The project is a documentation fact, not a setting

There is nowhere in the HCL to record it, and that is not an oversight: the GCS
backend addresses a bucket by its **globally unique name** and accepts no
`project` argument. Naming `terraform-backend-456523` here *is* the fix. Don't
go looking for a field in `backend "gcs"` to put it in.

**No org, no billing, no tfvars.** This root creates no project, so it needs
neither an org id nor a billing account and there is nothing an operator must
supply. Every variable has a working default and `gcp-proof/terraform.tfvars` —
still gitignored — can stay absent. What you do need is **access to the existing
`daekon-ai` project**: the `data "google_project"` read fails at plan time
otherwise, which is the intended way to learn you are pointed at the wrong
credential.

**`.terraform.lock.hcl` is gitignored here** rather than committed. Your first
`terraform init` writes one for your own platform; a lock file generated on
someone else's would fail your `init` until you re-locked it.

---

## 2. The order, and what is meaningful before the end of it

Rungs 3–5 **cannot** pass until all four of these have happened. A red rung
before that point is the sequence being incomplete, not a defect:

| Step | What | Why the ladder needs it |
| --- | --- | --- |
| (a) | **#414 merges** — injection at launch (#313 S4) | Until then no container gets `/chuggernaut/cloud/…` at all, so **rung 1 fails** and nothing above it is reachable |
| (b) | **A `deploy` job puts the dispatcher on epoch 5**, and `update.sh`'s idempotent `init` leg generates `oidc_private.pem` / `oidc_public.pem` | The signing key (#313 S1) and the epoch a `min_dispatcher: 5` config needs to run at all |
| (c) | **`curl $BASE/.well-known/jwks.json > gcp-proof/jwks.json`** | The file `jwks_json` uploads. It is gitignored — it is fetched from the running platform, never authored |
| (d) | **`terraform apply`** | The pool, the provider, the SAs, the buckets and the binding |

**Rungs 1 and 2 are meaningful as soon as (a) and (b) are done** — they read the
injected files and decode the token's claims locally, with no network call at
all, so they prove the platform's half of the boundary before any cloud resource
exists. That is the point of ordering the ladder this way: the rungs that need
nothing from GCP run first, and a run that stops at rung 3 has still told you
something true.

**On `min_dispatcher: 5`.** `.chug/jobs/gcp-proof.yaml` declares it because a
non-empty `workload_identities:` requires it (spec §1.1). Two different gates
read that number, and they fire at different moments:

- **At merge**, the dispatcher refuses to land a branch declaring an epoch above
  the one it runs, escalating with `merge_config_skew` (spec §14.3). It knows
  its own epoch, so this is the check that holds. `.chug/tasks/ci.sh`'s
  config-skew gate is the advisory, earlier signal: it asks the *deployed*
  dispatcher only when `CHUG_API_URL` is set, which it is not inside an
  evaluator container, and otherwise compares against the checkout's own
  `CONFIG_SCHEMA_EPOCH` (6).
- **At release**, the running dispatcher enforces it for real (spec §14.2): a
  `gcp-proof` job released against a pre-epoch-5 dispatcher parks `Stalled` with
  `config_schema_skew` and launches nothing.

So step (b) is not paperwork. Until a dispatcher carrying epoch 5 is deployed,
releasing a `gcp-proof` job parks it.

---

## 3. Apply

```sh
cd infra/gcp-proof
terraform init                 # add -backend-config=bucket=<name> if you renamed it
terraform plan                 # read it; every Create lands INSIDE the shared daekon-ai
terraform apply
```

Read the plan with the shared project in mind: nothing should show a project
being created, and nothing outside this root's own resources should show a
change. A `data.google_project.proof` read that errors means the credential
cannot see `daekon-ai`, not that the id is wrong.

**An unchanged tree plans clean, and that is load-bearing.** Until job #431 every
plan reported `google_iam_workload_identity_pool_provider.chug will be updated
in-place` with one cosmetic diff — `~ jwks_json = jsonencode( # whitespace
changes` — because the file's whitespace and GCP's normalisation of the same JSON
never agreed. `mod.tf` now sends `jsonencode(jsondecode(file(var.jwks_path)))`, so
both sides encode the same bytes. **The alternative,
`ignore_changes = [oidc[0].jwks_json]`, is deliberately not taken: a key rotation
*is* a `jwks_json` change (§5), so ignoring the field would silently break the one
update this root must be able to perform.** It converges on the next `apply`,
which is what writes the normalised bytes GCP then echoes back; a plan that is
still dirty *after* that apply is not the diff this fixed — read what it names,
and suspect a real change before reaching for `ignore_changes`.

`terraform plan` is **not runnable from a job container** — it needs credentials
the platform deliberately does not hold — and the acceptance bar for a change to
this root is `terraform fmt -check` and `terraform validate`, both of which run
offline.

---

## 4. Hand the outputs to the platform

```sh
terraform output cloud_identity_command   # prints the exact admin call
terraform output granted_bucket
terraform output denied_bucket
```

Register the identity (spec §8.3 — plaintext KV, cloud coordinates, not a
secret):

```sh
chuggernaut admin cloud-identity set \
  --project kasofsk/chuggernaut --name gcp-proof \
  --audience  "$(terraform output -raw audience)" \
  --service-account "$(terraform output -raw service_account)"
```

Every name in a `workload_identities:` list must have a record at release
(spec §2.2), so a missing record fails the release with `cloud identity
'gcp-proof' is not set` rather than failing inside the container.

Then release a `gcp-proof` job with the two bucket names as inputs. Both have
defaults in `.chug/jobs/gcp-proof.yaml`; override them if you changed
`bucket_prefix`.

---

## 5. The trap: an invalid JWK set blames the issuer

**GCP does not validate the uploaded JWK set at create time.** Google's own
guidance: *"The command doesn't validate the cluster's JWKS. If the JWKS is
malformed or expired, subsequent authentication attempts might fail with an
error message `Error connecting to the given credential's issuer`."*

That error names the **issuer**, and under #313 D1 the issuer is an identifier
nobody fetches — so there is nothing at the other end to be "connected to", and
the message is actively misleading. **When rung 3 reports
`Error connecting to the given credential's issuer`, suspect the upload in step
(c) before anything else:**

1. Is `gcp-proof/jwks.json` valid RFC 7517 with a non-empty `keys` array?
   (`jq -e '.keys | length > 0' gcp-proof/jwks.json`)
2. Was it fetched *after* the deploy that generated the current
   `oidc_public.pem`? A JWKS from a previous key is well-formed and wrong.
3. Does its `kid` match the `kid` in the header of the token in
   `/chuggernaut/cloud/gcp-proof/token`? Rung 2 prints the decoded payload; the
   header decodes the same way.

Re-uploading is `terraform apply` after replacing the file. Rotation is the same
act — the provider allows up to 8 keys, so publish the new one, wait out the
longest token TTL (≤1h, #313 A3), then retire the old.

A rotation therefore **must** show up as a `jwks_json` diff in the plan. That is
why §3's fix for the permanently-dirty plan normalises the encoding rather than
ignoring the field.

---

## 6. Reading the ladder

The job's deliverable is its **stdout**, and the ladder summary prints last on
every path — including a failure — because a worker keeps only the final 700 KiB
of a task's logs. Each rung reads `PASS`, `FAIL` or `NOT REACHED`, in order, and
the run stops at the first failure.

| Rung | Proves | A failure usually means |
| --- | --- | --- |
| 1 | The credential is injected, at the promised modes | (a) has not merged, or the job type lost its `workload_identities:` |
| 2 | The claims are what was minted — decoded **locally**, no network | The issuer's claim assembly changed; the cloud would refuse this too, and this names the field |
| 3 | The STS accepts the exchange | The JWKS upload (§5), or (c)/(d) not done |
| 3b | The federated token impersonates the SA | The member in the `workloadIdentityUser` binding — one that matches nothing applies cleanly and grants nothing; the `/` is literal, per the retraction above |
| 4 | The granted read succeeds | The objectViewer binding on the granted bucket; 3b already proved the member |
| 5 | The ungranted read is **refused** | A grant wider than the terraform declares — a real finding |
| 5b | An evaluator declaring no identity gets **nothing** — no credential file, no `GOOGLE_APPLICATION_CREDENTIALS`, and no token from **ambient** credentials | Injection is not per-container (#313 A5 non-inheritance is broken), or the container is wearing the node's own identity |

Rungs 3–5 speak **REST over `curl` + `jq`**, not `gcloud`: no job type here
pulls a public image and neither agent image carries the SDK (#313 gap 11), and
a missing `curl` or `jq` is a named `NOT REACHED` rather than a silent pass. The
audience, the endpoints and the service account are read out of the injected
`adc.json`, so this proves the STS accepts our token and that the ADC document
describes the exchange correctly — it does **not** prove #313 A3's claim that an
unmodified Google client reads the same file with no glue, which A3 now records
as still open.

Rung 5 accepts only a **403**. A 404 is inconclusive (an absent object refuses
everyone, which is why the terraform writes a canary into both buckets), and a
request that never got an answer refuses nobody.

Rung 5b runs in the `no-identity` stage-0 evaluator, not in the work container:
the property is non-inheritance, and only a container that declared no identity
can test it. **A ladder that passes 1–4 and skips 5 has proved that a credential
was wired, not that it is bounded.**

Its third check — the **ambient** one — asks the GCE metadata server for the
node's default token directly over HTTP (`metadata.google.internal`, then the
link-local `169.254.169.254` for a container whose resolver does not know the
name), with no `gcloud`, bounded at a 2s connect and a 4s request. **It has two
different passes and its line says which.** Today's workers are on-prem, so it
reads:

```text
gcp-proof: rung 5b … gets nothing: PASS — no metadata server was reachable, so ambient minting was NOT exercised
```

That is a pass of the two file/env assertions and **not** of the ambient one. A
worker on GCE gets `PASS — a metadata server answered at … with HTTP 404 and
minted nothing`, which is the real result; a token in the reply is a **FAIL**,
and it is the finding nothing dispatcher-side would prevent — per-container
scoping bounds what this platform *hands* a container, never what the node it
runs on offers it.

---

## 7. Teardown

```sh
cd infra/gcp-proof && terraform destroy
chuggernaut admin cloud-identity delete --project kasofsk/chuggernaut --name gcp-proof
```

**A destroy is now partial, on purpose.** It removes the pool, the provider, the
two service accounts, the two buckets (`force_destroy = true`, so their canaries
go with them) and the IAM bindings this root created. It does **not** remove the
`daekon-ai` project — nothing here manages it — and it does **not** disable the
three APIs, because `disable_on_destroy = false` and other tenants of that
project use them. Re-enabling by hand afterwards is not something you should have
to do; leaving them is the cheaper mistake.

So teardown no longer returns the account to a clean slate the way a disposable
project did. Check what you expect to be gone actually is:

```sh
gcloud iam service-accounts list --project=daekon-ai
gcloud storage ls --project=daekon-ai
```

The state bucket sits in `terraform-backend-456523`, is managed by no terraform,
and survives this untouched; delete it by hand only when you are done with every
root that uses it.
