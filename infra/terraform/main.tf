terraform {
  required_version = ">= 1.4"
}

# ---------------------------------------------------------------------------
# NATS: KV buckets + JetStream streams
# ---------------------------------------------------------------------------

module "nats" {
  source   = "./modules/nats"
  nats_url = var.nats_url
}

# ---------------------------------------------------------------------------
# Forgejo users with scoped tokens
# ---------------------------------------------------------------------------

module "worker_user" {
  source      = "./modules/forgejo-user"
  forgejo_url = var.forgejo_url
  admin_token = var.forgejo_admin_token
  username    = "chuggernaut-worker"
  password    = var.worker_password
  email       = "worker@chuggernaut.local"
  token_name  = "chuggernaut-worker"

  # Minimum permissions for work actions:
  # - clone repo, push branches, create/find PRs
  # - NO merge (that's the reviewer's job)
  token_scopes = [
    "read:repository",
    "write:repository",
    "read:issue",
    "write:issue",
  ]
}

module "reviewer_user" {
  source      = "./modules/forgejo-user"
  forgejo_url = var.forgejo_url
  admin_token = var.forgejo_admin_token
  username    = "chuggernaut-reviewer"
  password    = var.reviewer_password
  email       = "reviewer@chuggernaut.local"
  token_name  = "chuggernaut-reviewer"

  # Minimum permissions for review actions:
  # - clone repo, read PRs, submit reviews, merge (bot reviewer)
  # - Separate user ensures reviewer cannot approve their own PRs
  token_scopes = [
    "read:repository",
    "write:repository",
    "read:issue",
    "write:issue",
  ]
}

module "human_user" {
  source      = "./modules/forgejo-user"
  forgejo_url = var.forgejo_url
  admin_token = var.forgejo_admin_token
  username    = var.human_username
  password    = var.human_password
  email       = "${var.human_username}@chuggernaut.local"
  token_name  = var.human_username

  # Human collaborator: can approve/reject PRs but cannot merge.
  # Branch protection restricts merge to admin + reviewer bot only.
  token_scopes = [
    "read:repository",
    "write:repository",
    "read:issue",
    "write:issue",
  ]
}

# ---------------------------------------------------------------------------
# Managed repos: org, repo, workflows, secrets, variables
# ---------------------------------------------------------------------------

module "repos" {
  source   = "./modules/forgejo-repo"
  for_each = { for r in var.managed_repos : "${r.org}/${r.repo}" => r }

  # Explicit dependency: all users must exist before repo provisioning
  depends_on = [module.worker_user, module.reviewer_user, module.human_user]

  forgejo_url       = var.forgejo_url
  admin_token       = var.forgejo_admin_token
  org_name          = each.value.org
  repo_name         = each.value.repo
  admin_username    = var.forgejo_admin_username
  worker_username   = module.worker_user.username
  reviewer_username = module.reviewer_user.username
  human_username    = module.human_user.username
  worker_token      = module.worker_user.token
  reviewer_token    = module.reviewer_user.token
  nats_url          = var.nats_worker_url
  dispatcher_url    = var.dispatcher_url
  claude_oauth_token = var.claude_oauth_token
  anthropic_api_key  = var.anthropic_api_key
  work_workflow      = file("${path.root}/../../action/work.yml")
  review_workflow    = file("${path.root}/../../action/review.yml")
  chug_action        = file("${path.root}/../../action/chug/action.yml")
}

# ---------------------------------------------------------------------------
# Forgejo Action runners
# ---------------------------------------------------------------------------

module "runners" {
  source               = "./modules/forgejo-runner"
  forgejo_url          = var.forgejo_url
  forgejo_internal_url = "http://host.docker.internal:3000"
  admin_token          = var.forgejo_admin_token
  runner_count         = var.runner_count
  runner_config_path   = abspath("${path.root}/../runner/config.yaml")
}
