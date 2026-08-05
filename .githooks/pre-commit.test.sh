#!/bin/sh
# Shell test for .githooks/pre-commit — needs git and (for two cases) rustfmt;
# no NATS, no Docker, no cargo, no network.
#
# Each case builds a throwaway git repo with the hook installed via
# `core.hooksPath` (exactly how bootstrap_cmd wires it) and drives a real
# `git commit`, so what is asserted is the committed tree, not a dry run. The
# .chug/tasks/*.sh gates are absent from most fixtures — the hook skips a gate it
# cannot find — and stubbed where a specific exit code is the point.
#
# Run:  .githooks/pre-commit.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
HOOK="$HERE/pre-commit"
TASKS="$HERE/../.chug/tasks"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
OUT="$WORK/out"

check() { # <name> <expected-rc> <actual-rc> [must-contain]
	name="$1"
	want="$2"
	got="$3"
	needle="${4:-}"
	if [ "$got" = "$want" ] && { [ -z "$needle" ] || grep -qF "$needle" "$OUT"; }; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc want=$want got=$got${needle:+; expected output to contain: $needle}"
		echo "----- output -----"
		cat "$OUT"
		echo "------------------"
		fail=$((fail + 1))
	fi
}

check_text() { # <name> <expected> <actual>
	if [ "$2" = "$3" ]; then
		echo "ok   - $1"
		pass=$((pass + 1))
	else
		echo "FAIL - $1: want [$2] got [$3]"
		fail=$((fail + 1))
	fi
}

# A git repo with the hook installed the way bootstrap_cmd installs it.
new_repo() { # <name>
	repo="$WORK/$1"
	rm -rf "$repo"
	mkdir -p "$repo/.githooks"
	git init -q "$repo"
	cp "$HOOK" "$repo/.githooks/pre-commit"
	chmod +x "$repo/.githooks/pre-commit"
	git -C "$repo" config core.hooksPath .githooks
	git -C "$repo" config user.email hook-test@example.com
	git -C "$repo" config user.name "Hook Test"
	git -C "$repo" config commit.gpgsign false
	printf '[workspace.package]\nedition = "2024"\n' >"$repo/Cargo.toml"
	printf '%s' "$repo"
}

commit() { # <repo> <message>  -> rc in $RC, output in $OUT, wall-clock ms in $MS
	_start="$(date +%s%N 2>/dev/null || echo 0)"
	set +e
	git -C "$1" commit -q -m "$2" >"$OUT" 2>&1
	RC=$?
	set -e
	_end="$(date +%s%N 2>/dev/null || echo 0)"
	MS=0
	case "$_start$_end" in
	*[!0-9]* | '') ;;
	*) MS=$(((_end - _start) / 1000000)) ;;
	esac
}

have_rustfmt=1
command -v rustfmt >/dev/null 2>&1 || have_rustfmt=0
[ "$have_rustfmt" -eq 1 ] || echo "note - rustfmt absent: the formatting cases are SKIPPED"

MISFORMATTED='fn  main( ) {
let x=1;
println!("{x}");
}
'

# --- case 1: mis-formatted Rust is fixed, re-staged, and commits -----------
if [ "$have_rustfmt" -eq 1 ]; then
	repo="$(new_repo fmt_fix)"
	mkdir -p "$repo/src"
	printf '%s' "$MISFORMATTED" >"$repo/src/main.rs"
	git -C "$repo" add -A
	commit "$repo" "add main"
	check "mis-formatted Rust commits cleanly" 0 "$RC" "reformatted and re-staged"
	committed="$(git -C "$repo" show HEAD:src/main.rs)"
	expected="$(printf '%s' "$MISFORMATTED" | rustfmt --edition 2024)"
	check_text "the COMMITTED file is formatted" "$expected" "$committed"
	check_text "the worktree matches the commit" "" "$(git -C "$repo" status --porcelain)"
	echo "note - formatting commit took ${MS}ms"
fi

# --- case 2: a partially staged file is left alone, loudly ----------------
if [ "$have_rustfmt" -eq 1 ]; then
	repo="$(new_repo fmt_partial)"
	mkdir -p "$repo/src"
	printf 'fn main() {}\n' >"$repo/src/main.rs"
	git -C "$repo" add -A
	git -C "$repo" commit -q --no-verify -m base
	printf '%s' "$MISFORMATTED" >"$repo/src/main.rs"
	git -C "$repo" add src/main.rs
	printf '%sfn  extra( ) {}\n' "$MISFORMATTED" >"$repo/src/main.rs"
	commit "$repo" "partial"
	check "partially staged file commits with a warning" 0 "$RC" "left unformatted"
	check_text "the unstaged edit stayed out of the commit" "" \
		"$(git -C "$repo" show HEAD:src/main.rs | grep 'extra' || true)"
