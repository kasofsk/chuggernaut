#!/bin/sh
# MIGRATION BRIDGE — delete this file, and this directory, one release after the
# config-root move (spec §1.1).
#
# The chuggernaut config moved to `.chug/`, but a job loads its job type at the
# `base_ref` pinned when it launched. Every job released from a pre-move `main`
# therefore runs the `ci` evaluator that `jobs/_defaults.yaml` declared *there*
# — `./tasks/ci.sh`, executed inside the job-branch checkout, which is this
# file. Without it those jobs fail their gate on a missing script rather than on
# their own diff, including the very change that performs the move.
#
# Removal condition, both parts: a dispatcher that reads `.chug/` is deployed
# (deploy/prod/README.md §3), and no in-flight job still pins a pre-move
# `base_ref`. After that every job resolves `.chug/jobs/_defaults.yaml`, whose
# `ci` evaluator runs `./.chug/tasks/ci.sh` directly.
set -eu
exec ./.chug/tasks/ci.sh "$@"
