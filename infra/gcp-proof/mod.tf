# Chuggernaut's own workload-identity federation, and the fixtures that PROVE it
# is bounded (design #313 half A, slice S6).
#
# This root federates GCP with THIS platform, for the `gcp-proof` job type and
# nothing else. Its attribute condition names `kasofsk/chuggernaut`, so a
# consumer project (beacon, and anything after it) needs a SEPARATE provider
# resource: nothing here can grow into one by accident, which is the reason the
# boundary is proven against the platform itself before a live deploy path
# depends on it.
#
# The asymmetry below is the deliverable. Two service accounts and two buckets
# exist so the ladder's negative rungs have something to be refused BY:
#   * `granted`     is impersonatable by the proof's principalSet, and reads
#                   bucket `granted` only.
#   * `unreachable` has NO workloadIdentityUser binding at all, and is the sole
#                   reader of bucket `denied`.
# So bucket `denied` holds a readable object that this platform's token must not
# be able to read. A negative rung that passes because the object is missing has
# proved nothing, which is why the canary is written to BOTH buckets.
#
# The operator applies this. Nothing in CI and no job ever runs `terraform
# apply` — see infra/README.md for the order, the prerequisites, and the trap
# that an invalid uploaded JWK set surfaces as an issuer error.

terraform {
  required_version = ">= 1.5"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 6.0"
    }
  }

  # The state bucket is a PREREQUISITE the operator creates once, out of band: a
  # root cannot create the bucket holding its own state. `terraform init
  # -backend-config=bucket=...` overrides it. See infra/README.md §1.
  backend "gcs" {
    bucket = "chuggernaut-tfstate"
    prefix = "gcp-proof"
  }
}

# Deliberately no `project` on the provider: every resource names the project
# this root creates, so there is no chicken-and-egg between provider config and
# the project it would point at.
provider "google" {
  region = var.region
}

locals {
  # The composite policy key the issuer emits as the `workload` claim
  # (#313 A1) — `{owner}/{project}:{job_type}:{container}`, computed cloud-side
  # here ONLY to build the member string, never re-derived in CEL.
  workload = "${var.chug_project}:${var.job_type}:${var.container}"

  # THE LOAD-BEARING STRING. The `/` inside the project component must be
  # percent-encoded as `%2F` — an IAM member with a literal `/` is accepted at
  # apply time and simply matches nothing, so a mistake here fails OPEN-LOOKING:
  # no grant, and an error that names the issuer rather than the member.
  principal_set = join("", [
    "principalSet://iam.googleapis.com/projects/",
    google_project.proof.number,
    "/locations/global/workloadIdentityPools/",
    google_iam_workload_identity_pool.chug.workload_identity_pool_id,
    "/attribute.workload/",
    replace(local.workload, "/", "%2F"),
  ])

  # Which service account reads which bucket. The map is the whole fixture: the
  # keys name the buckets, the values name their ONLY reader.
  bucket_readers = {
    granted = google_service_account.granted.email
    denied  = google_service_account.unreachable.email
  }
}

resource "google_project" "proof" {
  name            = var.project_id
  project_id      = var.project_id
  org_id          = var.org_id
  billing_account = var.billing_account

  # A proof project is disposable by construction; teardown is in the runbook.
  deletion_policy = "DELETE"
}

# `sts` and `iamcredentials` are the exchange path itself; `storage` is what
# rungs 4 and 5 read. Left enabled on destroy so a re-apply does not thrash them.
resource "google_project_service" "apis" {
  for_each = toset([
    "iamcredentials.googleapis.com",
    "sts.googleapis.com",
    "storage.googleapis.com",
  ])

  project            = google_project.proof.project_id
  service            = each.value
  disable_on_destroy = false
}

resource "google_iam_workload_identity_pool" "chug" {
  project                   = google_project.proof.project_id
  workload_identity_pool_id = var.pool_id
  display_name              = "Chuggernaut"
  description               = "Workload identities federated from the Chuggernaut platform (design #313)."

  depends_on = [google_project_service.apis]
}