fi

# --- case 3: a docs-only commit runs no formatter and no npm ---------------
repo="$(new_repo docs_only)"
mkdir -p "$WORK/bin"
for b in rustfmt npx; do
	{
		echo '#!/bin/sh'
		echo "echo $b >>\"$WORK/calls\""
	} >"$WORK/bin/$b"
	chmod +x "$WORK/bin/$b"
done
: >"$WORK/calls"
printf '# Doc\n\nA line.\n' >"$repo/README.md"
git -C "$repo" add -A
PATH="$WORK/bin:$PATH" commit "$repo" "docs only"
check "docs-only commit passes" 0 "$RC" "nothing to check"
check_text "no formatter or npm process was started" "" "$(cat "$WORK/calls")"
echo "note - docs-only commit took ${MS}ms"

# --- case 4: missing tooling skips loudly, never blocks --------------------
mkdir -p "$WORK/nofmt"
for b in git sh date sed grep awk head tail cat basename dirname rm mv cp printf ls; do
	p="$(command -v "$b" 2>/dev/null || true)"
	[ -n "$p" ] && ln -sf "$p" "$WORK/nofmt/$b"
done
repo="$(new_repo no_rustfmt)"
mkdir -p "$repo/src"
printf '%s' "$MISFORMATTED" >"$repo/src/main.rs"
git -C "$repo" add -A
PATH="$WORK/nofmt" commit "$repo" "no rustfmt"
check "absent rustfmt skips loudly and still commits" 0 "$RC" \
	"no rustfmt on PATH — formatting SKIPPED"

# --- case 5: an unrunnable duplication gate skips; a clone rejects ---------
repo="$(new_repo dup_unrunnable)"
mkdir -p "$repo/.chug/tasks" "$repo/src"
{
	echo '#!/bin/sh'
	echo 'echo "!!! check-duplication: jscpd produced no report"'
	echo 'exit 2'
} >"$repo/.chug/tasks/check-duplication.sh"
chmod +x "$repo/.chug/tasks/check-duplication.sh"
printf 'fn main() {}\n' >"$repo/src/main.rs"
git -C "$repo" add -A
commit "$repo" "dup gate broken"
check "unrunnable duplication gate skips, commit lands" 0 "$RC" \
	"duplication check could not run — SKIPPED"

repo="$(new_repo dup_clone)"
mkdir -p "$repo/.chug/tasks" "$repo/src"
{
	echo '#!/bin/sh'
	echo 'echo "clone found"'
	echo 'exit 1'
} >"$repo/.chug/tasks/check-duplication.sh"
chmod +x "$repo/.chug/tasks/check-duplication.sh"
printf 'fn main() {}\n' >"$repo/src/main.rs"
git -C "$repo" add -A
commit "$repo" "duplicated"
check "a clone rejects the commit" 1 "$RC" "commit REJECTED"
check "the rejection names the bypass" 1 "$RC" "git commit --no-verify"

# --- case 6: comment lint — rule 1 rejects, old doc-comment debt does not --
repo="$(new_repo comments)"
mkdir -p "$repo/.chug/tasks" "$repo/src"
cp "$TASKS/check-comments.sh" "$repo/.chug/tasks/check-comments.sh"
chmod +x "$repo/.chug/tasks/check-comments.sh"
{
	echo '/// One. Two. Three. Four sentences of pre-existing debt.'
	echo 'pub fn old() {}'
} >"$repo/src/lib.rs"
git -C "$repo" add -A
git -C "$repo" commit -q --no-verify -m "debt"

printf 'pub fn added() {}\n' >>"$repo/src/lib.rs"
git -C "$repo" add src/lib.rs
commit "$repo" "unrelated line beside an over-long doc comment"
check "pre-existing doc-comment debt does not block" 0 "$RC"

printf '// a fresh comment\npub fn commented() {}\n' >>"$repo/src/lib.rs"
git -C "$repo" add src/lib.rs
commit "$repo" "adds a comment"
check "a newly added non-doc comment rejects" 1 "$RC" "comment lint failed"

