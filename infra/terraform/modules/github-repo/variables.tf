variable "github_token" {
  description = "GitHub personal access token or app token with repo, workflow, and admin:org scopes"
  type        = string
  sensitive   = true
}

variable "owner" {
  description = "GitHub organization or user that owns the repo"
  type        = string
}

variable "repo_name" {
  description = "Repository name"
  type        = string
}

variable "worker_username" {
  description = "GitHub username for the worker bot (collaborator with write access)"
  type        = string
}

variable "reviewer_username" {
  description = "GitHub username for the reviewer bot (collaborator with write access + merge)"
  type        = string
}

variable "human_username" {
  description = "GitHub username for the human collaborator"
  type        = string
}

variable "worker_token" {
  description = "Worker's GitHub token (stored as repo action secret)"
  type        = string
  sensitive   = true
}

variable "reviewer_token" {
  description = "Reviewer's GitHub token (stored as repo action secret)"
  type        = string
  sensitive   = true
}

variable "nats_url" {
  description = "NATS URL reachable from inside action runners"
  type        = string
}

variable "dispatcher_url" {
  description = "Dispatcher HTTP URL (set as repo action variable)"
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

variable "work_workflow" {
  description = "Content of work.yml workflow file"
  type        = string
}

variable "review_workflow" {
  description = "Content of review.yml workflow file"
  type        = string
}

variable "initial_files" {
  description = "Map of file paths to content to push into the repo during bootstrap"
  type        = map(string)
  default     = {}
}

variable "required_approvals" {
  description = "Number of required PR approvals before merge"
  type        = number
  default     = 1
}
