output "runner_count" {
  value = var.runner_count
}

output "container_names" {
  value = [for i in range(var.runner_count) : "chuggernaut-runner-${i}"]
}

output "registration_token" {
  value     = data.external.registration_token.result["token"]
  sensitive = true
}
