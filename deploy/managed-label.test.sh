#!/bin/sh
# Contract test for the `chuggernaut.managed` label on the three built images —
# no Docker, no build; a static assertion over the Dockerfile text, in the same
# spirit as prod/agent-rust-seed.test.sh / prod/worker-refresh.test.sh.
#
# A host `docker system prune --all` on a daily timer removes every image not
# backing a RUNNING container. The agent images back nothing between jobs by
# design, so an unfiltered sweep deletes them and the next job on that node dies
# with `404: No such image` (observed on gumbo-nuc-0, job #258). The host-side
# fix filters on this label; it is inert unless every image carries it. This
# locks the invariants a careless edit would quietly break:
#   1. All three images carry the label — one missing image is one dead node.
#   2. The key is exactly MANAGED_LABEL (crates/container/src/docker.rs), the
#      same marker stamped on launched containers: one key means "Chuggernaut
#      owns this Docker object" whatever kind of object it is. Read from the
#      Rust source, so a rename there fails here instead of drifting.
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

fail() { echo "FAIL: $1" >&2; exit 1; }
# Last line number matching a pattern, empty when absent.
line_of() { grep -nE "$2" "$1" | tail -1 | cut -d: -f1; }

# 2. The key comes from the Rust constant — this test cannot pass with a drifted
#    label because it never hard-codes one.
[ -f "$DOCKER_RS" ] || fail "$DOCKER_RS not found (MANAGED_LABEL source)"
KEY="$(sed -n 's/^const MANAGED_LABEL: &str = "\(.*\)";$/\1/p' "$DOCKER_RS")"
[ -n "$KEY" ] || fail "could not read MANAGED_LABEL from $DOCKER_RS"
KEY_RE="$(printf '%s' "$KEY" | sed 's/\./\\./g')"
echo "ok: MANAGED_LABEL = $KEY (read from crates/container/src/docker.rs)"

# 1 + 3. Every image carries it, in the final stage.
for df in $DOCKERFILES; do
  name="$(basename "$df")"
  [ -f "$df" ] || fail "$df not found"
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
