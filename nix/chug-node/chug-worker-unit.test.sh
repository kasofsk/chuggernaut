#!/bin/sh
# One shape, not two (design #440 slice 7). Two things render the worker
# daemon's systemd unit — `nix/chug-node/` from `chug-worker.service.in`, and
# `deploy/prod/build-worker.sh` from its own heredoc — and this suite is what
# keeps them in step. It is text over text: it proves the template and the
# script agree on the unit's SHAPE and on every DEFAULT, and it proves nothing
# about whether the nix evaluates. **Nothing in this repo's CI evaluates
# `nix/chug-node/`** (#372 §2.3) and slice 7 did not change that; the consuming
# host repo's `nixos-rebuild build` is still the only gate on the nix itself.
#
# Run: sh nix/chug-node/chug-worker-unit.test.sh
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRIPT="$REPO/deploy/prod/build-worker.sh"
REFRESH="$REPO/deploy/prod/worker-refresh.sh"
TEMPLATE="$HERE/chug-worker.service.in"
NIXOS="$HERE/nixos.nix"
DARWIN="$HERE/darwin.nix"
OPTIONS="$HERE/options.nix"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM

fail() { echo "FAIL: $1" >&2; exit 1; }

# ── Case 1: the template IS the unit build-worker.sh renders ──────────────────
# Compared with the script's own variables left unexpanded — `$NODE` against
# `@NODE@` and so on — so this asserts the shape and the case below asserts the
# values. A divergence in either is a node whose reboot brings back a different
# daemon than its last deploy installed.
script_unit() {
	sed -n '/^  UNIT_TEXT="\[Unit\]$/,/^WantedBy=multi-user\.target"$/p' "$SCRIPT" |
		sed -e '1s/^  UNIT_TEXT="//' -e '$s/"$//'
}

template_unit() {
	sed -e '/^#/d' \
		-e 's|@NODE@|$NODE|g' \
		-e 's|@ENV_FILE@|$ENV_FILE|g' \
		-e 's|@PATH@|$NODE_PATH|g' \
		-e 's|@BINARY@|$BIN_DIR/chuggernaut|g' \
		"$TEMPLATE"
}

script_unit > "$WORK/from-script"
template_unit > "$WORK/from-template"
[ -s "$WORK/from-script" ] || fail "no unit could be read out of $SCRIPT — the extraction, not the unit, is what broke"
if ! diff -u "$WORK/from-script" "$WORK/from-template" > "$WORK/unit.diff"; then
	fail "the unit nix/chug-node/ declares is not the unit deploy/prod/build-worker.sh renders:
$(cat "$WORK/unit.diff")"
fi
grep -q '^User=root$' "$TEMPLATE" || fail "the unit must run as root: a root daemon creates SYSTEM systemd scopes for host tasks, which is why it needs no XDG_RUNTIME_DIR (#440 slice 7's provisioning question)"
echo "ok: the template and build-worker.sh render one unit"

# ── Case 2: every placeholder is substituted, and by the right thing ──────────
# A placeholder the module does not name reaches a node verbatim, and
# `ExecStart=@BINARY@ worker` is a unit that fails at load.
for token in $(grep -o '@[A-Z_]*@' "$TEMPLATE" | sort -u); do
	grep -qF "\"$token\"" "$NIXOS" ||
		fail "$token is in the unit template and nix/chug-node/nixos.nix never substitutes it"
done
grep -qF 'config.networking.hostName' "$NIXOS" ||
	fail "@NODE@ must resolve to this host's own name — the fleet node name is run spec and stays in the environment file (#372 §8 R3)"
echo "ok: nixos.nix substitutes every placeholder the template carries"

# ── Case 3: the defaults are the deploy's defaults ─────────────────────────────
# The split D2 answers R3 with only holds while both halves name the same files.
# Read out of the script rather than restated here, so a change there fails this
# rather than drifting quietly.
script_default() { sed -n "s|^ *$1=.*$2:-\\(.*\\)}\"\$|\\1|p" "$SCRIPT" | head -n 1; }
nix_default() { sed -n "/^      $1 = mkOption {/,/^      };/p" "$OPTIONS" | sed -n 's|^        default = "\(.*\)";$|\1|p'; }

