#!/bin/sh
# Reusable CI task (a command task is just a script — see README.md).
# Wire it as an evaluator in a job type, or project-wide in jobs/_defaults.yaml:
#
#   eval:
#     - name: ci
#       type: command
#       run: ./tasks/ci.sh
#
# Exit 0 = pass, non-zero = fail. Replace the placeholder with your real
# test suite (cargo test, npm test, pytest, ...).
set -e
echo "tasks/ci.sh: no tests configured yet — edit this script"
exit 0
