#!/bin/sh
# Shell test for tasks/ci.sh's MODULES.md registry gate — no NATS, no Docker,
# no cargo required.
#
# The gate lives inside ci.sh as functions rather than as its own script, and
# ci.sh's main body runs on source (it would try to build the workspace). So
# this test slices the gate's block out of ci.sh with awk and sources *that*
# into a subshell whose cwd is a fixture tree. The slice is delimited by the
# same `# --- ` banner comments ci.sh already uses to separate its gates; if
# those banners are renamed, the extraction below fails loudly (case 0) rather
# than silently testing nothing.
#
# What it pins is the refactor-plan C8 rule the flat-glob version could not
# express: a named context directory registers under its own name via its
# `mod.rs`, and its members register under their src-relative paths. Plus the
# C9 rule that grew out of it: when a context graduates to its own crate, the
# gate follows it there — its rows become relative to the new crate's `src/`,
# and a member without a row still fails.
#
# Run:  tasks/modules-registry.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
CI="$HERE/ci.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

GATE="$WORK/gate.sh"
awk '
	/^# --- MODULES\.md registry-completeness gate/ { grab = 1 }
	grab && /^# --- copy-paste detection gate/ { grab = 0 }
	grab { print }
' "$CI" >"$GATE"

pass=0
fail=0

check() { # <name> <expected-rc> <actual-rc> <output-file> [must-contain]
	name="$1"
	want="$2"
	got="$3"
	out="$4"
	needle="${5:-}"
	if [ "$got" = "$want" ] && { [ -z "$needle" ] || grep -qF "$needle" "$out"; }; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc want=$want got=$got${needle:+; expected output to contain: $needle}"
		echo "----- output -----"
		cat "$out"
		echo "------------------"
		fail=$((fail + 1))
	fi
}

run_gate() { # <fixture-root> -> writes rc to $RC, output to $OUT
	OUT="$WORK/out"
	set +e
	(
		cd "$1"
		# shellcheck disable=SC1090
		. "$GATE"
		modules_registry_gate
	) >"$OUT" 2>&1
	RC=$?
	set -e
}

# A fixture root with a dispatcher tree and a MODULES.md carrying whatever rows
# the caller passes on stdin. No domain tree, so the domain half self-skips.
fixture() { # <name> <module-path>... ; rows on stdin
	root="$WORK/$1"
	shift
	rm -rf "$root"
	mkdir -p "$root/crates/dispatcher/src"
	echo '// crate root' >"$root/crates/dispatcher/src/lib.rs"
	for m in "$@"; do
		mkdir -p "$root/crates/dispatcher/src/$(dirname "$m")"
		echo '// module' >"$root/crates/dispatcher/src/$m.rs"
	done
	{
		echo '# MODULES'
		echo
		echo '## `dispatcher` — `crates/dispatcher/src/`'
		echo
		echo '| Module | Contract | Spec |'
		echo '| --- | --- | --- |'
		cat
	} >"$root/MODULES.md"
	printf '%s' "$root"
}

# --- case 0: the slice actually contains the gate --------------------------
OUT="$WORK/out"
grep -q '^modules_registry_gate() {' "$GATE" && RC=0 || RC=1
echo "extracted $(wc -l <"$GATE") lines from ci.sh" >"$OUT"
check "gate block extracted from ci.sh" 0 "$RC" "$OUT"

# --- case 1: flat modules, every one rowed, lib.rs needs no row ------------
root="$(fixture flat core handlers <<'EOF'
| `core` | x | §1 |
| `handlers` | x | §2 |
EOF
)"
run_gate "$root"
check "flat tree in sync passes (lib.rs exempt)" 0 "$RC" "$OUT" "in sync"

# --- case 2: a named context registers as <dir> via mod.rs (C8) ------------
root="$(fixture context core platform_ops/mod platform_ops/cd <<'EOF'
| `core` | x | §1 |

## `platform_ops` — `crates/dispatcher/src/platform_ops/`

| Module | Contract | Spec |
| --- | --- | --- |
| `platform_ops` | the charter | §3 |
| `platform_ops/cd` | x | §4 |
EOF
)"
run_gate "$root"
check "context dir + its own section pass" 0 "$RC" "$OUT" "in sync"

# --- case 3: a module inside a context with no row fails -------------------
root="$(fixture missing_row core platform_ops/mod platform_ops/cd <<'EOF'
| `core` | x | §1 |
| `platform_ops` | the charter | §3 |
EOF
)"
run_gate "$root"
check "context member with no row fails" 1 "$RC" "$OUT" \
	"crates/dispatcher/src/platform_ops/cd.rs has no row"

# --- case 4: a row naming no module fails ----------------------------------
root="$(fixture stale_row core <<'EOF'
| `core` | x | §1 |
| `platform_ops/gone` | x | §3 |
EOF
)"
run_gate "$root"
check "row naming no module fails" 1 "$RC" "$OUT" \
	'lists module `platform_ops/gone`'

# --- case 5: a missing MODULES.md is a broken gate, not a pass -------------
root="$(fixture no_registry core <<'EOF'
| `core` | x | §1 |
EOF
)"
rm "$root/MODULES.md"
run_gate "$root"
check "missing MODULES.md fails loudly" 1 "$RC" "$OUT" "MODULES.md is missing"

# A context crate tree beside the dispatcher one (refactor-plan C9): modules
# under `crates/platform-ops/src`, registered by a section naming that path.
context_crate() { # <fixture-root> <module-name>...
	root="$1"
	shift
	mkdir -p "$root/crates/platform-ops/src"
	echo '// crate root' >"$root/crates/platform-ops/src/lib.rs"
	for m in "$@"; do
		echo '// module' >"$root/crates/platform-ops/src/$m.rs"
	done
}

# --- case 6: a graduated context crate registers under its own src/ --------
root="$(fixture context_crate_ok core <<'EOF'
| `core` | x | §1 |

## `chuggernaut-platform-ops` — `crates/platform-ops/src/`

| Module | Contract | Spec |
| --- | --- | --- |
| `cd` | x | CD plan C |
| `fleet` | x | §3.1 |
EOF
)"
context_crate "$root" cd fleet
run_gate "$root"
check "context crate in sync passes (its lib.rs exempt too)" 0 "$RC" "$OUT" "in sync"

# --- case 7: the gate really walks the context crate, not just past it -----
# Without this the C9 loop could silently skip the new tree and every case
# above would still pass — the drift the gate exists to catch.
root="$(fixture context_crate_drift core <<'EOF'
| `core` | x | §1 |

## `chuggernaut-platform-ops` — `crates/platform-ops/src/`

| Module | Contract | Spec |
| --- | --- | --- |
| `cd` | x | CD plan C |
EOF
)"
context_crate "$root" cd fleet
run_gate "$root"
check "context-crate member with no row fails" 1 "$RC" "$OUT" \
	"crates/platform-ops/src/fleet.rs has no row"

echo
echo "modules-registry.test.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