SCRIPT_ENV_FILE="$(script_default ENV_FILE WORKER_ENV_FILE)"
SCRIPT_PATH="$(script_default NODE_PATH WORKER_PATH)"
SCRIPT_BIN_DIR="$(sed -n 's|^BIN_DIR=\(.*\)$|\1|p' "$SCRIPT" | head -n 1)"
[ -n "$SCRIPT_ENV_FILE" ] && [ -n "$SCRIPT_PATH" ] && [ -n "$SCRIPT_BIN_DIR" ] ||
	fail "could not read build-worker.sh's own defaults — the extraction, not the defaults, is what broke"

[ "$(nix_default environmentFile)" = "$SCRIPT_ENV_FILE" ] ||
	fail "chug.node.daemon.environmentFile defaults to '$(nix_default environmentFile)' and build-worker.sh writes '$SCRIPT_ENV_FILE' — the unit would read a file nothing renders"
[ "$(nix_default path)" = "$SCRIPT_PATH" ] ||
	fail "chug.node.daemon.path defaults to '$(nix_default path)' and build-worker.sh sets '$SCRIPT_PATH'"
[ "$(nix_default binary)" = "$SCRIPT_BIN_DIR/chuggernaut" ] ||
	fail "chug.node.daemon.binary defaults to '$(nix_default binary)' and the deploy installs '$SCRIPT_BIN_DIR/chuggernaut'"

# And exactly once. A second copy in nixos.nix would not fail the check above —
# it would warn on every correctly-configured node the moment this path moved,
# which is the warning-nobody-can-resolve that darwin.nix's header rejects.
! grep -qF "\"$SCRIPT_ENV_FILE\"" "$NIXOS" ||
	fail "nixos.nix restates the environment-file default ('$SCRIPT_ENV_FILE'); options.nix declares it, so read it from there"
grep -qF 'options.chug.node.daemon.environmentFile.default' "$NIXOS" ||
	fail "the environment-file warning must compare against the declared default rather than a copy of it"
echo "ok: the module's defaults are the deploy's defaults, declared once"

# ── Case 4: one unit name, across all three of them ────────────────────────────
# The name nix writes is the name build-worker.sh installs and the name the swap
# restarts; a mismatch is a self-refresh that restarts nothing, or a second
# daemon on one node name (#440 §1).
grep -qF 'systemd.units."chug-worker.service"' "$NIXOS" || fail "nixos.nix must declare chug-worker.service"
grep -qF 'UNIT_PATH="$UNIT_DIR/chug-worker.service"' "$SCRIPT" ||
	fail "build-worker.sh no longer installs chug-worker.service"
grep -qF 'WORKER_UNIT:-chug-worker.service' "$REFRESH" ||
	fail "worker-refresh.sh's swap no longer restarts chug-worker.service"
echo "ok: nix, the deploy and the swap all name chug-worker.service"

# ── Case 5: adopting the module still changes no node ──────────────────────────
# `enable`-scoped and opt-in: #440 landed with nothing applied to any node, and a
# module that starts a daemon on adoption would convert one by import.
sed -n '/systemd.units."chug-worker.service"/p' "$NIXOS" | grep -qF 'mkIf daemon.enable' ||
	fail "the unit must be gated on chug.node.daemon.enable, which defaults off"
grep -qF 'wantedBy = [ "multi-user.target" ]' "$NIXOS" ||
	fail "NixOS makes its own enablement symlinks and never runs \`systemctl enable\` over /etc/systemd/system, so the unit's own [Install] section would leave it never starting at boot"
grep -qF 'assertion = !cfg.daemon.enable;' "$DARWIN" ||
	fail "darwin has no systemd: chug.node.daemon.enable must be refused there, pointing at deploy/prod/install-worker-launchd.sh"
echo "ok: the unit is opt-in, enabled at boot, and refused on darwin"

echo "PASS: nix/chug-node/'s unit and deploy/prod/build-worker.sh's are one shape (the nix itself is UNEVALUATED here)"
