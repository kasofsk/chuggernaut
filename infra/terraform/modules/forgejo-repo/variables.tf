variable "forgejo_url" {
  description = "Forgejo base URL"
  type        = string
}

variable "admin_token" {
  description = "Admin API token for org/repo management"
  type        = string
  sensitive   = true
}

variable "org_name" {
  description = "Organization name"
  type        = string
}

variable "repo_name" {
  description = "Repository name"
  type        = string
}

variable "admin_username" {
  description = "Admin user (has merge permission on protected branches)"
  type        = string
}

variable "worker_username" {
  description = "Worker user to add as collaborator"
  type        = string
}

variable "reviewer_username" {
  description = "Reviewer user to add as collaborator"
  type        = string
}

variable "human_username" {
  description = "Human user (can approve PRs but cannot merge)"
  type        = string
}

variable "worker_token" {
  description = "Worker's scoped API token (stored as repo action secret)"
  type        = string
  sensitive   = true
}

variable "reviewer_token" {
  description = "Reviewer's scoped API token (stored as repo action secret)"
  type        = string
  sensitive   = true
}

variable "nats_url" {
  description = "NATS URL reachable from inside action containers"
  type        = string
}

variable "dispatcher_url" {
  description = "Dispatcher HTTP URL (set as repo action variable)"
  type        = string
  default     = ""
}

variable "claude_oauth_token" {
  description = "Claude Code OAuth token (from `claude /setup-token`)"
  type        = string
  sensitive   = true
  default     = ""
}

variable "anthropic_api_key" {
  description = "Anthropic API key (alternative to OAuth token)"
  type        = string
  sensitive   = true
  default     = ""
}

variable "actions_url" {
  description = "Forgejo URL reachable from action runners (for uses: directives)"
  type        = string
}

variable "work_workflow" {
  description = "Content of work.yml workflow file"
  type        = string
}

variable "review_workflow" {
  description = "Content of review.yml workflow file"
  type        = string
}

