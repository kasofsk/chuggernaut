# ---------------------------------------------------------------------------
# Provider selection
# ---------------------------------------------------------------------------

variable "git_provider" {
  description = "Git hosting provider: 'forgejo' or 'github'"
  type        = string
  default     = "forgejo"

  validation {
    condition     = contains(["forgejo", "github"], var.git_provider)
    error_message = "git_provider must be 'forgejo' or 'github'"
  }
}

# ---------------------------------------------------------------------------
# Common variables (used by both providers)
# ---------------------------------------------------------------------------

variable "nats_url" {
  description = "NATS URL for Terraform provisioning (host-accessible)"
  type        = string
  default     = "nats://localhost:4222"
}

variable "nats_worker_url" {
  description = "NATS URL as seen from inside action containers"
  type        = string
  default     = "nats://host.docker.internal:4222"
}

variable "dispatcher_url" {
  description = "Dispatcher HTTP URL as seen from inside action containers"
  type        = string
  default     = ""
}

variable "human_username" {
  description = "Human collaborator username (can approve PRs but cannot merge)"
  type        = string
  default     = "human"
}

variable "managed_repos" {
  description = "List of repos to bootstrap. initial_files is an optional map of file paths to content pushed during repo creation."
  type = list(object({
    org           = string
    repo          = string
    initial_files = optional(map(string), {})
  }))
  default = []
}

variable "runner_count" {
  description = "Number of action runners to register"
  type        = number
  default     = 1
}

variable "runner_labels" {
  description = "Runner labels mapping"
  type        = string
  default     = ""
}

variable "claude_oauth_token" {
  description = "Claude Code OAuth token"
  type        = string
  sensitive   = true
  default     = ""
}

variable "anthropic_api_key" {
  description = "Anthropic API key"
  type        = string
  sensitive   = true
  default     = ""
}

# ---------------------------------------------------------------------------
# Forgejo-specific variables (ignored when git_provider = "github")
# ---------------------------------------------------------------------------

variable "forgejo_url" {
  description = "Forgejo base URL"
  type        = string
  default     = "http://localhost:3000"
}

variable "forgejo_admin_username" {
  description = "Admin username (has merge permission on protected branches)"
  type        = string
  default     = "chuggernaut-admin"
}

variable "forgejo_admin_token" {
  description = "Pre-existing admin API token (created during Forgejo first-run)"
  type        = string
  sensitive   = true
  default     = ""
}

variable "forgejo_actions_url" {
  description = "Forgejo URL reachable from action runners (for uses: directives in workflows)"
  type        = string
  default     = "http://host.docker.internal:3000"
}

variable "worker_password" {
  description = "Password for the chuggernaut-worker Forgejo user"
  type        = string
  sensitive   = true
  default     = "chuggernaut-worker-pass"
}

variable "reviewer_password" {
  description = "Password for the chuggernaut-reviewer Forgejo user"
  type        = string
  sensitive   = true
  default     = "chuggernaut-reviewer-pass"
}

variable "human_password" {
  description = "Password for the human Forgejo user"
  type        = string
  sensitive   = true
  default     = "bananaboi"
}

# ---------------------------------------------------------------------------
# GitHub-specific variables (ignored when git_provider = "forgejo")
# ---------------------------------------------------------------------------

variable "github_token" {
  description = "GitHub PAT or App token with repo, workflow, and admin:org scopes"
  type        = string
  sensitive   = true
  default     = ""
}

variable "github_owner" {
  description = "GitHub organization or user that owns repos"
  type        = string
  default     = ""
}

variable "github_worker_username" {
  description = "GitHub username for the worker bot"
  type        = string
  default     = ""
}

variable "github_reviewer_username" {
  description = "GitHub username for the reviewer bot"
  type        = string
  default     = ""
}

variable "github_worker_token" {
  description = "GitHub PAT for the worker bot (stored as repo action secret)"
  type        = string
  sensitive   = true
  default     = ""
}

variable "github_reviewer_token" {
  description = "GitHub PAT for the reviewer bot (stored as repo action secret)"
  type        = string
  sensitive   = true
  default     = ""
}