# A scanner that aborts mid-file exits 2 — "the linter crashed", not "this file
# is dirty". The agent cannot fix that in-loop, so it is a skip.
repo="$(new_repo comments_linter_error)"
mkdir -p "$repo/.chug/tasks" "$repo/src"
{
	echo '#!/bin/sh'
	echo 'echo "!!! check-comments: LINTER ERROR on src/lib.rs"'
	echo 'exit 2'
} >"$repo/.chug/tasks/check-comments.sh"
chmod +x "$repo/.chug/tasks/check-comments.sh"
printf 'pub fn scanned() {}\n' >"$repo/src/lib.rs"
git -C "$repo" add -A
commit "$repo" "comment gate broken"
check "an aborted comment scan skips, commit lands" 0 "$RC" \
	"comment lint could not run — SKIPPED"

# --- case 6b: doc facts — a stale claim rejects, an unrunnable check skips --
# The two guards design #415 S1b names: the gate can now fail every job, so a
# check that cannot run must be loud and must not read as clean.
repo="$(new_repo doc_facts)"
mkdir -p "$repo/.chug/tasks" "$repo/crates/pkg/src" "$repo/docs"
cp "$TASKS/check-doc-facts.sh" "$repo/.chug/tasks/check-doc-facts.sh"
chmod +x "$repo/.chug/tasks/check-doc-facts.sh"
printf 'x\n' >"$repo/crates/pkg/src/lib.rs"
printf '# Doc\n\nState lives in `crates/pkg/src/gone.rs`.\n' >"$repo/docs/stale.md"
git -C "$repo" add -A
commit "$repo" "a stale path claim"
check "a stale doc claim rejects the commit" 1 "$RC" "referenced path not found"
check "the doc-fact rejection names the markers" 1 "$RC" "<!-- intent -->"

printf '# Doc\n\nState lives in `crates/pkg/src/lib.rs`.\n' >"$repo/docs/stale.md"
git -C "$repo" add -A
commit "$repo" "a resolving path claim"
check "a resolving doc claim commits" 0 "$RC"

repo="$(new_repo doc_facts_unrunnable)"
mkdir -p "$repo/.chug/tasks" "$repo/docs"
{
	echo '#!/bin/sh'
	echo 'echo "!!! check-doc-facts: not a git checkout"'
	echo 'exit 2'
} >"$repo/.chug/tasks/check-doc-facts.sh"
chmod +x "$repo/.chug/tasks/check-doc-facts.sh"
printf '# Doc\n\nContent.\n' >"$repo/docs/note.md"
git -C "$repo" add -A
commit "$repo" "doc-fact gate broken"
check "an unrunnable doc-fact check skips, commit lands" 0 "$RC" \
	"doc-fact check could not run — SKIPPED"

# --- case 7: prettier honours web/.prettierignore --------------------------
# `web/src/api/wire-samples.json` is Rust-emitted and a cargo test asserts its
# exact bytes; reformatting it here would fail CI after the agent has exited.
PRETTIER_VER="$(sed -n 's/.*"prettier"[[:space:]]*:[[:space:]]*"[^0-9]*\([0-9][0-9.]*\)".*/\1/p' \
	"$HERE/../web/package.json" 2>/dev/null | head -n1)"
have_prettier=0
if [ -n "$PRETTIER_VER" ] && command -v npx >/dev/null 2>&1 \
	&& npx --yes "prettier@$PRETTIER_VER" --version >/dev/null 2>&1; then
	have_prettier=1
fi
[ "$have_prettier" -eq 1 ] || echo "note - prettier unavailable: the .prettierignore case is SKIPPED"

if [ "$have_prettier" -eq 1 ]; then
	repo="$(new_repo prettierignore)"
	mkdir -p "$repo/web/src/api"
	printf '{ "devDependencies": { "prettier": "^%s" } }\n' "$PRETTIER_VER" >"$repo/web/package.json"
	printf 'dist\npackage-lock.json\nsrc/api/wire-samples.json\n' >"$repo/web/.prettierignore"
	GENERATED='{
  "knowledge_tags": [
    "web"
  ]
}
'
	printf '%s' "$GENERATED" >"$repo/web/src/api/wire-samples.json"
	printf 'export const  x   =  1\n' >"$repo/web/src/app.ts"
	git -C "$repo" add -A
	commit "$repo" "regenerate wire samples beside a source edit"
	check "a commit touching web/ lands" 0 "$RC"
	check_text "the ignored Rust-emitted JSON is committed byte-for-byte" \
		"$(printf '%sx' "$GENERATED")" \
		"$(git -C "$repo" show HEAD:web/src/api/wire-samples.json && printf x)"
	check_text "the sibling source file was formatted" "export const x = 1;" \
		"$(git -C "$repo" show HEAD:web/src/app.ts)"
	echo "note - web commit took ${MS}ms"
