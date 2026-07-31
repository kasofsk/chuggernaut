#!/bin/sh
# Shell test for check-comments.sh — no NATS, no Docker, no cargo, no network.
#
# It drives the gate in both modes over fixtures in a temp dir:
#
#   * Explicit mode (paths as arguments, every line linted) for the rules
#     themselves — what counts as a comment, what counts as a sentence, and the
#     three things that must NOT be flagged: `//` inside a string, a scheme
#     separator, and a machine-read directive.
#   * Default mode (no arguments) in a throwaway git repo with an origin, for
#     the split the gate now rests on: rule 1 (no non-doc comments) holds over
#     every tracked source, diff or no diff, while rule 2 (the 2-sentence doc
#     cap) still reports only blocks the diff adds a line inside.
#
# Plus: an undeterminable diff drops the doc-length ratchet (loud) but still
# enforces rule 1 — the tree has no non-doc comments to grandfather.
#
# Plus the gate's own failure modes, which must never read as "this file is
# dirty": a source file holding an astral-plane character lints clean (macOS
# BWK awk aborts on one in a UTF-8 locale, hence the LC_ALL=C pin), non-ASCII
# prose counts the same sentences as ASCII prose does, and an awk that exits
# non-zero is reported as a LINTER ERROR with exit 2, not as a violation.
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

cat >"$WORK/astral.rs" <<'EOF'
/// Every trailing byte of the chunk is the marker.
pub fn astral(tail: &str) -> bool {
    assert!(tail.chars().all(|c| c == '🚀'));
    let banner = "— shipped ✅";
    banner.contains('✅')
}
EOF
run_sut "$WORK/astral.rs"
check "an astral-plane character lints clean" 0 "$RC" "$OUT" "check-comments: clean"

cat >"$WORK/prose.rs" <<'EOF'
/// Retries the fetch — the caller’s “budget” is already spent. Ok?
pub fn two() {}

/// First — with an em-dash. Second, with “curly” quotes. Third is too many.
pub fn three() {}
EOF
run_sut "$WORK/prose.rs"
check        "non-ASCII prose counts sentences the same" 1 "$RC" "$OUT" "prose.rs:4: doc comment is 3 sentences"
check_absent "a 2-sentence doc of non-ASCII prose passes" 1 "$RC" "$OUT" "prose.rs:1:"

cat >"$WORK/gen.gen.ts" <<'EOF'
// generated files are not linted
export const x = 1
EOF
run_sut "$WORK/gen.gen.ts"
check "generated file is skipped" 0 "$RC" "$OUT" "0 source file(s) checked"

# --- default mode: rule 1 tree-wide, rule 2 on added lines -------------------

REPO="$WORK/repo"
mkdir -p "$REPO/crates/demo/src"
git init -q -b main "$REPO"
cat >"$REPO/crates/demo/src/lib.rs" <<'EOF'
/// Pre-existing doc over the cap. It is untouched by the diff. Third sentence.
pub fn old() -> u32 {
    1
}
EOF
cat >"$REPO/crates/demo/src/legacy.rs" <<'EOF'
// pre-existing comment in a file no diff touches
pub fn legacy() -> u32 {
    9
}
EOF
git -C "$REPO" add -A
git -C "$REPO" commit -qm "seed"
git init -q --bare "$WORK/origin.git"
git -C "$REPO" remote add origin "$WORK/origin.git"
git -C "$REPO" push -q origin main
git -C "$REPO" checkout -q -b job

run_default() { # -> $RC / $OUT, run from inside the temp repo
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
run_default
check        "a comment in an untouched file fails rule 1" 1 "$RC" "$OUT" "legacy.rs:1: comment"
check_absent "an untouched over-long doc stays ratcheted"  1 "$RC" "$OUT" "lib.rs:1: doc comment"

git -C "$REPO" rm -q "$REPO/crates/demo/src/legacy.rs"
git -C "$REPO" commit -qm "delete the legacy comment"
run_default
check "a tree with no non-doc comment passes" 0 "$RC" "$OUT" "check-comments: clean"

cat >>"$REPO/crates/demo/src/lib.rs" <<'EOF'

// a comment this diff adds
pub fn added_dirty() -> u32 {
    3
}
EOF
git -C "$REPO" commit -qam "dirty addition"
run_default
check "a comment the diff adds fails" 1 "$RC" "$OUT" "lib.rs:11: comment"

# --- default mode: no diff to ratchet against --------------------------------

run_baseless() { # -> $RC / $OUT, with BASE_BRANCH unset
	OUT="$WORK/out"
	set +e
	(cd "$REPO" && unset BASE_BRANCH; "$SUT") >"$OUT" 2>&1
	RC=$?
	set -e
}

run_baseless
check "an undeterminable diff still enforces rule 1" 1 "$RC" "$OUT" "lib.rs:11: comment"

git -C "$REPO" checkout -q HEAD~1 -- crates/demo/src/lib.rs
git -C "$REPO" commit -qm "revert the dirty addition"
run_baseless
check "an undeterminable diff reports the doc-length ratchet as unenforced" 0 "$RC" "$OUT" "NOT enforced"

# --- the linter's own failure is not a violation -----------------------------

mkdir -p "$WORK/fakebin"
cat >"$WORK/fakebin/awk" <<'EOF'
#!/bin/sh
echo "awk: towc: multibyte conversion failure on: 'x'" >&2
exit 2
EOF
chmod +x "$WORK/fakebin/awk"

OUT="$WORK/out"
set +e
PATH="$WORK/fakebin:$PATH" "$SUT" "$WORK/clean.rs" >"$OUT" 2>&1
RC=$?
set -e
check        "an aborted scan exits 2 as a linter error" 2 "$RC" "$OUT" "LINTER ERROR on $WORK/clean.rs"
check        "the awk message is surfaced"               2 "$RC" "$OUT" "multibyte conversion failure"
check_absent "an aborted scan is not a comment violation" 2 "$RC" "$OUT" "comment violations"

echo
echo "check-comments.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
