output "worker_username" {
  value = module.worker_user.username
}

output "worker_token" {
  value     = module.worker_user.token
  sensitive = true
}

output "reviewer_username" {
  value = module.reviewer_user.username
}

output "reviewer_token" {
  value     = module.reviewer_user.token
  sensitive = true
}

output "repo_urls" {
  value = { for k, v in module.repos : k => v.repo_url }
}

output "nats_kv_buckets" {
  value = module.nats.kv_buckets
}

output "nats_streams" {
  value = module.nats.streams
}

output "runner_count" {
  value = module.runners.runner_count
}

output "runner_containers" {
  value = module.runners.container_names
}
