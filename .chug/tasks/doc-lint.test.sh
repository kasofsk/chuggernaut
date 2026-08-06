#!/bin/sh
# Shell test for doc-lint.sh — no NATS, no Docker.
#
# It drives doc-lint.sh in explicit-file mode (paths as positional args, which
# bypass the diff selection) over fixtures written to a temp dir, and asserts
# the behaviours the brief calls out: a clean doc passes and a broken relative
# link fails. A `.txt` argument exercises the self-skip (no markdown to lint).
#
# The referenced-path and constant-value cases moved to
# .chug/tasks/check-doc-facts.test.sh with the checks themselves (design #415
# S1b) — a suite follows the code it pins.
#
# The design-filename rule (rule 3) matches on the *repo-relative* path, so its
# cases run the script from inside $WORK with relative arguments (`run_sut_rel`)
# — that is the only way the temp-dir harness can present a path the rule sees
# as `docs/design/<name>.md`. The absolute-path cases double as the anchoring
# guard: they must stay unaffected by the rule.
#
# Run:  .chug/tasks/doc-lint.test.sh   (exits 0 if all cases pass)
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

run_sut_rel() { # <repo-relative file>... — same, but run from $WORK as the root
	OUT="$WORK/out"
	set +e
	(cd "$WORK" && "$SUT" "$@") >"$OUT" 2>&1
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

# 3. No markdown argument -> self-skip, pass.
printf 'not markdown\n' > "$WORK/notes.txt"
run_sut "$WORK/notes.txt"
check "no markdown self-skips" 0 "$RC" "$OUT" "nothing to lint, skipping"

# 4. A conforming design filename -> pass.
printf '# Something\n\nContent.\n' > "$WORK/docs/design/366-something.md"
run_sut_rel docs/design/366-something.md
check "conforming design filename passes" 0 "$RC" "$OUT" "0 error(s)"

# 5. A design doc missing its `{seq}-` prefix -> fail, naming file and shape.
printf '# Something\n\nContent.\n' > "$WORK/docs/design/something.md"
run_sut_rel docs/design/something.md
check "design filename without a seq prefix fails" 1 "$RC" "$OUT" \
	"docs/design/something.md: design doc filename must be {seq}-{slug}.md"

# 6. A slug with a character outside the class is just as wrong as a missing
#    prefix. Underscore is out of class under every locale.
printf '# Shouty\n\nContent.\n' > "$WORK/docs/design/366-shouty_slug.md"
run_sut_rel docs/design/366-shouty_slug.md
check "design filename with an underscore in the slug fails" 1 "$RC" "$OUT" \
	"docs/design/366-shouty_slug.md: design doc filename must be {seq}-{slug}.md"

# 6b. Uppercase alone, with nothing else out of class — this is the case that
#     regresses if the slug pattern goes back to a collation-dependent `a-z`
#     range, which under en_US.UTF-8 spans the uppercase letters too.
printf '# Shouty\n\nContent.\n' > "$WORK/docs/design/366-ShoutySlug.md"
run_sut_rel docs/design/366-ShoutySlug.md
check "design filename with an uppercase slug fails" 1 "$RC" "$OUT" \
	"docs/design/366-ShoutySlug.md: design doc filename must be {seq}-{slug}.md"

# 7. Only files *directly* under docs/design/ are design docs.
mkdir -p "$WORK/docs/design/notes"
printf '# Scratch\n\nContent.\n' > "$WORK/docs/design/notes/scratch.md"
run_sut_rel docs/design/notes/scratch.md
check "nested subdirectory is out of the filename rule's scope" 0 "$RC" "$OUT" "0 error(s)"

# 8. The rule anchors on the repo-relative path: an absolute path that merely
#    ends in docs/design/<name>.md belongs to some other tree (this is what
#    keeps the sibling.md fixture and case 1 working).
run_sut "$WORK/docs/design/something.md"
check "absolute docs/design path is not gated by the filename rule" 0 "$RC" "$OUT" "0 error(s)"

# --- `--emit-links`: rule 2's extractor with the verdict removed --------------
# The mode .chug/tasks/doc-staleness.sh's orphan half reads (design #415 D15,
# S12). It names paths against the tree, so it needs a checkout — the cases run
# in their own `git init` fixture rather than in $WORK.

check_absent() { # <name> <expected-rc> <actual-rc> <output-file> <must-NOT-contain>
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

# 9. Outside a checkout there is no tree to name paths against — a loud 2.
run_sut_rel --emit-links docs/design/good.md
check "--emit-links outside a checkout is a loud refusal" 2 "$RC" "$OUT" "not a git checkout"

if command -v git >/dev/null 2>&1; then
	REPO="$WORK/emit"
	mkdir -p "$REPO/docs/design"
	git -C "$REPO" -c init.defaultBranch=main init -q
	printf '# Notes\n' > "$REPO/notes.md"
	printf '# Sibling\n' > "$REPO/docs/design/sibling.md"
	cat > "$REPO/docs/design/links.md" <<'EOF'
# Links

Up to [notes](../../notes.md) and across to [the sibling](./sibling.md).
Off-tree: [home](https://example.invalid/) and [an anchor](#links).

```md
Fenced: [not a link record](sibling.md)
```
EOF
	git -C "$REPO" add . >/dev/null 2>&1
	git -C "$REPO" -c user.email=t@e -c user.name=t commit -qm fixture >/dev/null 2>&1

	OUT="$WORK/out"
	set +e
	(cd "$REPO" && "$SUT" --emit-links) >"$OUT" 2>&1
	RC=$?
	set -e
	check "--emit-links collapses ../ to a repo-relative path" 0 "$RC" "$OUT" \
		"$(printf 'docs/design/links.md\t3\tnotes.md')"
	check "--emit-links collapses ./ to a repo-relative path" 0 "$RC" "$OUT" \
		"$(printf 'docs/design/links.md\t3\tdocs/design/sibling.md')"
	check_absent "--emit-links drops an off-tree target" 0 "$RC" "$OUT" "example.invalid"
	check_absent "--emit-links drops an anchor-only target" 0 "$RC" "$OUT" \
		"$(printf 'docs/design/links.md\t4\t')"
	check_absent "--emit-links inherits the fence tracking, so an example is not a link" \
		0 "$RC" "$OUT" "$(printf 'docs/design/links.md\t7\t')"
else
	echo "skip - --emit-links fixture cases (git unavailable)"
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
