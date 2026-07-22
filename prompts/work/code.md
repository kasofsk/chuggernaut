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
   do not chase them. Run these in the **foreground and wait** for them — never
   as a background task (see the finish-line rules below).
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

**Finish-line rules (this is a headless run — respect them or the work is lost):**

- Your session ends with your final message. Nothing runs after it — when your
  turn ends the container is torn down, so anything not yet committed and pushed
  is gone.
- **Never** run verification, a build, or anything load-bearing as a background
  task. Run it in the foreground and wait for it to finish. A summary that says
  "waiting on the test run" means you are not done — the container will die
  mid-run.
- **Commit and push before you compose your final summary.** A summary that
  describes unpushed work is a failure: the dispatcher sees an empty branch and
  discards the attempt. If your correct outcome is genuinely *no change*, say so
  explicitly in the summary — a non-empty summary is what distinguishes a
  deliberate no-op from an agent that died at the finish line.
- If verification is still running when you think you are done, you are not
  done: wait for it, then commit and push, then call `submit_result`.

An inline reviewer will judge your change against the same brief; if it finds
problems you will be re-invoked with its findings.
