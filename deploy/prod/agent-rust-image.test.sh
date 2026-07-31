#!/bin/sh
# Contract test for Dockerfile.agent-rust — no Docker, no build; a static
# assertion over the Dockerfile text, in the same spirit as
# restart-verify.test.sh / worker-refresh.test.sh.
#
# This is the ONLY gate that reads this Dockerfile at all: .chug/tasks/ci.sh
# builds the cargo workspace and web/ and never a container image,
# worker-refresh.test.sh fakes every `docker build`, and agent reviewers run
# read-only (spec §4.3). Everything below is therefore first exercised for real
# on the next deploy's worker-refresh leg.
#
# It replaces agent-rust-seed.test.sh, which locked the warm-target seed #352
# deleted. The contract it now locks:
#   1. NOTHING of this workspace is compiled at image-build time — no seed
#      build, no source copy, no BuildKit cache mounts that existed only to
#      serve it. Compile reuse is the node-local sccache's job alone (spec §3.1).
#      Re-adding a bake is a 2.26GB image regression and ~600s on every refresh
#      leg, for the 45s/task the #352 A/B/C measured.
#   2. CARGO_TARGET_DIR is baked as an ENV at a canonical LITERAL path outside
#      /workspace. Out-of-tree so a ~10GB target/ never lands in the clone the
#      agent commits from; literal and stable because sccache's hash covers the
#      target-derived `-L dependency=` paths, so a per-container path would
#      silently drop the node cache's hit rate to zero (measured: same path
#      100%, different path 0%).
#   3. Both downloaded binaries (sccache, nats-server) select their tarball by
#      architecture instead of hardcoding x86_64/amd64 — the fleet is mixed and
#      a foreign-arch binary runs under qemu rather than failing (deploy #347).
#      With the seed gone, sccache IS the build cache, so this matters more than
#      when it was written, not less.
#   4. The toolchain an agent task needs is present: rustfmt + clippy (the
#      .chug/tasks/ci.sh gate) and the claude CLI (the task itself).
#
# Run:  deploy/prod/agent-rust-image.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
DF="$HERE/Dockerfile.agent-rust"
TARGET_DIR="/opt/chug-cargo-target"

[ -f "$DF" ] || { echo "FAIL: $DF not found" >&2; exit 1; }

fail() { echo "FAIL: $1" >&2; exit 1; }
# Reduce the Dockerfile to its logical instructions, the way the docker parser
# does: drop comment-only lines FIRST (a comment inside a RUN is removed, not a
# statement terminator), then fold backslash continuations. Without the drop,
# prose in a comment could satisfy — or, for the negative cases below, falsely
# trip — an assertion meant for a build step.
folded="$(sed '/^[[:space:]]*#/d' "$DF" | sed ':a;/\\$/{N;s/\\\n//;ba}')"
has() { printf '%s\n' "$folded" | grep -qE "$1" || fail "$2"; }
hasnt() { printf '%s\n' "$folded" | grep -qE "$1" && fail "$2"; return 0; }

# 1. No workspace build. Each clause is one limb of the deleted seed, named
#    separately so a partial resurrection reports which limb came back.
hasnt "COPY \. /workspace" \
  "the image must not copy the workspace in — nothing is compiled at build time (#352)"
hasnt "cargo (build|test|clippy|fetch)" \
  "the image must not compile the workspace — sccache is the build cache (#352)"
hasnt "mount=type=cache" \
  "the BuildKit cache mounts existed only for the deleted seed build (#352)"
hasnt "cp -a /build-target" \
  "no seed copy — there is no seed (#352)"
hasnt "rm -rf /workspace" \
  "nothing to tear down: the image never populates /workspace (#352)"
echo "ok: no workspace build, no source copy, no seed-only cache mounts"

# 2. CARGO_TARGET_DIR: baked ENV, literal, out-of-tree, and created empty so the
#    agent's first cargo run does not race on making it.
has "^ENV CARGO_TARGET_DIR=${TARGET_DIR}\$" \
  "CARGO_TARGET_DIR must be baked as an ENV at the literal path ${TARGET_DIR}"
case "$TARGET_DIR" in
  /workspace*) fail "CARGO_TARGET_DIR must live OUTSIDE /workspace (agents commit from the clone)" ;;
  *'$'*) fail "CARGO_TARGET_DIR must be a literal path — a varying one zeroes the sccache hit rate" ;;
esac
has "mkdir -p ${TARGET_DIR}" "the target dir must exist in the image, empty"
hasnt "prebuilt" \
  "the target path must not claim to be prebuilt — nothing prebuilds it (#352)"
echo "ok: CARGO_TARGET_DIR is a literal out-of-tree ${TARGET_DIR}, created empty"

# 3. sccache and nats-server are fetched for the BUILD's architecture, not a
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

# 4. What the agent and its CI gate actually run inside this image.
has "rustup component add .*rustfmt" "the ci.sh gate runs cargo fmt — rustfmt must be installed"
has "rustup component add .*clippy"  "the ci.sh gate runs cargo clippy — clippy must be installed"
has "npm install -g @anthropic-ai/claude-code" "the agent CLI must be installed"
echo "ok: rustfmt, clippy and the claude CLI are installed"

echo "PASS: Dockerfile.agent-rust image contract holds"
