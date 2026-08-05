#!/bin/sh
# Shell test for doc-lint.sh — no NATS, no Docker.
#
# It drives doc-lint.sh in explicit-file mode (paths as positional args, which
# bypass the diff selection) over fixtures written to a temp dir, and asserts
# the behaviours the brief calls out: a clean doc passes, a broken relative link
# fails, and a nonexistent code path only warns (still passes). A `.txt`
# argument exercises the self-skip (no markdown to lint).
#
# The referenced-path cases (design #415 S1a) and the constant-value cases
# (S1c) need git, because both checks resolve against the index — `git ls-files`
# and `git grep` — and not the filesystem. They run inside a throwaway
# `git init` repo under $WORK/repo (`run_sut_repo`) so the fixture owns both
# what is tracked and what the tree's constants are, and they are skipped whole
# if git is absent.
#
# The design-filename rule (rule 4) matches on the *repo-relative* path, so its
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

# 5. A conforming design filename -> pass.
printf '# Something\n\nContent.\n' > "$WORK/docs/design/366-something.md"
run_sut_rel docs/design/366-something.md
check "conforming design filename passes" 0 "$RC" "$OUT" "0 error(s)"

# 6. A design doc missing its `{seq}-` prefix -> fail, naming file and shape.
printf '# Something\n\nContent.\n' > "$WORK/docs/design/something.md"
run_sut_rel docs/design/something.md
check "design filename without a seq prefix fails" 1 "$RC" "$OUT" \
	"docs/design/something.md: design doc filename must be {seq}-{slug}.md"

# 7. A slug with a character outside the class is just as wrong as a missing
#    prefix. Underscore is out of class under every locale.
printf '# Shouty\n\nContent.\n' > "$WORK/docs/design/366-shouty_slug.md"
run_sut_rel docs/design/366-shouty_slug.md
check "design filename with an underscore in the slug fails" 1 "$RC" "$OUT" \
	"docs/design/366-shouty_slug.md: design doc filename must be {seq}-{slug}.md"

# 7b. Uppercase alone, with nothing else out of class — this is the case that
#     regresses if the slug pattern goes back to a collation-dependent `a-z`
#     range, which under en_US.UTF-8 spans the uppercase letters too.
printf '# Shouty\n\nContent.\n' > "$WORK/docs/design/366-ShoutySlug.md"
run_sut_rel docs/design/366-ShoutySlug.md
check "design filename with an uppercase slug fails" 1 "$RC" "$OUT" \
	"docs/design/366-ShoutySlug.md: design doc filename must be {seq}-{slug}.md"

# 8. Only files *directly* under docs/design/ are design docs.
mkdir -p "$WORK/docs/design/notes"
printf '# Scratch\n\nContent.\n' > "$WORK/docs/design/notes/scratch.md"
run_sut_rel docs/design/notes/scratch.md
check "nested subdirectory is out of the filename rule's scope" 0 "$RC" "$OUT" "0 error(s)"

# 9. The rule anchors on the repo-relative path: an absolute path that merely
#    ends in docs/design/<name>.md belongs to some other tree (this is what
#    keeps the sibling.md fixture and case 1 working).
run_sut "$WORK/docs/design/something.md"
check "absolute docs/design path is not gated by the filename rule" 0 "$RC" "$OUT" "0 error(s)"

# --- Referenced-path check (design #415 S1a) --------------------------------
# A git fixture whose tracked set the cases control. `crates/pkg/target/` is
# present on disk and NOT tracked, standing in for build output: that is the
# fresh-worktree-vs-built-checkout divergence the check exists to avoid, so it
# must warn all the same.
if ! command -v git >/dev/null 2>&1; then
	echo "skip - referenced-path cases (git unavailable)"
