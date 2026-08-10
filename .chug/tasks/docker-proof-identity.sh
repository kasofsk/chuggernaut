#!/bin/sh
# The measurement design #517 never decided — run as the `identity` stage-0
# evaluator of the `docker-proof` job type (.chug/jobs/docker-proof.yaml).
#
# `DockerGrant::admits` (crates/container/src/docker.rs) matches on
# (JOB_PROJECT, JOB_TYPE) read out of the composed launch env, and an EVALUATOR
# launch carries both stamps too (Core::container_env,
# crates/dispatcher/src/exec.rs). So the allow-list appears to be keyed per JOB
# TYPE rather than per LEVEL, which would mean this container — an evaluator of
# an allow-listed job type, declaring nothing — holds the socket as well.
#
# THIS SCRIPT MEASURES THAT AND CHANGES NOTHING. It reports and always passes:
# whether an evaluator should receive the socket is a design question for #517,
# not a defect this job type may decide, and job #507 is the precedent for
# finding a level-versus-type confusion the expensive way. A socket here is a
# FINDING to carry back to the design doc.
#
# It is the mirror of gcp-proof's `no-identity` evaluator in shape and its
# opposite in force: that one FAILS on what it finds, because #313 A5 had
# already decided non-inheritance. Nothing has decided this one.
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
		!!! docker-proof: FINDING for design #517 — this EVALUATOR container holds
		!!!     $SOCKET. DockerGrant::admits matched a launch at eval level, so the
		!!!     node's allow-list is per JOB TYPE and not per level: every evaluator
		!!!     of an allow-listed type holds node root for its run, including the
		!!!     appended \`ci\` one. Record it in docs/design/517-docker-access-for-jobs.md
		!!!     and decide it there; nothing here is changed on the strength of it.
	EOF
	VERDICT="MEASURED — an evaluator DOES receive the socket"
elif [ -e "$SOCKET" ]; then
	ls -ld "$SOCKET"
	VERDICT="MEASURED — $SOCKET exists here and is not a socket"
else
	VERDICT="MEASURED — no $SOCKET here, so the grant did not reach this evaluator"
fi

echo
echo "docker-proof: evaluator identity: $VERDICT"
exit 0
