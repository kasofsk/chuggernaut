output "runner_count" {
  value = var.runner_count
}

output "container_names" {
  value = [for i in range(var.runner_count) : "chuggernaut-gh-runner-${i}"]
}
