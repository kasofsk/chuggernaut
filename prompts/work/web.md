# Implement the UI change

You are working on the **operator web UI** of the chuggernaut platform — the
SPA in `web/` (React 19 + React Router 7 + Vite + TypeScript, no component
library, no CSS framework). The change to make is described in the **Job
Brief** appended below. If there is no Job Brief, call `submit_result` with a
summary explaining that no ticket was provided, and exit non-zero.

Ground rules:

1. **Front-end only.** This job type exists so UI changes ship without the
   Rust pipeline. Do not touch `crates/`, `Cargo.*`, `deploy/`, or anything
   outside `web/` (and, only when the brief explicitly says so, `jobs/` or
   `prompts/`). If the brief genuinely requires an API change, stop: call
   `submit_result` explaining that this needs a `code` job instead, and exit
   non-zero.
2. Read `web/CLAUDE.md` first — it carries the conventions that bite,
   especially the **mobile rule**: any layout/spacing/width change must be
   verified at a ~360–390px viewport, not reasoned about in your head.
3. Match the existing component and CSS style; keep the change minimal.
4. Before submitting, prove it builds: `cd web && npm ci && npm run build`
   (that is `tsc -b && vite build` — type errors fail the build). A change
   that does not build is not done. Run the build in the **foreground and
   wait** for it — never as a background task (see the finish-line rules below).
5. Commit to the current branch (you are already on the job branch) with
   clear messages, and push.
6. Narrate with `update_status` as you go: after reading the brief (your
   one-line plan), after the meaningful edit(s), and before submitting.
7. Finish with `submit_result`: a short markdown `summary` of what changed and
   why — it becomes the squash-merge commit body. Structure it:
   - Open with **one plain sentence** stating the outcome — markdown-free; it is
     the readable first line of the squash commit body.
   - Then short `###` sections — `### What changed` (bulleted, per file or
     concern, inline code for paths/symbols), `### How verified` (commands run
     and their outcomes), `### Notes` (caveats, follow-ups, surprises). Omit any
     section that would be empty.
   - Prefer bullets to prose; no multi-sentence run-on paragraphs. Keep it
     brief — structure replaces neither brevity nor substance.

**Finish-line rules (this is a headless run — respect them or the work is lost):**

- Your session ends with your final message. Nothing runs after it — when your
  turn ends the container is torn down, so anything not yet committed and pushed
  is gone.
- **Never** run the build or anything load-bearing as a background task. Run it
  in the foreground and wait. A summary that says "waiting on the build" means
  you are not done — the container will die mid-run.
- **Commit and push before you compose your final summary.** A summary that
  describes unpushed work is a failure: the dispatcher sees an empty branch and
  discards the attempt. If your correct outcome is genuinely *no change*, say so
  explicitly in the summary.
- If the build is still running when you think you are done, you are not done:
  wait for it, then commit and push, then call `submit_result`.
