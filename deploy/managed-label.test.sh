#!/bin/sh
# Contract test for the `chug.managed` image label on the three built images —
# no Docker, no build; a static assertion over the Dockerfile text, in the same
# spirit as prod/agent-rust-seed.test.sh / prod/worker-refresh.test.sh.
#
# A host `docker system prune --all` on a daily timer removes every image not
# backing a RUNNING container. The agent images back nothing between jobs by
# design, so an unfiltered sweep deletes them and the next job on that node dies
# with `404: No such image` (observed on gumbo-nuc-0, job #258). The host-side
# fix filters on this label; it is inert unless every image carries it. This
# locks the invariants a careless edit would quietly break:
#   1. All three images carry `chug.managed` — one missing image is one dead
#      node.
#   2. No Dockerfile carries MANAGED_LABEL (crates/container/src/docker.rs), the
#      dispatcher's *container*-ownership marker, and the two keys are distinct.
#      Containers inherit their image's labels, so an image carrying that marker
#      makes every container from it — `chug-worker` above all — look like a job
#      container the startup sweep may reap: #266 did exactly that and a
#      dispatcher restart killed the whole worker fleet (#268). The key is read
#      from the Rust source, so a rename there is checked here rather than
#      drifting.
#   3. The LABEL sits in the FINAL build stage — one placed in an earlier stage
#      of Dockerfile.worker would be silently absent from the shipped image.
#   4. In Dockerfile.agent-rust it stays BELOW the warm-target seed: a LABEL is
#      its own layer, and above the seed every future build would pay the 10+
#      minute workspace bake (#123).
#
# Run:  deploy/managed-label.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
DOCKER_RS="$HERE/../crates/container/src/docker.rs"
DOCKERFILES="$HERE/prod/Dockerfile.worker $HERE/dev/Dockerfile.agent $HERE/prod/Dockerfile.agent-rust"
# The image-ownership key. Lives in the `chug.*` image namespace next to
# `chug.git.sha`; its other half is the host prune filter, outside this repo.
KEY="chug.managed"
KEY_RE="$(printf '%s' "$KEY" | sed 's/\./\\./g')"

fail() { echo "FAIL: $1" >&2; exit 1; }
# Last line number matching a pattern, empty when absent.
line_of() { grep -nE "$2" "$1" | tail -1 | cut -d: -f1; }

# 2. The container marker is a different key, and no image carries it.
[ -f "$DOCKER_RS" ] || fail "$DOCKER_RS not found (MANAGED_LABEL source)"
CONTAINER_KEY="$(sed -n 's/^const MANAGED_LABEL: &str = "\(.*\)";$/\1/p' "$DOCKER_RS")"
[ -n "$CONTAINER_KEY" ] || fail "could not read MANAGED_LABEL from $DOCKER_RS"
[ "$CONTAINER_KEY" != "$KEY" ] \
  || fail "MANAGED_LABEL is $CONTAINER_KEY — the container marker must not be the image key (#268)"
echo "ok: image key $KEY is distinct from MANAGED_LABEL $CONTAINER_KEY"

CONTAINER_KEY_RE="$(printf '%s' "$CONTAINER_KEY" | sed 's/\./\\./g')"
for df in $DOCKERFILES; do
  name="$(basename "$df")"
  [ -f "$df" ] || fail "$df not found"
  ! grep -qE "^LABEL ${CONTAINER_KEY_RE}=" "$df" \
    || fail "$name must not carry ${CONTAINER_KEY} — containers inherit image labels and the dispatcher reaps that marker (#268)"
done
echo "ok: no image carries the container marker ${CONTAINER_KEY}"

# 1 + 3. Every image carries the image key, in the final stage.
for df in $DOCKERFILES; do
  name="$(basename "$df")"
  label_line="$(line_of "$df" "^LABEL ${KEY_RE}=\"true\"$")"
  [ -n "$label_line" ] || fail "$name must carry LABEL ${KEY}=\"true\""
  from_line="$(line_of "$df" "^FROM ")"
  [ "$label_line" -gt "$from_line" ] \
    || fail "$name: LABEL must sit in the final stage (below the last FROM)"
  echo "ok: $name carries ${KEY}=\"true\" in its final stage"
done

# 4. agent-rust: below the warm-target seed, so an edit here never busts it.
DF_RUST="$HERE/prod/Dockerfile.agent-rust"
seed_line="$(line_of "$DF_RUST" "cargo build --workspace --all-targets")"
[ -n "$seed_line" ] || fail "Dockerfile.agent-rust: warm-target seed build not found"
rust_label_line="$(line_of "$DF_RUST" "^LABEL ${KEY_RE}=\"true\"$")"
[ "$rust_label_line" -gt "$seed_line" ] \
  || fail "Dockerfile.agent-rust: LABEL must stay BELOW the warm-target seed"
echo "ok: Dockerfile.agent-rust LABEL sits below the warm-target seed"

echo "PASS: ${KEY} image-label contract holds"
