#!/bin/sh
# Render the launchd plist templates for this checkout and (re)load them as
# user LaunchAgents. Run once at bootstrap and any time the plist templates
# change. Idempotent.
#
#   install-launchd.sh            install / reload all agents
#   install-launchd.sh uninstall  bootout all agents and remove the plists
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root
UID_N="$(id -u)"
DOMAIN="gui/$UID_N"
LA="$HOME/Library/LaunchAgents"

if [ "${1:-}" = "uninstall" ]; then
  for tmpl in "$HERE/launchd/"*.plist.template; do
    label="$(basename "$tmpl" .plist.template)"
    launchctl bootout "$DOMAIN/$label" 2>/dev/null || true
    rm -f "$LA/$label.plist"
    echo "removed $label"
  done
  exit 0
fi

mkdir -p "$LA" "$HOME/Library/Logs/chuggernaut"
# Files created via the deploy tooling aren't marked executable; launchd invokes
# them through /bin/sh, but make them +x anyway for manual runs.
chmod +x "$HERE"/*.sh

for tmpl in "$HERE/launchd/"*.plist.template; do
  label="$(basename "$tmpl" .plist.template)"
  out="$LA/$label.plist"
  sed -e "s|@REPO@|$REPO|g" -e "s|@HOME@|$HOME|g" "$tmpl" > "$out"
  plutil -lint "$out" >/dev/null
  launchctl bootout "$DOMAIN/$label" 2>/dev/null || true
  launchctl bootstrap "$DOMAIN" "$out"
  echo "installed $label"
done

echo "done — logs in ~/Library/Logs/chuggernaut/ ; 'launchctl print $DOMAIN/com.chuggernaut.api' for status"
