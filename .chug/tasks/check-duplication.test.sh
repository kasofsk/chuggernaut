#!/bin/sh
# Shell test for check-duplication.sh — no NATS, no Docker, no cargo. It needs
# `npx` and the npm registry (or a warm npx cache), exactly as the gate does.
#
# It drives check-duplication.sh in explicit-path mode (paths as positional
# arguments, which replace the whole-repo scan) over fixtures in a temp dir, and
# asserts the three behaviours the gate rests on:
#
#   1. Clean code passes (rc 0).
#   2. A RE-INTRODUCED clone fails (rc 1) and names both copies — the property
#      that makes threshold 0 a gate rather than decoration.
#   3. The `jscpd:ignore-start` / `jscpd:ignore-end` escape hatch works, so a
#      documented, deliberate exception is expressible without raising the bar
#      for everything else.
#
# Plus: a missing `npx` reports the gate as BROKEN (rc 2), never as a pass —
# an unrunnable check must not look like a clean one.
#
# Run:  .chug/tasks/check-duplication.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/check-duplication.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
check() { # <name> <expected-rc> <actual-rc> <output-file> <must-contain>
	name="$1"; want="$2"; got="$3"; out="$4"; needle="$5"
	if [ "$got" = "$want" ] && grep -qF "$needle" "$out"; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc want=$want got=$got; expected output to contain: $needle"
		echo "----- output -----"; cat "$out"; echo "------------------"
		fail=$((fail + 1))
	fi
}

run_sut() { # <arg>... -> writes rc to $RC, output to $OUT
	OUT="$WORK/out"
	set +e
	"$SUT" "$@" >"$OUT" 2>&1
	RC=$?
	set -e
}

# A body long enough to be a clone under .jscpd.json's minLines 10 / minTokens 80.
emit_body() { # <fn-name>
	cat <<-EOF
		fn $1(input: &Config) -> Result<Summary, Error> {
		    let first = input.resolve("alpha")?;
		    let second = input.resolve("beta")?;
		    let third = input.resolve("gamma")?;
		    let fourth = input.resolve("delta")?;
		    let fifth = input.resolve("epsilon")?;
		    let sixth = input.resolve("zeta")?;
		    let total = first + second + third + fourth + fifth + sixth;
		    let label = format!("{first}/{second}/{third}/{fourth}");
		    let note = format!("{fifth}/{sixth}/{total}/{label}");
		    Ok(Summary { total, label, note })
		}
	EOF
}

# 1. Clean: one copy of the body, nothing to pair it with -> pass.
mkdir -p "$WORK/clean"
emit_body only_once > "$WORK/clean/single.rs"
run_sut "$WORK/clean"
check "clean tree passes" 0 "$RC" "$OUT" "no duplicated code"

# 2. A re-introduced clone fails, and the report names both files.
mkdir -p "$WORK/dup"
emit_body first_copy > "$WORK/dup/one.rs"
emit_body second_copy > "$WORK/dup/two.rs"
run_sut "$WORK/dup"
check "re-introduced clone fails" 1 "$RC" "$OUT" "duplicated code found"
check "report names the first copy" 1 "$RC" "$OUT" "one.rs"
check "report names the second copy" 1 "$RC" "$OUT" "two.rs"

# 3. The documented escape hatch: bracketing BOTH copies with jscpd:ignore
#    markers excludes them, so a justified exception needs no threshold change.
mkdir -p "$WORK/ignored"
for f in one two; do
	{
		echo "// jscpd:ignore-start"
		emit_body "${f}_copy"
		echo "// jscpd:ignore-end"
	} > "$WORK/ignored/$f.rs"
done
run_sut "$WORK/ignored"
check "jscpd:ignore markers exclude a clone" 0 "$RC" "$OUT" "no duplicated code"

# 4. No `npx` on PATH -> the gate reports itself broken (rc 2), never a pass.
#    `git` stays on the stripped PATH so the script still resolves the repo root
#    (and so this case cannot pass for the unrelated missing-config reason).
mkdir -p "$WORK/bin"
ln -sf "$(command -v git)" "$WORK/bin/git"
OUT="$WORK/out"
set +e
env PATH="$WORK/bin" "$SUT" "$WORK/clean" >"$OUT" 2>&1
RC=$?
set -e
check "missing npx is a broken gate, not a pass" 2 "$RC" "$OUT" "no \`npx\` on PATH"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
