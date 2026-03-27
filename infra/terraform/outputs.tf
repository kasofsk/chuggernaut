output "git_provider" {
  value = var.git_provider
}

# Forgejo outputs (empty when using GitHub)
output "worker_username" {
  value = local.is_forgejo ? module.worker_user[0].username : var.github_worker_username
}

output "worker_token" {
  value     = local.is_forgejo ? module.worker_user[0].token : var.github_worker_token
  sensitive = true
}

output "reviewer_username" {
  value = local.is_forgejo ? module.reviewer_user[0].username : var.github_reviewer_username
}

output "reviewer_token" {
  value     = local.is_forgejo ? module.reviewer_user[0].token : var.github_reviewer_token
  sensitive = true
}

output "repo_urls" {
  value = local.is_forgejo ? (
    { for k, v in module.forgejo_repos : k => v.repo_url }
  ) : (
    { for k, v in module.github_repos : k => v.repo_url }
  )
}

output "nats_kv_buckets" {
  value = module.nats.kv_buckets
}

output "nats_streams" {
  value = module.nats.streams
}

output "runner_count" {
  value = local.is_forgejo ? (
    length(module.forgejo_runners) > 0 ? module.forgejo_runners[0].runner_count : 0
  ) : (
    length(module.github_runners) > 0 ? sum([for r in module.github_runners : r.runner_count]) : 0
  )
}

output "runner_containers" {
  value = local.is_forgejo ? (
    length(module.forgejo_runners) > 0 ? module.forgejo_runners[0].container_names : []
  ) : (
    flatten([for r in module.github_runners : r.container_names])
  )
}
