#!/bin/sh
# chug-install — streamlined Chuggernaut installation + initial deployment
# (job #80). The machine half of the `/chug-install` Claude Code skill
# (.claude/skills/chug-install/SKILL.md), but every subcommand is runnable by
# hand and idempotent, so the whole flow degrades to a documented runbook if the
# skill is unavailable (deploy/prod/README.md, INSTALL.md).
#
# It COMPOSES the pieces that already exist — it does not reinvent them:
#   - platform bringup:   boot.sh, `chuggernaut init`, install-launchd.sh, deploy-health.sh
#   - project creation:   `chuggernaut admin project create` (platform-owned)
#   - GitHub mirror:      a per-project launchd agent doing `git push --force-with-lease`
#   - worker join:        `admin worker-creds` / `admin worker-git-key` + build-worker.sh
#
# Subcommands:
#   chug-install.sh preflight                 deps + config check, non-destructive
#   chug-install.sh platform                  stand up dispatcher+api+NATS+ssh on this host
#   chug-install.sh project-import <git-url>  import an existing repo as a platform-owned
#                                             project and mirror main back to it
#   chug-install.sh worker-join               provision a worker node's creds + images
#
# Global flags (before the subcommand): --dry-run prints what WOULD run and
# changes nothing. Destructive/outward steps honor it. --force downgrades an
# otherwise-fatal config-validation failure (preflight) to a warning.
#
# Config comes from deploy/prod/chuggernaut.env (same file the services source).
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root
ENV_FILE="${CHUG_ENV_FILE:-$HERE/chuggernaut.env}"
DRY_RUN=0
FORCE=0

log()  { printf '\033[1;36mchug-install:\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mchug-install: warning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mchug-install: error:\033[0m %s\n' "$*" >&2; exit 1; }

# run — echo a command and (unless --dry-run) execute it. Used for every step
# that mutates state or reaches outward, so a dry run is a faithful preview.
run() {
	printf '  \033[2m$ %s\033[0m\n' "$*"
	[ "$DRY_RUN" -eq 1 ] && return 0
	"$@"
}

load_env() {
	[ -f "$ENV_FILE" ] || die "no env file at $ENV_FILE (copy deploy/prod/env.example and fill it in)"
	# shellcheck disable=SC1090  # env path is operator-provided config
	set -a; . "$ENV_FILE"; set +a
}

# ── preflight: deps + config, never mutates ─────────────────────────────────
have() { command -v "$1" >/dev/null 2>&1; }

check_dep() {
	# check_dep <cmd> <why> [optional]
	if have "$1"; then
		log "found $1"
	elif [ "${3:-}" = "optional" ]; then
		warn "$1 not found — $2 (optional)"
	else
		MISSING="$MISSING $1"
		warn "MISSING $1 — $2"
	fi
}

