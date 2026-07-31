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
#   5. The seed prune runs in the SAME layer as the `cp -a` that creates it — a
#      prune in a later RUN reclaims nothing, the bytes are already committed.
#   6. Both downloaded binaries (sccache, nats-server) select their tarball by
#      architecture instead of hardcoding x86_64/amd64 — the fleet is mixed and
#      a foreign-arch binary runs under qemu rather than failing (deploy #347).
#
# What this tier CANNOT assert: that the image builds, that the arch cases pick
# URLs that exist, or that the pruned seed is still reused by cargo. Nothing in
# the repo builds a container image (.chug/tasks/ci.sh builds the cargo
# workspace and web/, and agent reviewers run read-only, spec §4.3), so those
# are first exercised on the next deploy's worker-refresh leg.
#
# Run:  deploy/prod/agent-rust-seed.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
DF="$HERE/Dockerfile.agent-rust"
TARGET_DIR="/opt/chug-prebuilt-target"

[ -f "$DF" ] || { echo "FAIL: $DF not found" >&2; exit 1; }

fail() { echo "FAIL: $1" >&2; exit 1; }
# Reduce the Dockerfile to its logical instructions, the way the docker parser
# does: drop comment-only lines FIRST (a comment inside a RUN is removed, not a
# statement terminator), then fold backslash continuations. Without the drop, a
# commented step splits one RUN into several lines and a same-layer assertion
# would read as false — and prose in a comment could satisfy an assertion meant
# for a build step.
folded="$(sed '/^[[:space:]]*#/d' "$DF" | sed ':a;/\\$/{N;s/\\\n//;ba}')"
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
#    (pure image bloat — agent tasks and .chug/tasks/ci.sh run debug test/clippy).
has "cargo build --workspace --all-targets" "seed must warm the workspace build"
has "cargo test --workspace --no-run"       "seed must warm the test binaries"
hasnt "cargo (build|test)[^\n]*--release" \
  "seed must NOT build --release (agents run the debug profile)"
echo "ok: seed warms the debug build + test binaries, no --release"

# 4. The build tree is torn down afterward; the external target survives it.
has "rm -rf /workspace" \
  "seed must delete the /workspace build tree (the external target survives)"
echo "ok: seed removes the /workspace source after building"

# 5. The seed is pruned IN THE SAME LAYER as the `cp -a` that creates it
#    (deploy #347). A `rm`/`find -delete` in a later RUN reclaims nothing: the
#    bytes are already committed to the earlier layer, so the image keeps them
#    and only the prune's own whiteouts are added. Asserted by requiring both
#    prunes to appear on the SAME folded logical instruction as the `cp -a`.
has "cp -a /build-target/\. ${TARGET_DIR}/.*rm -rf ${TARGET_DIR}/debug/incremental" \
  "the incremental/ prune must run in the same RUN as the cp -a (a later layer reclaims nothing)"
has "cp -a /build-target/\. ${TARGET_DIR}/.*find ${TARGET_DIR}/debug .*-delete" \
  "the linked-executable prune must run in the same RUN as the cp -a (a later layer reclaims nothing)"
#    The prune keeps what the seed exists for: dependency .rlib/.rmeta live
#    beside the binaries in debug/deps and are NOT executable, proc-macro
#    dylibs are, so both exclusions are load-bearing.
has "find ${TARGET_DIR}/debug .*-perm /111" \
  "the prune must select linked executables by mode, not by name (an .rlib must survive)"
has "find ${TARGET_DIR}/debug .*-not -path '\*/build/\*'" \
  "the prune must spare build/ (build-script binaries are executable and are reused)"
has "find ${TARGET_DIR}/debug .*-not -name '\*\.so'" \
  "the prune must spare .so (proc-macro dylibs are dependency artifacts)"
echo "ok: seed prunes incremental/ and linked executables in the cp -a's own layer"

# 6. sccache and nats-server are fetched for the BUILD's architecture, not a
#    hardcoded one (deploy #347: both were x86_64 on air's arm64 colima, so
#    every rustc call and the whole tier-2 NATS suite ran under qemu). A
#    foreign-arch binary does not fail the build — it runs, slowly — so nothing
#    but this assertion notices.
has "dpkg --print-architecture.*sccache" \
  "the sccache fetch must select its tarball from dpkg --print-architecture"
has "dpkg --print-architecture.*nats-server" \
  "the nats-server fetch must select its tarball from dpkg --print-architecture"
for _arch in aarch64-unknown-linux-musl x86_64-unknown-linux-musl linux-arm64 linux-amd64; do
  has "$_arch" "the arch switch must offer $_arch"
done
hasnt "sccache-v[^-]*-x86_64-unknown-linux-musl(\.tar|/)" \
  "the sccache URL/path must not hardcode x86_64"
hasnt "nats-server-v[^-]*-linux-amd64(\.tar|/)" \
  "the nats-server URL/path must not hardcode amd64"
has "sccache --version"     "keep the sccache smoke check"
has "nats-server --version" "keep the nats-server smoke check"
echo "ok: sccache + nats-server are arch-selected, with their smoke checks intact"

echo "PASS: Dockerfile.agent-rust warm-target seed contract holds"
