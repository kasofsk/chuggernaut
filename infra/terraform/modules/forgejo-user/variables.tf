variable "forgejo_url" {
  description = "Forgejo base URL (e.g. http://localhost:3000)"
  type        = string
}

variable "admin_token" {
  description = "Pre-existing admin API token for user creation"
  type        = string
  sensitive   = true
}

variable "username" {
  description = "Username to create"
  type        = string
}

variable "password" {
  description = "User password (used for basic-auth token minting)"
  type        = string
  sensitive   = true
}

variable "email" {
  description = "User email address"
  type        = string
}

variable "token_name" {
  description = "Name for the scoped API token"
  type        = string
  default     = "chuggernaut"
}

variable "token_scopes" {
  description = "Forgejo API token scopes (e.g. read:repository, write:issue)"
  type        = list(string)
}