else
	REPO="$WORK/repo"
	mkdir -p "$REPO/crates/pkg/src" "$REPO/crates/pkg/target" "$REPO/docs"
	printf 'x\n' > "$REPO/crates/pkg/src/lib.rs"
	printf 'x\n' > "$REPO/crates/pkg/target/built.rs"
	git -C "$REPO" -c init.defaultBranch=main init -q
	git -C "$REPO" add crates/pkg/src >/dev/null 2>&1 || true

	run_sut_repo() { # <repo-relative file> — run from $REPO so it is the root
		OUT="$WORK/out"
		set +e
		(cd "$REPO" && "$SUT" "$@") >"$OUT" 2>&1
		RC=$?
		set -e
	}
	write_doc() { # <name> <body-line>...
		name="$1"; shift
		{ printf '# Fixture\n\n'; for l in "$@"; do printf '%s\n' "$l"; done; } \
			> "$REPO/docs/$name"
		git -C "$REPO" add "docs/$name" >/dev/null 2>&1 || true
	}

	# 10. A tracked file and a tracked directory claim both resolve.
	write_doc tracked.md 'See `crates/pkg/src/lib.rs` and `crates/pkg/` for it.'
	run_sut_repo docs/tracked.md
	check "tracked file and directory claims resolve" 0 "$RC" "$OUT" "0 error(s), 0 warning(s)"

	# 11. Present on disk but untracked -> still warns. This is the property that
	#     keeps the verdict identical in a fresh worktree and a built checkout.
	write_doc untracked.md 'Build output lands in `crates/pkg/target/built.rs` here.'
	run_sut_repo docs/untracked.md
	check "untracked-but-present path warns (git, not the filesystem)" 0 "$RC" "$OUT" \
		"referenced path not found -> crates/pkg/target/built.rs"

	# 12. The four false-positive classes are skipped silently.
	write_doc globs.md 'Every `crates/*/src/lib.rs` and `crates/**/*.rs` and `docs/?.md`.'
	run_sut_repo docs/globs.md
	check "globs are not path claims" 0 "$RC" "$OUT" "0 error(s), 0 warning(s)"

	write_doc absolute.md 'Mounted at `/dev/kvm`, `/workspace/chug-output.tar.gz`, `~/.ssh/config`.'
	run_sut_repo docs/absolute.md
	check "absolute and home-relative paths are not this tree" 0 "$RC" "$OUT" \
		"0 error(s), 0 warning(s)"

	write_doc templates.md 'Named `crates/{name}/src/lib.rs`, `crates/<name>.yaml`, `$ROOT/x.rs`, `crates/.../x.rs`.'
	run_sut_repo docs/templates.md
	check "placeholder templates are patterns, not claims" 0 "$RC" "$OUT" \
		"0 error(s), 0 warning(s)"

	write_doc citation.md 'Defined at `crates/pkg/src/lib.rs:193` and `crates/pkg/src/lib.rs:42-79`.'
	run_sut_repo docs/citation.md
	check "path:line citation resolves on the file" 0 "$RC" "$OUT" "0 error(s), 0 warning(s)"

	# 13. A citation whose FILE is missing still warns — the suffix is stripped,
	#     the file is not excused.
	write_doc citation-stale.md 'Defined at `crates/gone/src/lib.rs:193`.'
	run_sut_repo docs/citation-stale.md
	check "path:line citation still checks the file" 0 "$RC" "$OUT" \
		"referenced path not found -> crates/gone/src/lib.rs:193"

	# 14. A token rooted somewhere other than this checkout is refused, not judged.
	write_doc foreign.md 'Beacon has `src/api/types.gen.ts` and `dispatcher/tests/execution.rs`.'
	run_sut_repo docs/foreign.md
	check "foreign-rooted paths are skipped, not warned" 0 "$RC" "$OUT" \
		"0 error(s), 0 warning(s)"

	# 15. All three markers suppress the warning on the line that carries them.
	write_doc markers.md \
		'Images come from `.chug/images.yaml`. <!-- intent -->' \
		'The bundle is built into `web/dist`. <!-- runtime -->' \
		'`docs/design/epics.md` was never written. <!-- absent -->'
	run_sut_repo docs/markers.md
	check "intent, runtime and absent markers suppress the warning" 0 "$RC" "$OUT" \
		"0 error(s), 0 warning(s)"

	# 16. A marker is line-scoped: the unmarked line beside it still warns.
	write_doc marker-scope.md \
		'Images come from `.chug/images.yaml`. <!-- intent -->' \
		'State lives in `crates/dispatcher/src/state.rs`.'
	run_sut_repo docs/marker-scope.md
	check "a marker does not leak to the next line" 0 "$RC" "$OUT" \
		"referenced path not found -> crates/dispatcher/src/state.rs"

	# --- Constant values, design #415 D6 check 2 (S1c) ----------------------
	# The fixture owns the tree's side: `CONFIG_SCHEMA_EPOCH` is 7 here, so
	# every claim of 6 below is a mismatch and every claim of 7 agrees.
	# `MAX_BLOB_BYTES` is expression-valued and `DUPLICATE_EPOCH` resolves to two
	# disagreeing consts — both are refusals to judge, not findings.
	printf 'pub const CONFIG_SCHEMA_EPOCH: u32 = 7;\npub const MAX_BLOB_BYTES: usize = 16 * 1024;\n' \
		> "$REPO/crates/pkg/src/version.rs"
	printf 'pub const DUPLICATE_EPOCH: u32 = 7;\n' > "$REPO/crates/pkg/src/one.rs"
	printf 'pub const DUPLICATE_EPOCH: u32 = 8;\n' > "$REPO/crates/pkg/src/two.rs"
	git -C "$REPO" add crates/pkg/src >/dev/null 2>&1 || true

	# 17. Every recognised assertion shape, each disagreeing with the fixture:
	#     one warning naming both values, and nothing else.
	shape_n=0
	for shape in \
		'The epoch `CONFIG_SCHEMA_EPOCH` is `6` today.' \
		'`CONFIG_SCHEMA_EPOCH` is currently 6.' \
		'`CONFIG_SCHEMA_EPOCH` is already **6** in the tree.' \
		'`CONFIG_SCHEMA_EPOCH` = **6** in the tree.' \
		'`CONFIG_SCHEMA_EPOCH` == 6 in the tree.' \
		'Bump `CONFIG_SCHEMA_EPOCH` (currently `6`) in the same commit.' \
		'| `CONFIG_SCHEMA_EPOCH` | 6 | the job-type schema epoch |' \
		'Version.rs holds `CONFIG_SCHEMA_EPOCH = 6` today.' \
		'Version.rs holds `pub const CONFIG_SCHEMA_EPOCH: u32 = 6;` today.'; do
		shape_n=$((shape_n + 1))
		write_doc "shape-$shape_n.md" "$shape"
		run_sut_repo "docs/shape-$shape_n.md"
		check "mismatched value warns: $shape" 0 "$RC" "$OUT" \
			"stale constant -> CONFIG_SCHEMA_EPOCH is 7 in the tree, not 6"
		check "mismatched value warns exactly once: $shape" 0 "$RC" "$OUT" \
			"0 error(s), 1 warning(s)"
	done

	# 18. A claim that agrees with the tree is silent, in both shapes.
	write_doc agrees.md \
		'The epoch `CONFIG_SCHEMA_EPOCH` is `7` today.' \
		'Version.rs holds `CONFIG_SCHEMA_EPOCH = 7`.'
	run_sut_repo docs/agrees.md
	check "a matching value passes silently" 0 "$RC" "$OUT" "0 error(s), 0 warning(s)"

	# 19. Everything that is not a value claim about today's tree stays silent.
	#     A mention with no value is the class that must never warn (#415 M7);
	#     the rest are shapes the check refuses to parse rather than guess.
	quiet_n=0
	for quiet in \
		'The dispatcher compares `CONFIG_SCHEMA_EPOCH` before it merges.' \
		'Bump `CONFIG_SCHEMA_EPOCH` 6 → 7 in the same commit.' \
		'Bump `CONFIG_SCHEMA_EPOCH` to 9 when the parser changes.' \
		'`CONFIG_SCHEMA_EPOCH` was 6 when job inputs landed.' \
		'A config declaring `CONFIG_SCHEMA_EPOCH` >= 6 parks pre-Work.' \
		'`CONFIG_SCHEMA_EPOCH` is 6th in the table.' \
		'`PROJECT_IMAGE_SCHEMA_EPOCH` is 6 once the slice lands.' \
		'`DUPLICATE_EPOCH` is 6 in one of the two files.' \
		'`MAX_BLOB_BYTES` is 16384 bytes.' \
		'`CONFIG_SCHEMA_EPOCH` is `6` <!-- intent -->'; do
		quiet_n=$((quiet_n + 1))
		write_doc "quiet-$quiet_n.md" "$quiet"
		run_sut_repo "docs/quiet-$quiet_n.md"
		check "not a value claim, stays silent: $quiet" 0 "$RC" "$OUT" \
			"0 error(s), 0 warning(s)"
	done

	# 20. The claim must be on the line that names the constant.
	write_doc next-line.md '`CONFIG_SCHEMA_EPOCH` is' '6 as of this writing.'
	run_sut_repo docs/next-line.md
	check "a value on the next line is not a claim" 0 "$RC" "$OUT" \
		"0 error(s), 0 warning(s)"

	# 21. Outside a git checkout the check refuses to judge rather than falling
	#     back to the filesystem. $WORK is a plain temp dir, not a repo.
	printf '# Plain\n\nCites `crates/pkg/src/lib.rs`.\n' > "$WORK/plain.md"
	run_sut_rel plain.md
	check "non-git root disables the check rather than guessing" 0 "$RC" "$OUT" \
		"referenced-path check disabled"
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
