# Manual change

You are making a change to the **chuggernaut** platform repo by hand — no agent,
no container. The job's scratch branch is **`job/{N}`** (the number in this job's
title/URL).

## What to do

1. **Push your commits to `job/{N}`** over the SSH front. From your checkout:

   ```sh
   # one-time: point a remote at the platform's git front + this job's branch
   git remote add chug ssh://git@100.116.243.42:2222/kasofsk/chuggernaut.git   # if not already
   git push chug HEAD:job/{N}
   ```

   Push whatever you like to `job/{N}` — that branch is this job's work; the
   platform diffs and (on pass) merges it. Reach `:2222` over LAN/Tailscale.

   Write the commit message that becomes the squash body in the same shape a
   work agent's summary uses: open with **one plain sentence** stating the
   outcome (markdown-free — the readable first line of the commit body), then
   short `###` sections — `### What changed` (bulleted, inline code for
   paths/symbols), `### How verified` (commands run and their outcomes),
   `### Notes` (caveats, follow-ups) — omitting any empty section. Prefer
   bullets to prose; keep it brief.

2. **Resolve this task `Pass`** once your commits are on `job/{N}`.

## What happens next

Resolving `Pass` moves the job to Evaluation. The evergreen **`ci`** evaluator
(`fmt` / `clippy` / `test`) runs against `job/{N}` and **gates the merge** —
if CI is red the job reworks, so make sure your change builds and passes tests
before you resolve. On green, the job's `wrap_up` squash-merges `job/{N}` into
`main` through the merge queue.

Resolve `Fail` only if you cannot make the change — the job escalates.
