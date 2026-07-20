#!/bin/sh
# Chuggernaut backup: everything needed to rebuild the platform on a new
# machine, in one tarball. Three stores, snapshotted coherently enough:
#
#   1. bare git repos      → one verified `git bundle --all` per project
#   2. NATS JetStream      → `nats account backup` (consistent, via the JS
#                            API — jobs, tasks, users, secrets, events,
#                            artifacts)
#   3. data/keys           → the crown jewels: age/JWT/SSH/NATS keys.
#                            Without these the encrypted state is unreadable.
#
# The tarball lands on the SAME machine — ship it offsite yourself (rclone,
# scp, S3, ...). Restore instructions are written into the backup as
# RESTORE.md.
#
# Usage (defaults fit deploy/dev):
#   deploy/backup.sh
#   REPOS_ROOT=... KEYS_DIR=... DEST=... NATS_NETWORK=... deploy/backup.sh
set -eu

cd "$(dirname "$0")/.."
REPOS_ROOT=${REPOS_ROOT:-$PWD/deploy/dev/data/repos}
KEYS_DIR=${KEYS_DIR:-$PWD/deploy/dev/data/keys}
DEST=${DEST:-$PWD/deploy/dev/data/backups}
NATS_NETWORK=${NATS_NETWORK:-dev_default}
NATS_URL=${NATS_URL:-nats://nats:4222}   # as seen from inside NATS_NETWORK

STAMP=$(date +%Y%m%d-%H%M%S)
OUT="$DEST/$STAMP"
mkdir -p "$OUT/repos" "$OUT/jetstream"

echo "== git repos → verified bundles"
find "$REPOS_ROOT" -maxdepth 2 -name '*.git' -type d | while read -r repo; do
  rel=${repo#"$REPOS_ROOT"/}
  bundle="$OUT/repos/$(echo "$rel" | sed 's|/|__|g' | sed 's|\.git$||').bundle"
  git -C "$repo" bundle create "$bundle" --all 2>/dev/null
  git bundle verify "$bundle" >/dev/null
  echo "   $rel -> $(basename "$bundle")"
done

echo "== NATS JetStream (nats account backup via nats-box)"
docker run --rm --network "$NATS_NETWORK" \
  -v "$KEYS_DIR":/keys:ro -v "$OUT/jetstream":/backup \
  natsio/nats-box:latest \
  nats account backup -s "$NATS_URL" --creds /keys/dispatcher.creds /backup --force

echo "== keys"
cp -Rp "$KEYS_DIR" "$OUT/keys"

cat > "$OUT/RESTORE.md" << 'EOF'
# Restoring a Chuggernaut backup

On the new machine, from a chuggernaut checkout:

1. **Keys** — copy `keys/` to your KEYS_DIR (e.g. `deploy/dev/data/keys`),
   `chmod 600` the private files. This must happen before anything else:
   NATS boots from `nats-resolver.conf`, and every secret decrypts with
   these identities.

2. **NATS** — boot a fresh JetStream server with the restored resolver conf
   (`docker compose -f deploy/dev/compose.yaml up -d nats`), then:
   `docker run --rm --network dev_default -v <this dir>/jetstream:/backup \
      -v <KEYS_DIR>:/keys:ro natsio/nats-box:latest \
      nats account restore -s nats://nats:4222 --creds /keys/dispatcher.creds /backup`

3. **Repos** — each bundle is a full clone source:
   `git clone --mirror repos/acme__demo.bundle <REPOS_ROOT>/acme/demo.git`
   Then per repo, restore the platform config the bundle can't carry:
   `git -C <repo> config uploadpack.allowFilter true` and reinstall the
   pre-receive hook (`chuggernaut admin project create` does this for new
   repos; for restored ones copy hooks/pre-receive from any existing repo or
   re-run the hook install).

4. Start the dispatcher + api as usual. Restart-reconciliation (§3.6)
   rebuilds in-memory state from the restored KV; jobs that were mid-flight
   at backup time recover or escalate per the §3.6 rules.
EOF

tar -czf "$DEST/chug-backup-$STAMP.tgz" -C "$OUT/.." "$STAMP"
SIZE=$(du -h "$DEST/chug-backup-$STAMP.tgz" | cut -f1)
echo "== done: $DEST/chug-backup-$STAMP.tgz ($SIZE)"
echo "   ship it offsite — a backup on the same disk is only half a backup."
