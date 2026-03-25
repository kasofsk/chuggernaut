output "repo_full_name" {
  value = "${var.org_name}/${var.repo_name}"
}

output "repo_url" {
  value = "${var.forgejo_url}/${var.org_name}/${var.repo_name}"
}
