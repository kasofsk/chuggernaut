#!/bin/sh
# Shell test for doc-lint.sh — no NATS, no Docker, no git required.
#
# It drives doc-lint.sh in explicit-file mode (paths as positional args, which
# bypass the diff selection) over fixtures written to a temp dir, and asserts
# the three behaviours the brief calls out: a clean doc passes, a broken
# relative link fails, and a nonexistent code path only warns (still passes).
# A `.txt` argument exercises the self-skip (no markdown to lint).
#
# Run:  tasks/doc-lint.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/doc-lint.sh"

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

run_sut() { # <file>... -> writes rc to $RC, output to $OUT
	OUT="$WORK/out"
	set +e
	"$SUT" "$@" >"$OUT" 2>&1
	RC=$?
	set -e
}

# A resolving sibling so the clean doc's relative link points somewhere real.
mkdir -p "$WORK/docs/design"
printf '# Sibling\n\nContent.\n' > "$WORK/docs/design/sibling.md"

# 1. Clean doc: well-formed markdown, a link that resolves -> pass.
cat > "$WORK/docs/design/good.md" <<'EOF'
# Good design

See [the sibling](sibling.md) for context.

```sh
echo fenced hashes are ignored: #notaheading
```
EOF
run_sut "$WORK/docs/design/good.md"
check "clean doc passes" 0 "$RC" "$OUT" "0 error(s)"

# 2. Broken relative link -> fail with the offending target named.
cat > "$WORK/docs/design/broken.md" <<'EOF'
# Broken link

See [the missing page](does-not-exist.md).
EOF
run_sut "$WORK/docs/design/broken.md"
check "broken relative link fails" 1 "$RC" "$OUT" "broken relative link -> does-not-exist.md"

# 3. Nonexistent code path -> warning only, still passes.
cat > "$WORK/docs/design/codepath.md" <<'EOF'
# Code path reference

The handler lives in `crates/nope/src/imaginary.rs` (does not exist yet).
EOF
run_sut "$WORK/docs/design/codepath.md"
check "missing code path warns, does not fail" 0 "$RC" "$OUT" \
	"referenced path not found -> crates/nope/src/imaginary.rs"

# 4. No markdown argument -> self-skip, pass.
printf 'not markdown\n' > "$WORK/notes.txt"
run_sut "$WORK/notes.txt"
check "no markdown self-skips" 0 "$RC" "$OUT" "nothing to lint, skipping"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
