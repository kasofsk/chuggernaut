#!/bin/sh
# Comment lint (STYLE.md Tier 1): source carries DOCS, not comments.
#
# Two rules over Rust and TypeScript sources:
#
#   1. No non-doc comments. `//`, `/* */` and their trailing-on-a-code-line
#      forms are rejected outright. Doc comments — `///`, `//!`, `/** */`,
#      `/*! */` — are the only prose allowed to live in a source file.
#   2. A doc comment is at most 2 sentences. Anything longer is a doc page:
#      write it under `docs/` (or in the module's MODULES.md row) and let the
#      doc comment point at it.
#
# Why: scattered comments are a liability — they drift out of step with the
# code they annotate, nobody reviews them as a body of knowledge, and an agent
# reading the tree cannot tell a current one from a stale one. Docs are
# intentional and organized: one place to look, one place to update, and a job
# type (`docs`) that maintains them. Every comment this gate rejects is a
# sentence that belongs in a doc.
#
# Inner doc comments (`//!`, `/*! */`) are EXEMPT from rule 2 — deliberately.
# The module header (accepts / emits / guarantees / spec §, NORTH-STAR §4) and
# the crate-root `//!` pointing at its spec section are this repo's in-tree doc
# surface: registered in MODULES.md, reviewed as a contract, and structurally
# unable to scatter (one per module). Rule 1 still applies inside them — they
# are doc comments.
#
# RULE 1 IS ABSOLUTE; RULE 2 IS STILL A RATCHET. Job #342 deleted every non-doc
# comment in the tree (the rationale worth keeping was hoisted into
# `docs/implementation-notes.md`), so rule 1 no longer has pre-existing debt to
# grandfather: the default mode lints EVERY tracked Rust/TypeScript source, not
# just the changed ones, and one non-doc comment anywhere fails the gate. Rule 2
# still has ~500 over-long doc comments to work through, so it reports only
# blocks the diff ADDS a line inside — an edited doc comment gets trimmed rather
# than grandfathered (the STYLE.md `unwrap_used` precedent).
#
# Escape hatch: NONE for prose. The narrow allowlist below covers only comments
# a MACHINE reads — `jscpd:ignore-start`/`-end` (required by the Tier 1
# duplication rule), `SAFETY:` on an unsafe block, and the eslint/ts/prettier
# pragmas. Put a directive's justification on the directive line itself; put a
# change's rationale in the commit message (STYLE.md Tier 2 rule 5).
#
# Usage:
#   .chug/tasks/check-comments.sh            # rule 1 tree-wide, rule 2 on added lines
#   .chug/tasks/check-comments.sh <file>...  # explicit: lint every line of these files
#
# Exit: 0 = clean. 1 = violations (each is reported as file:line).
set -eu