cmd_preflight() {
	log "preflight: checking dependencies"
	MISSING=""
	check_dep git "version control + the platform bare repos"
	check_dep docker "container runtime (colima on macOS provides it)"
	check_dep node "web SPA build (update.sh builds web/)"
	check_dep age "secret encryption at rest (§8.2)"
	check_dep curl "health checks + API calls"
	check_dep launchctl "macOS launchd services" optional
	check_dep systemctl "Linux systemd services" optional
	if [ -n "$MISSING" ]; then
		die "missing required deps:$MISSING — on macOS: brew install colima docker node age"
	fi

	log "preflight: checking config ($ENV_FILE)"
	if [ ! -f "$ENV_FILE" ]; then
		warn "no $ENV_FILE yet — copy deploy/prod/env.example and fill it in"
		return 0
	fi
	load_env
	for var in NATS_URL REPO_URL_BASE AGENT_PROVIDER_DEFAULT REPOS_ROOT KEYS_DIR; do
		eval "val=\${$var:-}"
		# shellcheck disable=SC2154  # val is set by the eval above
		[ -n "$val" ] || warn "env var $var is unset"
	done
	# Validate any repo-authored job-type config offline (same rules as CI),
	# when the binary is built. Best-effort — CI is the authority.
	BIN="$REPO/target/release/chuggernaut"
	if [ -x "$BIN" ] && [ -d "$REPO/jobs" ]; then
		log "preflight: validating jobs/*.yaml (chuggernaut validate)"
		# A validation failure is FATAL: shipping config that fails the same rules
		# CI enforces would deploy a broken job type. `--force` downgrades it to a
		# warning for the operator who knowingly wants to proceed anyway.
		for f in "$REPO"/jobs/*.yaml; do
			[ "$(basename "$f")" = "_defaults.yaml" ] && continue
			[ -f "$f" ] || continue
			if ! "$BIN" validate "$f" >/dev/null 2>&1; then
				if [ "$FORCE" -eq 1 ]; then
					warn "validation FAILED for $f — proceeding anyway (--force); run: $BIN validate $f"
				else
					die "validation FAILED for $f — run: $BIN validate $f (or pass --force to override)"
				fi
			fi
		done
	else
		log "preflight: chuggernaut binary not built — skipping offline config validation (CI covers it)"
	fi
	log "preflight OK"
}

# ── platform: stand up the single-host stack ────────────────────────────────
# A thin, idempotent orchestrator over the README §1 bootstrap. Each step is
# guarded so a re-run skips what already exists; the skill narrates each script
# it is about to run, and any step can be run by hand per the README.
cmd_platform() {
	load_env
	log "platform install (host stack: dispatcher + api + NATS + ssh front)"
	log "this composes the documented bootstrap (README §1); re-running is safe"

	# 1. keys + init (idempotent — chuggernaut init is documented as such).
	if [ -d "${KEYS_DIR:-}" ] && [ -f "${KEYS_DIR}/jwt.pem" ]; then
		log "keys present in $KEYS_DIR — skipping keygen"
	else
		log "generating keys + platform init"
		run "$HERE/boot.sh" || warn "boot.sh reported an issue — see README §1 step 1/3"
	fi

	# 2. launchd (macOS) or systemd (Linux, best-effort/untested).
	if have launchctl; then
		log "installing launchd services (dispatcher, api, boot, backups)"
		run "$HERE/install-launchd.sh"
	elif have systemctl; then
		warn "Linux/systemd path is best-effort and UNTESTED — see README; templates under deploy/prod/systemd (if present)"
	else
		warn "no launchctl or systemctl — install services manually per README §1 step 5"
	fi

	# 3. health gate — reuse the deploy health check (a live dispatcher answers).
	if [ -x "$HERE/deploy-health.sh" ]; then
		log "verifying dispatcher health"
		run "$HERE/deploy-health.sh" || warn "health check failed — inspect ~/Library/Logs/chuggernaut/"
	else
		warn "no deploy-health.sh — verify the dispatcher is up manually (README §3)"
	fi
	log "platform install complete (or previewed) — next: 'chug-install.sh project-import <url>'"
}

# ── project-import: existing repo -> platform-owned, mirror back ─────────────
cmd_project_import() {
	SRC_URL="${1:-}"
	[ -n "$SRC_URL" ] || die "usage: chug-install.sh project-import <git-url> [--owner O --name N]"
	shift || true
	OWNER=""; NAME=""; MIRROR_URL="$SRC_URL"
	while [ $# -gt 0 ]; do
		case "$1" in
		--owner) OWNER="${2:?}"; shift 2 ;;
		--name)  NAME="${2:?}"; shift 2 ;;
		--mirror-url) MIRROR_URL="${2:?}"; shift 2 ;;
		*) die "unknown flag $1" ;;
		esac
	done
	load_env
	# Default owner/name from the source URL basename (acme/api.git → api).
	[ -n "$NAME" ] || NAME="$(basename "$SRC_URL" .git)"
	[ -n "$OWNER" ] || OWNER="$(basename "$(dirname "$SRC_URL")")"
	[ -n "$OWNER" ] && [ -n "$NAME" ] || die "could not derive owner/name — pass --owner/--name"
	BARE="${REPOS_ROOT}/${OWNER}/${NAME}.git"
	log "importing $SRC_URL as platform-owned project $OWNER/$NAME (mirror -> $MIRROR_URL)"

	# 1. create the platform-owned project (bare repo + hook + starter + counter).
	#    Idempotent: skip when the bare repo already exists.
	if [ -d "$BARE" ]; then
		log "bare repo $BARE already exists — skipping project create"
	else
		log "creating platform-owned project (chuggernaut admin project create)"
		run "$REPO/target/release/chuggernaut" admin project create \
			--owner "$OWNER" --name "$NAME" --repos-root "$REPOS_ROOT" \
			|| die "project create failed — is the chuggernaut binary built (cargo build --release)?"
	fi

	# 2. push the existing history into the platform bare repo as main.
	TMP="$(mktemp -d)"
	trap 'rm -rf "$TMP"' EXIT
	log "mirroring source history into the platform bare repo"
	run git clone --bare "$SRC_URL" "$TMP/src.git" || die "clone of $SRC_URL failed"
	# Push everything to the platform bare repo; the platform now OWNS main.
	run git -C "$TMP/src.git" push "$BARE" '+refs/heads/main:refs/heads/main' \
		|| warn "push of main into $BARE failed — the source may use a non-main default branch; re-run with the right ref"

	# 3. wire the GitHub remote as the MIRROR TARGET and install the per-project
	#    mirror agent. Store the push credential as deploy-key guidance only —
	#    never write secrets here.
	log "installing the per-project GitHub mirror (main -> $MIRROR_URL, read-only mirror)"
	run "$HERE/chug-mirror-install.sh" --owner "$OWNER" --name "$NAME" --mirror-url "$MIRROR_URL"

	# 4. verify a round trip is POSSIBLE (the bare repo has main and a mirror
	#    remote). A full commit->GitHub round trip is a live-stack check (see the
	#    skill / verification checklist).
	if [ "$DRY_RUN" -eq 0 ] && [ -d "$BARE" ]; then
		if git -C "$BARE" rev-parse --verify main >/dev/null 2>&1; then
			log "verified: $BARE has a 'main' ref"
		else
			warn "$BARE has no 'main' ref yet — the source push may not have landed"
		fi
	fi
	log "project-import complete (or previewed). Push a commit as a job and watch it appear on the mirror."
}

# ── worker-join: provision a worker node ────────────────────────────────────
cmd_worker_join() {
	NODE=""; PROJECT=""
	while [ $# -gt 0 ]; do
		case "$1" in
		--node) NODE="${2:?}"; shift 2 ;;
		--project) PROJECT="${2:?}"; shift 2 ;;
		*) die "unknown flag $1" ;;
		esac
	done
	load_env
	NODE="${NODE:-${CHUG_WORKER_NODE:-nuc}}"
	BIN="$REPO/target/release/chuggernaut"
	log "worker-join: provisioning node '$NODE'"

	# 1. mint the daemon's scoped NATS creds (subscribe req.worker.$NODE.> only).
	log "minting worker NATS creds (admin worker-creds)"
	run "$BIN" admin --keys-dir "${KEYS_DIR:?}" worker-creds --node "$NODE" \
		|| warn "worker-creds failed — see README §6"

	# 2. mint the node's read-only git credential for self-refresh, when a
	#    platform repo is named.
	if [ -n "$PROJECT" ]; then
		log "minting worker read-only git key (admin worker-git-key, project $PROJECT)"
		run "$BIN" admin --keys-dir "$KEYS_DIR" worker-git-key --node "$NODE" --project "$PROJECT" \
			|| warn "worker-git-key failed — self-refresh will be unavailable on this node"
	else
		warn "no --project given — skipping the self-refresh git key (deploy self-refresh disabled on this node)"
	fi

	# 3. build + start the daemon and agent images on the node (needs WORKER_SSH).
	if [ -n "${WORKER_SSH:-}" ]; then
		log "building + starting the worker daemon on $WORKER_SSH (build-worker.sh)"
		run "$HERE/build-worker.sh"
	else
		warn "WORKER_SSH unset — copy the creds to the node and run build-worker.sh there (README §6)"
	fi

	log "worker-join complete (or previewed)."
	log "Add the node to the dispatcher's DOCKER_NODES: \"$NODE|worker|<slots>\", then restart the dispatcher."
}

# ── dispatch ────────────────────────────────────────────────────────────────
main() {
	while [ $# -gt 0 ]; do
		case "$1" in
		--dry-run) DRY_RUN=1; shift ;;
		--force) FORCE=1; shift ;;
		--env) ENV_FILE="${2:?}"; shift 2 ;;
		-h|--help|help|"") usage; exit 0 ;;
		*) break ;;
		esac
	done
	[ "$DRY_RUN" -eq 1 ] && log "DRY RUN — no changes will be made"
	SUB="${1:-}"; shift || true
	case "$SUB" in
	preflight)      cmd_preflight "$@" ;;
	platform)       cmd_platform "$@" ;;
	project-import) cmd_project_import "$@" ;;
	worker-join)    cmd_worker_join "$@" ;;
	*) usage; die "unknown subcommand '${SUB:-}'" ;;
	esac
}

usage() {
	sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'
}

main "$@"
