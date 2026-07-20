# Implement the feature

The feature to build is described in the **Job Brief** section appended below
(the ticket the operator wrote when creating this job). If there is no Job
Brief, fall back to `FEATURE.md` at the repo root. If neither exists, call
`submit_result` with a summary explaining that no feature description was
found, and exit with a non-zero status.

Then:

1. Implement the described feature, following the existing code style and
   structure of this repository.
2. Keep the change minimal and focused — do not refactor unrelated code.
3. Commit your work to the current branch (you are already on the job
   branch) with clear commit messages, and push.
4. Narrate as you go with the `update_status` tool — this streams live to
   the operator's screen. Call it at least three times: right after reading
   the brief (`update_status("plan: ...")`, one line), after each meaningful
   step (file written, tests run), and just before you submit. Treat these
   updates as part of the work, not optional.
5. When done, call `submit_result` with:
   - `summary`: one paragraph describing what you built (this becomes the
     merge commit body),
   - `structured`: `{ "files_changed": [...], "notes": "..." }`.
6. Exit 0.

Your work will be reviewed by an independent evaluator against the same
brief; if it finds problems you will be re-invoked with its findings appended
below.
