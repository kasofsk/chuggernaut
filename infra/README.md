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

**The state bucket.** `gcp-proof/mod.tf` has a `backend "gcs"` block pointing at
`chug-tfstate-kasofsk`. A root cannot create the bucket that holds its own
state, so create it out of band, once, in a project you already own:

```sh
gcloud storage buckets create gs://chug-tfstate-kasofsk \
  --project <an-existing-project> --location US --uniform-bucket-level-access
gcloud storage buckets update gs://chug-tfstate-kasofsk --versioning
```

A different name is fine — `terraform init -backend-config=bucket=<name>`.

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

- **At merge**, `.chug/tasks/ci.sh`'s config-skew gate compares it against the
  *deployed* dispatcher — but only when `CHUG_API_URL` is set, which it is not
  inside an evaluator container. There it falls back to the checkout's own
  `CONFIG_SCHEMA_EPOCH` (5), so the file merges.
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
"DELETE"`, so the root is disposable by construction. The state bucket from §1
is **not** managed here and survives; delete it by hand if you are done with it.