resource "google_iam_workload_identity_pool_provider" "chug" {
  project                            = google_project.proof.project_id
  workload_identity_pool_id          = google_iam_workload_identity_pool.chug.workload_identity_pool_id
  workload_identity_pool_provider_id = var.provider_id
  display_name                       = "Chuggernaut OIDC"
  description                        = "Uploaded JWK set; the issuer is an identifier, not a URL anyone fetches (#313 D1)."

  # A straight copy of the claims the issuer already emits. `attribute.workload`
  # is NOT assembled here from its three components — the issuer computes the
  # composite so there is one definition of the policy key (#313 A1).
  attribute_mapping = {
    "google.subject"     = "assertion.sub"
    "attribute.project"  = "assertion.project"
    "attribute.job_type" = "assertion.job_type"
    "attribute.workload" = "assertion.workload"
  }

  # The TENANCY fence, and the reason this cannot become beacon's provider: a
  # token from any other project is refused before any binding is consulted. The
  # binding below is the independent AUTHORITY fence.
  attribute_condition = "assertion.project == '${var.chug_project}'"

  oidc {
    # Fixed by #313 D1 and equal to `auth::oidc::ISSUER_DEFAULT`. It is the
    # token's `iss`, so changing it invalidates every registered provider at
    # once. Nothing fetches it.
    issuer_uri = var.issuer_uri

    # Uploaded, not discovered — which is what removes the requirement that this
    # platform be reachable from the internet. GCP does NOT validate this at
    # create time; see the trap in infra/README.md §5.
    jwks_json = file(var.jwks_path)
  }
}

resource "google_service_account" "granted" {
  project      = google_project.proof.project_id
  account_id   = "gcp-proof-granted"
  display_name = "gcp-proof: the impersonatable SA"
  description  = "Reachable from the proof's principalSet; reads the `granted` bucket only."
}

# The negative fixture. It is a REAL identity with a REAL grant (it is the only
# reader of the `denied` bucket), and the proof's token must not be able to
# become it — because no workloadIdentityUser binding names it. That is a
# sharper negative than an orphan bucket nobody can read.
resource "google_service_account" "unreachable" {
  project      = google_project.proof.project_id
  account_id   = "gcp-proof-unreachable"
  display_name = "gcp-proof: the DELIBERATELY unreachable SA"
  description  = "No workloadIdentityUser binding. Reads the `denied` bucket; the platform must never reach it."
}

# The authority fence, and the ONLY impersonation grant in this root.
resource "google_service_account_iam_member" "granted_impersonation" {
  service_account_id = google_service_account.granted.name
  role               = "roles/iam.workloadIdentityUser"
  member             = local.principal_set
}

resource "google_storage_bucket" "proof" {
  for_each = local.bucket_readers

  project  = google_project.proof.project_id
  name     = "${var.bucket_prefix}-${each.key}"
  location = var.location

  # IAM is the only path to these objects: with ACLs available, a legacy
  # object-level grant could make a negative rung pass or fail for a reason the
  # terraform does not state.
  uniform_bucket_level_access = true
  force_destroy               = true

  depends_on = [google_project_service.apis]
}

# Written to BOTH buckets on purpose. If the `denied` bucket were empty, rung 5
# would be refused with a NOT FOUND and would prove nothing about the binding —
# the ladder treats that outcome as a failure for exactly this reason.
resource "google_storage_bucket_object" "canary" {
  for_each = google_storage_bucket.proof

  bucket  = each.value.name
  name    = "canary.txt"
  content = "chuggernaut gcp-proof canary (${each.key} bucket)\n"
}

resource "google_storage_bucket_iam_member" "reader" {
  for_each = local.bucket_readers

  bucket = google_storage_bucket.proof[each.key].name
  role   = "roles/storage.objectViewer"
  member = "serviceAccount:${each.value}"
}