root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# The awk linter. Reads one source file and prints one `file:line: message` per
# violation; `added` is either the literal ALL or a space-separated list of the
# line numbers the diff added, and `all_lines` makes rule 1 ignore it entirely.
#
# It is a scanner, not a set of line regexes, because `//` is only a comment
# outside a string: `"nats://127.0.0.1"` is code. String state carries across
# lines only where the language allows a literal to (Rust `"…"`, TS templates),
# so a stray apostrophe in JSX text cannot swallow the rest of a file.
check_comments_lint_file() { # <file> <lang> <added> <all_lines>
	awk -v file="$1" -v lang="$2" -v added="$3" -v all_lines="$4" '
	function is_added(n) { return added == "ALL" || index(addedset, " " n " ") > 0 }
	function in_scope(n) { return all_lines == "1" || is_added(n) }

	# Comments a machine reads, matched on the text right after the opener.
	function is_directive(t) {
		sub(/^[ \t*]*/, "", t)
		return t ~ /^jscpd:ignore-(start|end)/ || t ~ /^SAFETY:/ \
			|| t ~ /^eslint-/ || t ~ /^@ts-/ || t ~ /^prettier-ignore/ \
			|| t ~ /^biome-ignore/ || t ~ /^oxlint-/
	}

	# Approximate sentence count: terminators followed by whitespace or
	# end-of-text, plus one for a trailing fragment that never terminates.
	# Inline code, markdown links, the common abbreviations and dotted
	# numbers are neutralized first so they do not read as sentence ends.
	function sentences(t,   n) {
		gsub(/`[^`]*`/, " code ", t)
		gsub(/\[[^]]*\]\([^)]*\)/, " link ", t)
		gsub(/(e\.g|i\.e|etc|vs|cf|approx|Inc|No)\./, " ", t)
		gsub(/[0-9]\.[0-9]/, "0", t)
		n = 0
		while (match(t, /[.!?]+["'"'"')]*([ \t]|$)/)) {
			n++
			t = substr(t, RSTART + RLENGTH)
		}
		if (t ~ /[A-Za-z0-9]/) n++
		return n
	}

	function report(n, msg) {
		printf "ERROR %s:%d: %s\n", file, n, msg
		violations++
	}

	# Close the doc block under construction and judge its length. Inner doc
	# comments carry the module header contract and are exempt (see the file
	# header); a block nobody touched is pre-existing debt, not this diff.
	function doc_flush(   n) {
		if (!doc_open) return
		if (!doc_inner && doc_added) {
			n = sentences(doc_text)
			if (n > 2) {
				report(doc_line, "doc comment is " n " sentences (max 2) — " \
					"move the explanation into a doc under docs/ and leave a pointer")
			}
		}
		doc_open = 0; doc_text = ""; doc_inner = 0; doc_added = 0
	}

	BEGIN {
		addedset = " " added " "
		blk = 0; instr = 0; strq = ""
		doc_open = 0; doc_last = -1; doc_kind = ""; seen_doc = 0; violations = 0
	}

	{
		len = length($0)
		i = 1
		kind = ""   # doc kind found on THIS line: "" | outer | inner
		text = ""   # its prose
		while (i <= len) {
			if (blk > 0) {
				e = index(substr($0, i), "*/")
				if (blk == 2) {
					kind = blk_inner ? "inner" : "outer"
					text = text " " substr($0, i, (e > 0 ? e - 1 : len))
				}
				if (e == 0) { i = len + 1 } else { i = i + e + 1; blk = 0 }
				continue
			}
			if (instr) {
				c = substr($0, i, 1)
				if (c == "\\") { i += 2; continue }
				if (c == strq) { instr = 0; strq = "" }
				i++
				continue
			}
			c = substr($0, i, 1)
			c2 = substr($0, i, 2)
			c3 = substr($0, i, 3)
			prev = i > 1 ? substr($0, i - 1, 1) : ""
			if (c2 == "//") {
				# `https://`, `nats://` — a scheme separator, never a comment.
				if (prev == ":") { i += 2; continue }
				if (c3 == "///" || c3 == "//!") {
					kind = (c3 == "//!") ? "inner" : "outer"
					text = text " " substr($0, i + 3)
				} else if (!is_directive(substr($0, i + 2))) {
					if (in_scope(NR)) {
						report(NR, "comment — only doc comments (/// //! /** */) are allowed; " \
							"put the knowledge in a doc, the rationale in the commit message")
					}
				}
				i = len + 1
				continue
			}
			if (c2 == "/*") {
				if (c3 == "/**" || c3 == "/*!") {
					blk = 2
					blk_inner = (c3 == "/*!")
				} else {
					blk = 1
					if (!is_directive(substr($0, i + 2)) && in_scope(NR)) {
						report(NR, "block comment — only doc comments (/// //! /** */) are allowed; " \
							"put the knowledge in a doc, the rationale in the commit message")
					}
				}
				i += 2
				continue
			}
			if (c == "\"" || (lang == "ts" && (c == "'"'"'" || c == "`"))) {
				instr = 1; strq = c; i++
				continue
			}
			# A Rust char literal may hold a double quote; a lifetime marker
			# may not. Skip the literal, step over the lifetime.
			if (lang == "rust" && c == "'"'"'") {
				if (substr($0, i, 4) ~ /^'"'"'\\.'"'"'/) { i += 4 } \
				else if (substr($0, i, 3) ~ /^'"'"'[^'"'"']'"'"'/) { i += 3 } \
				else { i++ }
				continue
			}
			i++
		}

		# Only Rust `"…"` literals and TS templates survive a newline; anything
		# else still open at end-of-line was a false positive.
		if (instr && !(lang == "rust" && strq == "\"") && strq != "`") {
			instr = 0; strq = ""
		}

		if (kind != "") {
			if (doc_open && doc_last == NR - 1 && kind == doc_kind) {
				doc_text = doc_text " " text
			} else {
				doc_flush()
				doc_open = 1; doc_line = NR; doc_text = text; doc_kind = kind
				# TypeScript has no `//!`, so its module header is the first
				# doc block in the file — same contract, same exemption.
				doc_inner = (kind == "inner") || (lang == "ts" && !seen_doc)
				seen_doc = 1
				doc_added = 0
			}
			doc_last = NR
			if (is_added(NR)) doc_added = 1
		} else if (blk == 0) {
			doc_flush()
		}
	}

	END {
		doc_flush()
		exit violations > 0 ? 1 : 0
	}
	' "$1"
}

# rust | ts | "" (not a source file this gate owns). Generated and declaration
# files are excluded: their comments come from a generator, and the only way to
# satisfy the gate would be to hand-edit a DO-NOT-EDIT file.
check_comments_lang_of() { # <path>
	case "$1" in
	*.gen.ts | *.d.ts) echo "" ;;
	*.rs) echo "rust" ;;
	*.ts | *.tsx) echo "ts" ;;
	*) echo "" ;;
	esac
}

