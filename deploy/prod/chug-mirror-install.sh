#!/bin/sh
# chug-mirror-install — install/enable a per-project GitHub mirror (job #80).
# The platform's bare repo owns `main`; this scripts the previously hand-built
# `com.chuggernaut.mirror` launchd agent into a per-project artifact that
# force-pushes `main` to a read-only GitHub remote every 5 minutes.
#
#   chug-mirror-install.sh --owner O --name N --mirror-url URL [--remote mirror]
#                          [--interval 300] [--dry-run] [uninstall]
#
# Idempotent: re-running updates the remote + reloads the agent. Stores NO
# secrets — it configures the remote URL and prints deploy-key guidance; the
# push credential is an SSH deploy key you install out of band (see below).
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
ENV_FILE="${CHUG_ENV_FILE:-$HERE/chuggernaut.env}"
DRY_RUN=0
REMOTE="mirror"
INTERVAL=300
OWNER=""; NAME=""; MIRROR_URL=""; ACTION="install"

log()  { printf 'chug-mirror: %s\n' "$*"; }
warn() { printf 'chug-mirror: warning: %s\n' "$*" >&2; }
die()  { printf 'chug-mirror: error: %s\n' "$*" >&2; exit 1; }
run()  { printf '  $ %s\n' "$*"; [ "$DRY_RUN" -eq 1 ] && return 0; "$@"; }

while [ $# -gt 0 ]; do
	case "$1" in
	--owner) OWNER="${2:?}"; shift 2 ;;
	--name) NAME="${2:?}"; shift 2 ;;
	--mirror-url) MIRROR_URL="${2:?}"; shift 2 ;;
	--remote) REMOTE="${2:?}"; shift 2 ;;
	--interval) INTERVAL="${2:?}"; shift 2 ;;
	--dry-run) DRY_RUN=1; shift ;;
	uninstall) ACTION="uninstall"; shift ;;
	*) die "unknown arg $1" ;;
	esac
done
[ -n "$OWNER" ] && [ -n "$NAME" ] || die "--owner and --name are required"

[ -f "$ENV_FILE" ] || die "no env file at $ENV_FILE"
# shellcheck disable=SC1090
set -a; . "$ENV_FILE"; set +a
BARE="${REPOS_ROOT:?}/${OWNER}/${NAME}.git"
LABEL="com.chuggernaut.mirror.${OWNER}-${NAME}"
LA="$HOME/Library/LaunchAgents"
PLIST="$LA/$LABEL.plist"

if ! command -v launchctl >/dev/null 2>&1; then
	warn "no launchctl — this is the macOS path. On Linux, install an equivalent"
	warn "systemd timer running: git -C $BARE push $REMOTE main:main --force-with-lease"
	warn "(Linux/systemd is best-effort and untested — see README §6.)"
fi

if [ "$ACTION" = "uninstall" ]; then
	if command -v launchctl >/dev/null 2>&1; then
		run launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
	fi
	run rm -f "$PLIST"
	log "removed mirror agent $LABEL"
	exit 0
fi

[ -n "$MIRROR_URL" ] || die "--mirror-url is required to install"
[ -d "$BARE" ] || die "platform bare repo not found: $BARE (import the project first)"

# 1. point the mirror remote at GitHub (add or update — idempotent).
if git -C "$BARE" remote get-url "$REMOTE" >/dev/null 2>&1; then
	run git -C "$BARE" remote set-url "$REMOTE" "$MIRROR_URL"
else
	run git -C "$BARE" remote add "$REMOTE" "$MIRROR_URL"
fi

# 2. render the per-project launchd agent (generated directly, NOT via
#    install-launchd.sh — its template glob would leave placeholders unfilled).
if command -v launchctl >/dev/null 2>&1; then
	mkdir -p "$LA" "$HOME/Library/Logs/chuggernaut"
	if [ "$DRY_RUN" -eq 0 ]; then
		cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/git</string>
    <string>-C</string><string>$BARE</string>
    <string>push</string><string>$REMOTE</string>
    <string>main:main</string><string>--force-with-lease</string>
  </array>
  <key>StartInterval</key><integer>$INTERVAL</integer>
  <key>WorkingDirectory</key><string>$REPO</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key><string>$HOME</string>
    <key>GIT_SSH_COMMAND</key><string>ssh -i $HOME/.ssh/chug-mirror-$OWNER-$NAME -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new</string>
    <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/chuggernaut/mirror-$OWNER-$NAME.log</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/chuggernaut/mirror-$OWNER-$NAME.log</string>
</dict>
</plist>
PLIST
		plutil -lint "$PLIST" >/dev/null 2>&1 || warn "plutil lint failed on $PLIST"
	else
		printf '  (dry-run) would write %s\n' "$PLIST"
	fi
	run launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
	run launchctl bootstrap "gui/$(id -u)" "$PLIST"
	log "installed mirror agent $LABEL (every ${INTERVAL}s: git push $REMOTE main:main --force-with-lease)"
fi

cat <<GUIDANCE
chug-mirror: deploy-key guidance (do this once, out of band — no secret is stored here):
  1. Generate a dedicated key:   ssh-keygen -t ed25519 -f ~/.ssh/chug-mirror-$OWNER-$NAME -N ''
  2. Add the PUBLIC key ($HOME/.ssh/chug-mirror-$OWNER-$NAME.pub) as a DEPLOY KEY
     WITH WRITE ACCESS on the GitHub repo ($MIRROR_URL).
  3. Use an ssh:// mirror URL (git@github.com:owner/repo.git) so the key applies.
  ⚠ GitHub is a READ-ONLY MIRROR: the platform force-pushes over it. Never push
    to GitHub main directly — land changes as jobs on the platform (README §3).
GUIDANCE
