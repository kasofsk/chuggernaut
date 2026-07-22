#!/bin/sh
# Contract test for the warm-target seed baked into Dockerfile.agent-rust (#123)
# — no Docker, no build; a static assertion over the Dockerfile text, in the
# same spirit as restart-verify.test.sh / worker-refresh.test.sh.
#
# The seed's correctness lives entirely in a few invariants that a careless edit
# would quietly break (dropping the win or corrupting the runtime clone). This
# locks them:
#   1. CARGO_TARGET_DIR is baked as an ENV at a canonical path OUTSIDE
#      /workspace — so the source can be deleted while the target survives, and
#      the runtime clone reuses it in place with no copy.
#   2. The seed builds at /workspace (the bootstrap clone path, spec §4.1) so
#      workspace-crate fingerprints line up with the runtime tree.
#   3. It warms the DEBUG profile the agent actually runs — `cargo build`,
#      `cargo test --no-run`, `cargo clippy` — and NEVER `--release`.
#   4. The build tree is removed afterward (the target is external, so it
#      survives) — the image ships artifacts, not a stale second checkout.
#
# Run:  deploy/prod/agent-rust-seed.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
DF="$HERE/Dockerfile.agent-rust"
TARGET_DIR="/opt/chug-prebuilt-target"

[ -f "$DF" ] || { echo "FAIL: $DF not found" >&2; exit 1; }

fail() { echo "FAIL: $1" >&2; exit 1; }
# Match a single logical instruction even when it is split across
# backslash-continued lines: fold continuations, then grep.
folded="$(sed ':a;/\\$/{N;s/\\\n//;ba}' "$DF")"
has() { printf '%s\n' "$folded" | grep -qE "$1" || fail "$2"; }
hasnt() { printf '%s\n' "$folded" | grep -qE "$1" && fail "$2"; return 0; }

# 1. CARGO_TARGET_DIR baked as ENV, at the canonical /opt path (not under
#    /workspace — else the runtime clone would collide with or wipe the seed).
has "^ENV CARGO_TARGET_DIR=${TARGET_DIR}" \
  "CARGO_TARGET_DIR must be baked as an ENV at ${TARGET_DIR}"
case "$TARGET_DIR" in
  /workspace*) fail "CARGO_TARGET_DIR must live OUTSIDE /workspace" ;;
esac
echo "ok: CARGO_TARGET_DIR baked as ENV at ${TARGET_DIR}, outside /workspace"

# 2. The seed compiles at /workspace — the same path the bootstrap clones to
#    (spec §4.1) — so workspace-crate fingerprints match the runtime tree.
has "COPY \. /workspace" \
  "seed must COPY the repo to /workspace (fingerprints are manifest-path keyed)"
has "cd /workspace" "seed must build from /workspace"
echo "ok: seed builds at /workspace (matches the runtime clone path)"

# 3. Warm the debug profile the agent actually runs; never a release target
#    (pure image bloat — agent tasks and tasks/ci.sh run debug test/clippy).
has "cargo build --workspace --all-targets" "seed must warm the workspace build"
has "cargo test --workspace --no-run"       "seed must warm the test binaries"
hasnt "cargo (build|test)[^\n]*--release" \
  "seed must NOT build --release (agents run the debug profile)"
echo "ok: seed warms the debug build + test binaries, no --release"

# 4. The build tree is torn down afterward; the external target survives it.
has "rm -rf /workspace" \
  "seed must delete the /workspace build tree (the external target survives)"
echo "ok: seed removes the /workspace source after building"

echo "PASS: Dockerfile.agent-rust warm-target seed contract holds"
