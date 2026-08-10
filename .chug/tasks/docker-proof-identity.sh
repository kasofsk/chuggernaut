#!/bin/sh
# The measurement design #517 left open and design #543 D5 decided — run as the
# `identity` stage-0 evaluator of the `docker-proof` job type
# (.chug/jobs/docker-proof.yaml).
#
# `DockerGrant::admits` (crates/container/src/docker.rs) matches on
# (JOB_PROJECT, JOB_TYPE) read out of the composed launch env, and an EVALUATOR
# launch carries both stamps too (Core::container_env,
# crates/dispatcher/src/exec.rs) — so until #543 S3 the allow-list was keyed per
# JOB TYPE and not per LEVEL, and job #542 measured this container holding the
# socket. S3 scoped the match to CHUG_PHASE=Work, so an evaluator of an
# allow-listed type now receives NOTHING, the appended `ci` one included.
#
# THIS SCRIPT MEASURES THAT AND CHANGES NOTHING. It reports and always passes:
# a socket here after S3 means the node's worker daemon predates S3 or the scope
# regressed, and either is a finding for the design doc rather than a defect the
# job type may decide. The absent case is now the expected one, and the reading
# is in docs/design/517-docker-access-for-jobs.md's 2026-08-10 S3 note.
#
# It is the mirror of gcp-proof's `no-identity` evaluator in shape and its
# opposite in force: that one FAILS on what it finds, because #313 A5 had
# already decided non-inheritance. This one still only reports even though #543
# D5 has now decided it, because a failing gate on a deploy the branch cannot
# influence measures the FLEET's worker version, not the branch's.
set -eu

SOCKET="${CHUG_DOCKER_SOCKET:-/var/run/docker.sock}"

echo "docker-proof: identity — the (project, job type) this container runs under"
echo "  JOB_PROJECT=${JOB_PROJECT:-<unset>}"
echo "  JOB_TYPE=${JOB_TYPE:-<unset>}"
echo "  CHUG_PHASE=${CHUG_PHASE:-<unset>}"
echo "  CHUG_EVALUATOR=${CHUG_EVALUATOR:-<none>}"
echo "  DOCKER_HOST=${DOCKER_HOST:-<unset>}"

if [ -S "$SOCKET" ]; then
	ls -l "$SOCKET"
	cat <<-EOF
		!!! docker-proof: FINDING for design #543 S3 — this EVALUATOR container still
		!!!     holds $SOCKET. The grant is meant to reach CHUG_PHASE=Work alone, so
		!!!     either this node's chug-worker predates S3 or the scope regressed:
		!!!     every evaluator of an allow-listed type holds node root for its run,
		!!!     including the appended \`ci\` one. Check the node's worker version
		!!!     first, then docs/design/543-placement-granularity.md D5; nothing here
		!!!     is changed on the strength of it.
	EOF
	VERDICT="MEASURED — an evaluator DOES receive the socket, which S3 says it must not"
elif [ -e "$SOCKET" ]; then
	ls -ld "$SOCKET"
	VERDICT="MEASURED — $SOCKET exists here and is not a socket"
else
	VERDICT="MEASURED — no $SOCKET here, so the grant is work-level as #543 S3 scoped it"
fi

echo
echo "docker-proof: evaluator identity: $VERDICT"
exit 0