fi

# --- case 7b: the local install is run from web/, with web-relative paths ---
repo="$(new_repo prettier_local)"
mkdir -p "$repo/web/node_modules/.bin" "$repo/web/src"
printf '{ "devDependencies": { "prettier": "^3.9.6" } }\n' >"$repo/web/package.json"
{
	echo '#!/bin/sh'
	echo "echo \"\$PWD :: \$*\" >>\"$WORK/prettier-args\""
} >"$repo/web/node_modules/.bin/prettier"
chmod +x "$repo/web/node_modules/.bin/prettier"
: >"$WORK/prettier-args"
printf 'export const x = 1;\n' >"$repo/web/src/app.ts"
git -C "$repo" add -A
commit "$repo" "web edit"
check "a web commit with a local prettier lands" 0 "$RC"
check_text "prettier ran from web/ so it reads web/.prettierignore" \
	"$repo/web :: --write --ignore-unknown package.json src/app.ts" \
	"$(cat "$WORK/prettier-args")"

# --- case 8: finishing a conflicted cherry-pick is not gated ---------------
# git 2.39 commits a clean cherry-pick in-process, hooks and all skipped; the
# conflicted one is finished by a real `git commit` that does reach the hook.
repo="$(new_repo cherry_pick)"
mkdir -p "$repo/.chug/tasks" "$repo/src"
{
	echo '#!/bin/sh'
	echo 'echo "clone found"'
	echo 'exit 1'
} >"$repo/.chug/tasks/check-duplication.sh"
chmod +x "$repo/.chug/tasks/check-duplication.sh"
printf 'fn main() {}\n' >"$repo/src/main.rs"
git -C "$repo" add -A
git -C "$repo" commit -q --no-verify -m base
git -C "$repo" checkout -q -b side
printf 'fn main() { let _ = 1; }\n' >"$repo/src/main.rs"
git -C "$repo" add -A
git -C "$repo" commit -q --no-verify -m "someone else's change"
git -C "$repo" checkout -q master 2>/dev/null || git -C "$repo" checkout -q main
printf 'fn main() { let _ = 2; }\n' >"$repo/src/main.rs"
git -C "$repo" add -A
git -C "$repo" commit -q --no-verify -m "conflicting change"
set +e
git -C "$repo" cherry-pick side >/dev/null 2>&1
set -e
printf 'fn main() { let _ = 3; }\n' >"$repo/src/main.rs"
git -C "$repo" add src/main.rs
commit "$repo" "resolve the cherry-pick"
check "a conflicted cherry-pick is not gated" 0 "$RC" "CHERRY_PICK_HEAD present — skipping"

# --- case 9: the registry gate follows check-modules.sh, not a copied list -
repo="$(new_repo modules_trigger)"
mkdir -p "$repo/.chug/tasks" "$repo/crates/newctx/src"
: >"$WORK/modcalls"
{
	echo '#!/bin/sh'
	echo "echo ran >>\"$WORK/modcalls\""
	echo 'echo "!!! ci: crates/newctx/src/thing.rs has no row in docs/reference/modules.md"'
	echo 'exit 1'
} >"$repo/.chug/tasks/check-modules.sh"
chmod +x "$repo/.chug/tasks/check-modules.sh"
printf '# Doc\n\nA line.\n' >"$repo/README.md"
git -C "$repo" add -A
git -C "$repo" commit -q --no-verify -m base
printf '# Doc\n\nAnother line.\n' >"$repo/README.md"
git -C "$repo" add README.md
commit "$repo" "docs only"
check "a docs-only commit does not run the registry gate" 0 "$RC"
check_text "the registry gate was not called" "" "$(cat "$WORK/modcalls")"

printf 'pub fn thing() {}\n' >"$repo/crates/newctx/src/thing.rs"
git -C "$repo" add crates/newctx/src/thing.rs
commit "$repo" "a crate the hook has never heard of"
check "any staged crates/ path runs the registry gate" 1 "$RC" "docs/reference/modules.md registry drift"

echo
echo "pre-commit.test.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
