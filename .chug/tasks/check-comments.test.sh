#!/bin/sh
# Shell test for check-comments.sh — no NATS, no Docker, no cargo, no network.
#
# It drives the gate in both modes over fixtures in a temp dir:
#
#   * Explicit mode (paths as arguments, every line linted) for the rules
#     themselves — what counts as a comment, what counts as a sentence, and the
#     three things that must NOT be flagged: `//` inside a string, a scheme
#     separator, and a machine-read directive.
#   * Ratchet mode (no arguments) in a throwaway git repo with an origin, for
#     the property the whole gate rests on: a comment the diff ADDS fails, a
#     comment that was already there does not.
#
# Plus: an undeterminable diff reports the ratchet as unenforced (rc 0, loud) —
# a ratchet with nothing to ratchet against must not fail every job forever.
#
# Run:  .chug/tasks/check-comments.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/check-comments.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

GIT_AUTHOR_NAME="check-comments test"
GIT_AUTHOR_EMAIL="test@example.invalid"
GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME"
GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

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

check_absent() { # <name> <expected-rc> <actual-rc> <output-file> <must-not-contain>
	name="$1"; want="$2"; got="$3"; out="$4"; needle="$5"
	if [ "$got" = "$want" ] && ! grep -qF "$needle" "$out"; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc want=$want got=$got; expected output NOT to contain: $needle"
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

# --- explicit mode: the rules ------------------------------------------------

cat >"$WORK/clean.rs" <<'EOF'
//! Module header prose is exempt from the sentence cap. It carries the
//! accepts/emits/guarantees contract, so it is allowed to run long. Third.

/// The job id this record belongs to.
pub fn clean(input: &str) -> bool {
    let url = "nats://127.0.0.1:4222";
    let quote = '"';
    let embedded = "a // b";
    // SAFETY: directives are machine-read, not prose.
    url.contains(embedded) || quote == '"'
}
EOF
run_sut "$WORK/clean.rs"
check "clean rust file passes" 0 "$RC" "$OUT" "check-comments: clean"

cat >"$WORK/dirty.rs" <<'EOF'
// a plain comment
pub fn a() {}

pub fn b() {} // a trailing comment

/* a block comment */
pub fn c() {}

/// One sentence. Two sentences. Three is one too many.
pub fn d() {}
EOF
run_sut "$WORK/dirty.rs"
check "plain comment is rejected"    1 "$RC" "$OUT" "dirty.rs:1: comment"
check "trailing comment is rejected" 1 "$RC" "$OUT" "dirty.rs:4: comment"
check "block comment is rejected"    1 "$RC" "$OUT" "dirty.rs:6: block comment"
check "3-sentence doc is rejected"   1 "$RC" "$OUT" "dirty.rs:9: doc comment is 3 sentences"

cat >"$WORK/mod.ts" <<'EOF'
/**
 * The first doc block is the TypeScript module header. It is exempt from the
 * sentence cap, exactly as a Rust `//!` header is. Third sentence.
 */
export function a(): string {
  const href = "https://example.com"
  return href
}

/** First. Second. Third sentence too many. */
export const b = 1
EOF
run_sut "$WORK/mod.ts"
check        "ts item doc over the cap is rejected" 1 "$RC" "$OUT" "mod.ts:10: doc comment is 3 sentences"
check_absent "ts module header is exempt"           1 "$RC" "$OUT" "mod.ts:1:"

cat >"$WORK/gen.gen.ts" <<'EOF'
// generated files are not linted
export const x = 1
EOF
run_sut "$WORK/gen.gen.ts"
check "generated file is skipped" 0 "$RC" "$OUT" "0 source file(s) checked"

# --- ratchet mode: added lines only ------------------------------------------

REPO="$WORK/repo"
mkdir -p "$REPO/crates/demo/src"
git init -q -b main "$REPO"
cat >"$REPO/crates/demo/src/lib.rs" <<'EOF'
// pre-existing comment, untouched by the diff
pub fn old() -> u32 {
    1
}
EOF
git -C "$REPO" add -A
git -C "$REPO" commit -qm "seed"
git init -q --bare "$WORK/origin.git"
git -C "$REPO" remote add origin "$WORK/origin.git"
git -C "$REPO" push -q origin main
git -C "$REPO" checkout -q -b job

run_ratchet() { # -> $RC / $OUT, run from inside the temp repo
	OUT="$WORK/out"
	set +e
	(cd "$REPO" && BASE_BRANCH=main "$SUT") >"$OUT" 2>&1
	RC=$?
	set -e
}

cat >>"$REPO/crates/demo/src/lib.rs" <<'EOF'

/// Added with no comment.
pub fn added() -> u32 {
    2
}
EOF
git -C "$REPO" commit -qam "clean addition"
run_ratchet
check "untouched pre-existing comment does not fail the ratchet" 0 "$RC" "$OUT" "check-comments: clean"

cat >>"$REPO/crates/demo/src/lib.rs" <<'EOF'

// a comment this diff adds
pub fn added_dirty() -> u32 {
    3
}
EOF
git -C "$REPO" commit -qam "dirty addition"
run_ratchet
check "a comment the diff adds fails the ratchet" 1 "$RC" "$OUT" "lib.rs:11: comment"

# --- ratchet mode: no diff to ratchet against --------------------------------

OUT="$WORK/out"
set +e
(cd "$REPO" && unset BASE_BRANCH; "$SUT") >"$OUT" 2>&1
RC=$?
set -e
check "an undeterminable diff reports the ratchet as unenforced" 0 "$RC" "$OUT" "NOT enforced"

echo
echo "check-comments.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
