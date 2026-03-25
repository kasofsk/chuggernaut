output "username" {
  value = var.username
}

output "token" {
  description = "Scoped API token for this user"
  value       = data.external.token.result["token"]
  sensitive   = true
}
