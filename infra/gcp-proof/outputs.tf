# These outputs are the handoff: they are what the operator types into
# `chuggernaut admin cloud-identity set` and into the job's inputs. See
# infra/README.md §4.

output "project_id" {
  description = "The shared project this root adopted; it is read, never created."
  value       = data.google_project.proof.project_id
}

output "project_number" {
  description = "The project NUMBER — resource names and the principalSet use this, never the id."
  value       = data.google_project.proof.number
}

output "audience" {
  description = "The `audience` field of the cloud-identities record. The provider's full resource name, which is the default `aud` GCP accepts (#313 A3)."
  value       = "//iam.googleapis.com/${google_iam_workload_identity_pool_provider.chug.name}"
}

output "service_account" {
  description = "The `service_account` field of the cloud-identities record — the SA the exchanged token impersonates."
  value       = google_service_account.granted.email
}

output "unreachable_service_account" {
  description = "The SA with no impersonation binding. Never goes in a cloud-identities record; it exists to be out of reach."
  value       = google_service_account.unreachable.email
}

output "granted_bucket" {
  description = "The `granted_bucket` input of a gcp-proof job. Rung 4 reads its canary."
  value       = google_storage_bucket.proof["granted"].name
}

output "denied_bucket" {
  description = "The `denied_bucket` input of a gcp-proof job. Rung 5 must be REFUSED reading its canary, which exists."
  value       = google_storage_bucket.proof["denied"].name
}

output "principal_set" {
  description = "The member string the whole proof turns on. The `/` in the project component is literal; a member that matches nothing applies cleanly and grants nothing."
  value       = local.principal_set
}

output "cloud_identity_command" {
  description = "The admin CLI call that registers this identity with the platform (spec §8.3). Run it before releasing a gcp-proof job."
  value = join(" ", [
    "chuggernaut admin cloud-identity set",
    "--project ${var.chug_project} --name ${var.job_type}",
    "--audience //iam.googleapis.com/${google_iam_workload_identity_pool_provider.chug.name}",
    "--service-account ${google_service_account.granted.email}",
  ])
}
