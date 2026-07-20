#!/bin/sh
# Offsite backup to Cloudflare R2, building on deploy/backup.sh.
#
#   backup-r2.sh                 make a fresh backup, encrypt, push to hourly/
#   backup-r2.sh promote daily   copy the newest hourly object -> daily/
#   backup-r2.sh promote monthly copy the newest hourly object -> monthly/
#
# Retention is enforced by R2 lifecycle rules per prefix (see deploy/prod/README),
# not here. The tarball IS the crown-jewel keys, so it is age-encrypted to a
# recipient whose PRIVATE key lives offline before it ever leaves the box.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root

set -a
. "$HERE/chuggernaut.env"
set +a

: "${RCLONE_REMOTE:?set RCLONE_REMOTE in chuggernaut.env}"
: "${BACKUP_AGE_RECIPIENT:?set BACKUP_AGE_RECIPIENT in chuggernaut.env}"
case "$BACKUP_AGE_RECIPIENT" in
  age1xxxxxxxx*) echo "backup-r2: BACKUP_AGE_RECIPIENT is still the placeholder" >&2; exit 1 ;;
esac

promote() {
  tier="$1"
  newest="$(rclone lsf "$RCLONE_REMOTE/hourly/" | sort | tail -1)"
  if [ -z "$newest" ]; then echo "backup-r2: no hourly backups to promote"; exit 0; fi
  rclone copyto "$RCLONE_REMOTE/hourly/$newest" "$RCLONE_REMOTE/$tier/$newest"
  echo "backup-r2: promoted $newest -> $tier/"
}

if [ "${1:-}" = "promote" ]; then
  promote "${2:?promote needs a tier: daily|monthly}"
  exit 0
fi

# --- make a fresh backup (all three stores) via the shared script.
REPOS_ROOT="$REPOS_ROOT" KEYS_DIR="$KEYS_DIR" DEST="$BACKUP_DEST" \
  NATS_NETWORK="$NATS_NETWORK" NATS_URL="nats://nats:4222" \
  "$REPO/deploy/backup.sh"

TARBALL="$(ls -t "$BACKUP_DEST"/chug-backup-*.tgz 2>/dev/null | head -1)"
[ -n "$TARBALL" ] || { echo "backup-r2: no tarball produced" >&2; exit 1; }

# backup.sh leaves a plaintext staging dir (contains a copy of the keys!) next
# to the tarball — remove it once the tarball exists.
STAMP="$(basename "$TARBALL" | sed 's/^chug-backup-//; s/\.tgz$//')"
rm -rf "${BACKUP_DEST:?}/$STAMP"

# --- encrypt to the offline recipient, drop the plaintext tarball.
age -r "$BACKUP_AGE_RECIPIENT" -o "$TARBALL.age" "$TARBALL"
rm -f "$TARBALL"

# --- push to R2.
rclone copyto "$TARBALL.age" "$RCLONE_REMOTE/hourly/$(basename "$TARBALL.age")"
echo "backup-r2: pushed $(basename "$TARBALL.age") -> hourly/"

# --- local prune: keep only the newest $BACKUP_LOCAL_KEEP encrypted tarballs.
KEEP="${BACKUP_LOCAL_KEEP:-24}"
ls -t "$BACKUP_DEST"/chug-backup-*.tgz.age 2>/dev/null | tail -n "+$((KEEP + 1))" |
  while IFS= read -r old; do rm -f "$old"; done