# The line numbers this diff adds to one file, as awk wants them: the `+a,b`
# side of each unified-diff hunk header, expanded.
check_comments_added_lines() { # <base> <file>
	git diff -U0 "$1"...HEAD -- "$2" 2>/dev/null | awk '
		/^@@/ {
			for (i = 1; i <= NF; i++) if ($i ~ /^\+[0-9]/) {
				split(substr($i, 2), a, ",")
				count = (a[2] == "" ? 1 : a[2])
				for (n = 0; n < count; n++) printf "%d ", a[1] + n
			}
		}
	'
}

violations=0
files=""
mode="ratchet"
base=""

if [ "$#" -gt 0 ]; then
	mode="explicit"
	for f in "$@"; do
		files="$files$f
"
	done
else
	cd "$root"
	# Same change-set computation as .chug/tasks/ci.sh: HEAD vs the merge-base
	# with origin/$BASE_BRANCH, which holds for both the evaluation run (job
	# branch) and the merge-gate rerun (candidate commit).
	if [ -n "${BASE_BRANCH:-}" ] \
		&& git fetch origin "$BASE_BRANCH:refs/remotes/origin/$BASE_BRANCH" >/dev/null 2>&1; then
		base="$(git merge-base HEAD "origin/$BASE_BRANCH" 2>/dev/null || true)"
	fi
	if [ -z "$base" ]; then
		# Rule 1 needs no diff — it holds over the whole tree — so an unknown
		# change-set degrades to rule 1 only rather than skipping the gate.
		echo "!!! check-comments: could not determine the changed lines (BASE_BRANCH unset"
		echo "!!!     or no merge-base) — rule 1 (no non-doc comments) is still enforced over"
		echo "!!!     the whole tree; the doc-length ratchet is NOT enforced for this run."
	fi
	files="$(git ls-files -- '*.rs' '*.ts' '*.tsx' 2>/dev/null || true)"
fi

checked=0
IFS='
'
for f in $files; do
	[ -n "$f" ] || continue
	[ -f "$f" ] || continue
	lang="$(check_comments_lang_of "$f")"
	[ -n "$lang" ] || continue
	all_lines=0
	if [ "$mode" = "explicit" ]; then
		lines="ALL"
	else
		all_lines=1
		lines=""
		[ -n "$base" ] && lines="$(check_comments_added_lines "$base" "$f")"
	fi
	checked=$((checked + 1))
	set +e
	out="$(check_comments_lint_file "$f" "$lang" "$lines" "$all_lines")"
	rc=$?
	set -e
	[ -n "$out" ] && echo "$out"
	[ "$rc" -eq 0 ] || violations=$((violations + 1))
done
unset IFS

if [ "$violations" -ne 0 ]; then
	echo "!!! check-comments: $violations file(s) with comment violations (STYLE.md Tier 1)."
	echo "!!!     Source carries docs, not comments: delete the comment and put what it"
	echo "!!!     said in a doc under docs/ (or the module's MODULES.md row), or in the"
	echo "!!!     commit message if it is rationale. Doc comments stay under 2 sentences."
	echo "!!!     Reproduce locally with: .chug/tasks/check-comments.sh"
	exit 1
fi

echo "check-comments: clean ($checked source file(s) checked)"
