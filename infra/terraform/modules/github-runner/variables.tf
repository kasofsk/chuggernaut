variable "github_token" {
  description = "GitHub token with admin:org scope (for runner registration)"
  type        = string
  sensitive   = true
}

variable "owner" {
  description = "GitHub organization or user that owns the repos"
  type        = string
}

variable "repo_name" {
  description = "Repository to register runners for (org-level runners require different API)"
  type        = string
}

variable "runner_count" {
  description = "Number of self-hosted runners to register"
  type        = number
  default     = 1
}

variable "runner_labels" {
  description = "Comma-separated labels for the runner (e.g. 'self-hosted,chuggernaut,ubuntu-latest')"
  type        = string
  default     = "self-hosted,chuggernaut"
}

variable "runner_image" {
  description = "Docker image for the runner environment"
  type        = string
  default     = "chuggernaut-runner-env:latest"
}

variable "runner_work_dir" {
  description = "Working directory inside the runner container"
  type        = string
  default     = "/tmp/runner"
}
