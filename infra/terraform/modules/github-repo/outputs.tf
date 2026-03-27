output "repo_full_name" {
  value = "${var.owner}/${var.repo_name}"
}

output "repo_url" {
  value = "https://github.com/${var.owner}/${var.repo_name}"
}
