# Only `billing_account` has no default: everything else is fixed by a decision
# recorded in design #313 or by this platform's own identity, and a default that
# an operator has to override is a default that was wrong.

variable "billing_account" {
  description = "Billing account to attach the proof project to. No default — the one value this root cannot know."
  type        = string
}

variable "org_id" {
  description = "The kasofsk.xyz organization."
  type        = string
  default     = "496204159091"
}

variable "project_id" {
  description = "Project id for the proof project. Globally unique across GCP, so a collision means overriding this."
  type        = string
  default     = "chug-wif-proof"
}

variable "region" {
  description = "Provider region. Nothing here is regional; it is set so the provider is fully configured."
  type        = string
  default     = "us-central1"
}

variable "location" {
  description = "Bucket location for the two proof buckets."
  type        = string
  default     = "US"
}

variable "pool_id" {
  description = "Workload identity pool id. Appears verbatim in the principalSet member and in every job's audience."
  type        = string
  default     = "chuggernaut"
}

variable "provider_id" {
  description = "Workload identity pool PROVIDER id. Beacon's eventual provider is a different resource, not this one."
  type        = string
  default     = "chuggernaut-oidc"
}

variable "issuer_uri" {
  description = "The token's `iss`. Fixed by #313 D1 and equal to auth::oidc::ISSUER_DEFAULT; changing it invalidates every registered provider."
  type        = string
  default     = "https://chug.kasofsk.xyz"

  validation {
    condition     = startswith(var.issuer_uri, "https://") && !endswith(var.issuer_uri, "/")
    error_message = "The issuer must be https:// and must not end in a slash — auth::oidc::resolve_issuer rejects both, so a token would never carry this value."
  }
}

variable "jwks_path" {
  description = "Path to the JWK set fetched from the running platform's /.well-known/jwks.json. Uploaded, never fetched by GCP (#313 A4)."
  type        = string
  default     = "jwks.json"
}

variable "chug_project" {
  description = "The `{owner}/{project}` this provider accepts tokens from. THIS platform, deliberately — not a consumer project."
  type        = string
  default     = "kasofsk/chuggernaut"
}

variable "job_type" {
  description = "The job type half of the `workload` composite claim. Must equal the `name:` in .chug/jobs/gcp-proof.yaml."
  type        = string
  default     = "gcp-proof"
}

variable "container" {
  description = "The container half of the `workload` composite claim. `work` alone is granted; an evaluator's token carries a different value and matches no binding here."
  type        = string
  default     = "work"
}

variable "bucket_prefix" {
  description = "Prefix for the two proof buckets. Bucket names are globally unique, so a collision means overriding this."
  type        = string
  default     = "chug-wif-proof"
}
