#!/bin/bash
# Roll prod back to a specific prior main commit. Run as the `work` step of a
# `rollback` job (.chug/jobs/rollback.yaml). The target commit comes from the
# job's declared `sha` input, delivered as $CHUG_INPUT_SHA (spec §1.1 `inputs:`,
# design docs/design/311-job-inputs.md).
#
# THE EFFECT IS EXTERNAL. Revoking the job does not un-deploy the commit, so
# everything below is arranged to fail CLOSED and to say what it is about to do
# before it does it: resolve the input against the real repository, refuse
# anything that is not a commit on main, print the target and its distance from
# main, and only then hand off to the ssh. It is idempotent to the extent
# update.sh is — a re-run of the same SHA finds `.deployed-sha` already matching
# and no-ops ("already deployed X — nothing to do").
#
# bash, not /bin/sh: `set -o pipefail` is not POSIX, and a silently-swallowed
# failure upstream of a pipe is exactly the class of bug that would let this
# script ship the wrong commit. Both agent images are Debian-based
# (deploy/dev/Dockerfile.agent, deploy/prod/Dockerfile.agent-rust), so bash is
# present.
set -euo pipefail

# The missing-value check, and deliberately the ONLY one: an input with no
# resolved value injects no key at all (#311 Decision 4, "absent means absent"),
# so `set -u` on a bare expansion aborts the run with "CHUG_INPUT_SHA: unbound
# variable" before anything external happens. Do NOT write ${CHUG_INPUT_SHA:-…}
# here — a defaulted rollback target ships a commit nobody asked for, which is
# precisely the failure mode "absent means absent" exists to make impossible.
SHA="$CHUG_INPUT_SHA"

# Where a validated target is checked against reality. `main` on the platform
# repo is the only legitimate rollback target set, and mere existence is NOT
# enough to check, for two independent reasons:
#
#   1. The Mini's update.sh resolves the ref we pass with
#      `git rev-parse --verify --quiet "$ref^{commit}" || git rev-parse origin/main`
#      — a SHA it cannot resolve makes it deploy CURRENT MAIN instead, silently.
#      That is the opposite of a rollback, and it is why the refusal has to live
#      here, on this side of the ssh.
#   2. A commit that was never on main (a job branch, an abandoned attempt) has
#      never passed a merge gate. Shipping one to prod is not a rollback either.
#
# The job branch's clone is `--single-branch` on job/{seq}, so main is fetched
# explicitly under its own refspec rather than assumed present.
MAIN_REF="refs/remotes/origin/main"

echo "rollback: requested target '$SHA' — fetching origin main to resolve it"
git fetch --quiet --filter=blob:none origin "+refs/heads/main:$MAIN_REF"

if ! FULL_SHA="$(git rev-parse --verify --quiet "${SHA}^{commit}")"; then
  echo "rollback: '$SHA' is not a commit in this repository — refusing to deploy anything" >&2
  exit 1
fi
if ! git merge-base --is-ancestor "$FULL_SHA" "$MAIN_REF"; then
  echo "rollback: $FULL_SHA is not on main's history — refusing (only a commit that reached main has been gated and built)" >&2
  exit 1
fi

# Say it before doing it. The resolved 40-char SHA is what actually ships: the
# input may be a 7-char abbreviation, and an abbreviation is ambiguous on the
# Mini's repository in a way it is not here.
MAIN_SHA="$(git rev-parse "$MAIN_REF")"
BEHIND="$(git rev-list --count "$FULL_SHA..$MAIN_REF")"
echo "rollback: target  $FULL_SHA"
echo "rollback:         $(git log -1 --format='%ad  %s' --date=short "$FULL_SHA")"
echo "rollback:         $BEHIND commit(s) behind main ($MAIN_SHA)"
if [ "$FULL_SHA" = "$MAIN_SHA" ]; then
  # Not an error — shipping main is a legal thing to ask for and update.sh will
  # no-op if it is already deployed — but it is worth being loud about, because
  # an operator who typed this expecting a rollback got the opposite.
  echo "rollback: NOTE the target IS current main; this ships main and rolls nothing back"
fi

# One ssh path, one deploy-key path, one self-restart contract: .chug/tasks/deploy.sh
# already owns all three (including the §3.6 reconciliation note that applies
# identically here — the dispatcher supervising this job is restarted by the
# deploy it is running). It takes the target SHA as its optional argument, so a
# rollback IS a deploy of a resolved older commit. Do not fork a second copy of
# that code; fix it there and both paths get the fix.
HERE="$(cd "$(dirname "$0")" && pwd)"
echo "rollback: handing off to deploy.sh with the resolved SHA"
exec "$HERE/deploy.sh" "$FULL_SHA"
