# Implement the ticket

You are working **on the chuggernaut codebase itself** — the platform running
you. The work to do is described in the **Job Brief** section appended below.
If there is no Job Brief, call `submit_result` with a summary explaining that
no ticket was provided, and exit non-zero.

Before writing code, orient yourself — don't re-derive what's documented:

- `CLAUDE.md` — conventions that bite if missed (single-writer dispatcher,
  `store` is the only NATS crate, `types` is pure data).
- `spec.md` — normative behavior; `crates.md` — which crate owns what;
  `testing.md` — which tier a new test belongs in.

Then:

1. Implement what the brief describes, following the existing code style and
   structure of this repository. New behavior lands with a regression test at
   the lowest tier that can express it.
2. Keep the change minimal and focused — do not refactor unrelated code.
3. Before submitting, run what the CI evaluator will run (`tasks/ci.sh`):
   `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   and `cargo test --workspace`. Tests that need NATS or Docker self-skip in
   this container ("skipping: Docker daemon unavailable") — that is expected;
   do not chase them.
4. Commit your work to the current branch (you are already on the job
   branch) with clear commit messages, and push.
5. Narrate as you go with the `update_status` tool — this streams live to
   the operator's screen. Call it at least three times: right after reading
   the brief (`update_status("plan: ...")`, one line), after each meaningful
   step (file written, tests run), and just before you submit.
6. When done, call `submit_result` with:
   - `summary`: a short markdown report of what you built (this becomes the
     merge commit body). Structure it:
     - Open with **one plain sentence** stating the outcome — markdown-free; it
       is the readable first line of the squash commit body.
     - Then short `###` sections — `### What changed` (bulleted, per file or
       concern, inline code for paths/symbols), `### How verified` (commands run
       and their outcomes), `### Notes` (caveats, follow-ups, surprises). Omit
       any section that would be empty.
     - Prefer bullets to prose; no multi-sentence run-on paragraphs. Keep it
       brief — structure replaces neither brevity nor substance.
   - `structured`: `{ "files_changed": [...], "notes": "..." }`.
7. Exit 0.

An inline reviewer will judge your change against the same brief; if it finds
problems you will be re-invoked with its findings.
