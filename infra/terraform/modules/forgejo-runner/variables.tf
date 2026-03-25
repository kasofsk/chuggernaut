variable "forgejo_url" {
  description = "Forgejo base URL"
  type        = string
}

variable "admin_token" {
  description = "Admin API token (for obtaining runner registration tokens)"
  type        = string
  sensitive   = true
}

variable "runner_count" {
  description = "Number of runners to register"
  type        = number
  default     = 1
}

variable "runner_env_tag" {
  description = "Tag for the chuggernaut-runner-env image (e.g. latest, rust)"
  type        = string
  default     = "rust"
}

variable "runner_labels" {
  description = "Runner labels (maps label to container image). Uses runner_env_tag by default."
  type        = string
  default     = ""
}

variable "runner_config_path" {
  description = "Absolute path to runner config.yaml on the host"
  type        = string
}

variable "forgejo_internal_url" {
  description = "Forgejo URL as seen from inside runner containers (e.g. http://host.docker.internal:3000)"
  type        = string
  default     = ""
}

variable "runner_image" {
  description = "Forgejo runner Docker image"
  type        = string
  default     = "data.forgejo.org/forgejo/runner:11"
}
