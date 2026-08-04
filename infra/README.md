# infra — terraform roots, and the operator runbook for them

Chuggernaut's terraform lives here because config travels with the project repo
(CLAUDE.md). **The operator applies. Nothing in CI and no job ever runs
`terraform apply`** — a gate that can mutate cloud state is a gate that can
mutate cloud state when it is wrong.

| Root | What it owns |
| --- | --- |
| [`gcp-proof/`](./gcp-proof) | Chuggernaut's own GCP project, its workload identity pool and OIDC provider, and the two-service-account/two-bucket fixture that the `gcp-proof` job type climbs (design #313 half A, slice S6) |

There are no beacon resources here and there never will be: beacon's
`infra/gcp-workload-id/` is operator-owned and lives in beacon's own repo. This
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

Half A is unexercised. The first consumer of a workload-identity path should not
be a live project's deploy path, so this proves it against **this platform**,
where a wrong answer costs a report rather than a deploy.

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

### The `%2F` is load-bearing

The IAM member is:

```text
principalSet://iam.googleapis.com/projects/{PROJECT_NUMBER}/locations/global/workloadIdentityPools/{pool}/attribute.workload/kasofsk%2Fchuggernaut:gcp-proof:work
```

A literal `/` in place of `%2F` **applies cleanly and grants nothing**. The
failure is open-looking: there is no error at apply time, and the error at use
time names the *issuer*, not the member. `terraform output principal_set` prints
the exact string that was applied — compare it before suspecting anything else.

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

> **Never create it inside `chug-wif-proof`.** This root creates its project with
> `deletion_policy = "DELETE"` and both proof buckets with `force_destroy = true`
> (§7): everything it owns is disposable on purpose. A state bucket in that
> project would be deleted by the very `terraform destroy` that reads it —
> mid-run the state disappears, and what survives is whatever GCP had not
> deleted yet, with no record left of what it was. Nothing warns you at
> `init` time, because a `backend "gcs"` block names a bucket and a bucket name
> says nothing about the project it sits in.

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
service-account emails, bucket names, the project number and the billing account
id. Set both flags anyway — the habit must not be re-decided per state file, and
the next root to share this bucket may hold more.

A different bucket name is fine — `terraform init -backend-config=bucket=<name>`.

### The project is a documentation fact, not a setting

There is nowhere in the HCL to record it, and that is not an oversight: the GCS
backend addresses a bucket by its **globally unique name** and accepts no
`project` argument. Naming `terraform-backend-456523` here *is* the fix. Don't
go looking for a field in `backend "gcs"` to put it in.

**Org and billing.** The org id (`496204159091`) is defaulted;
`billing_account` is the one variable with no default, because it is the one
value this repo cannot know. Put it in `gcp-proof/terraform.tfvars`, which is
gitignored:

```hcl
billing_account = "XXXXXX-XXXXXX-XXXXXX"
```

`project_id` and `bucket_prefix` are globally unique across GCP; a collision
means overriding them, and nothing else changes.

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
  `CONFIG_SCHEMA_EPOCH` (5).
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
terraform plan                 # read it; this creates a project and a billing attachment
terraform apply
```

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
| 4 | The granted read succeeds | The `%2F` member, or the objectViewer binding |
| 5 | The ungranted read is **refused** | A grant wider than the terraform declares — a real finding |
| 5b | An evaluator declaring no identity gets **nothing** | Injection is not per-container; #313 A5 non-inheritance is broken |

Rung 5b runs in the `no-identity` stage-0 evaluator, not in the work container:
the property is non-inheritance, and only a container that declared no identity
can test it. **A ladder that passes 1–4 and skips 5 has proved that a credential
was wired, not that it is bounded.**

---

## 7. Teardown

```sh
cd infra/gcp-proof && terraform destroy
chuggernaut admin cloud-identity delete --project kasofsk/chuggernaut --name gcp-proof
```

The buckets are `force_destroy = true` and the project is `deletion_policy =
"DELETE"`, so the root is disposable by construction — do not relax either; the
disposability is the point, and §1 is what keeps the disposal from reaching the
state. The state bucket sits in `terraform-backend-456523`, is managed by no
terraform, and survives this untouched; delete it by hand only when you are done
with every root that uses it.
