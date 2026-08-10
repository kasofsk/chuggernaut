#!/bin/sh
# The docker-grant proof ladder — design #517, and the FIRST consumer of the
# mechanism S1-S5a built. Run as the `work` step of a `docker-proof` job
# (.chug/jobs/docker-proof.yaml), pinned to the node whose own
# WORKER_DOCKER_GRANTS names this (project, job type) pair. Until an operator
# writes that entry, NOTHING HAS EVER RECEIVED THE SOCKET (#517 S5b), and
# proving the grant end to end against a real daemon is the whole point of this
# job type — #490 slice 6 is the precedent: five slices and fifteen green
# evaluators had passed before anyone ran the thing.
#
# THE DELIVERABLE IS STDOUT. The dispatcher harvests a command work task's
# container logs into its `stdout.log` artifact (spec §3.2,
# crates/platform-ops/src/harvest.rs) and a worker keeps only the LAST 700 KiB
# of them, so the LADDER summary is printed LAST, from the EXIT trap, on every
# path including a failure.
#
# EACH RUNG PROVES SOMETHING DIFFERENT, and a failure names which one broke:
#
#   1. the socket is bound   — the grant reached THIS launch, at the fixed path,
#                              and WRITABLE: no client dials a read-only bind
#   2. no DOCKER_HOST is set — the grant injects none (#517 S3), so a client
#                              must find the socket by convention
#   3. the daemon answers    — /_ping, /version, /info
#   4. an image builds       — the workload the whole of #313 half B argued for
#   5. the built image runs  — and prints the marker THIS run baked into it
#   6. cleanup, and prove it — re-listed rather than assumed
#
# IT SPEAKS THE ENGINE API OVER curl, NOT `docker` — gcp-proof.sh's move for
# gcloud (#313 gap 11), for the same reason and with the same honesty about
# what it costs: neither agent image carries a docker CLI (deploy/dev/Dockerfile.agent,
# deploy/prod/Dockerfile.agent-rust) and no job type here pulls a public image.
# So this proves the socket answers the calls a `docker build` makes; it does
# not exercise the CLI's own endpoint resolution. Rung 2 covers the half of
# that which the grant decides — the absent DOCKER_HOST a CLI would fall back
# from — and the rest is a claim about tooling nobody here installs.
#
# THE PREREQUISITE IS A DEPLOY, NOT A MERGE. A merge of this file grants
# nothing: it needs a worker daemon carrying #517 S3 (job #522) and a node whose
# WORKER_DOCKER_SOCKET/WORKER_DOCKER_GRANTS name this pair
# (docs/reference/runbooks/worker-docker-grant.md). Released before that, it fails at
# rung 1, and rung 1's message says which of the two is missing rather than
# reading as a broken daemon.
#
# IT MUST LEAVE THE NODE AS IT FOUND IT. A proof that leaks images onto a node
# is worse than no proof, and the node's daily `docker system prune` spares
# anything labelled chug.managed — which a build FROM an agent image inherits.
# So rung 6 removes and re-lists, a failed cleanup is a FAIL rather than a
# warning, and the EXIT trap sweeps best-effort when the ladder died before it.
#
# Every wait is bounded by wall clock (docs/reference/style.md Tier 2 rule 3) and every bound
# is named in the failure it produces. The socket path, the bounds and the base
# candidates are env-overridable for exactly one reason — so
# .chug/tasks/docker-proof.test.sh can drive the real ladder against stubs.
set -eu

SOCKET="${CHUG_DOCKER_SOCKET:-/var/run/docker.sock}"
HTTP_TIMEOUT_SECS="${CHUG_DOCKER_HTTP_TIMEOUT_SECS:-30}"
BUILD_TIMEOUT_SECS="${CHUG_DOCKER_BUILD_TIMEOUT_SECS:-600}"
RUN_TIMEOUT_SECS="${CHUG_DOCKER_RUN_TIMEOUT_SECS:-120}"
PULL_TIMEOUT_SECS="${CHUG_DOCKER_PULL_TIMEOUT_SECS:-300}"

# A base the node ALREADY HAS, preferred over a pull: a proof that fails on a
# registry hiccup measures the registry. These two are the images every
# container node builds on every deploy (deploy/prod/build-worker.sh), and the
# first of them is what this job type itself runs as, so the happy path pulls
# nothing at all.
BASE_CANDIDATES="${CHUG_DOCKER_BASE_CANDIDATES:-chuggernaut/agent:prod chuggernaut/agent-rust:prod}"
PULL_FALLBACK="${CHUG_DOCKER_PULL_FALLBACK:-alpine:3.20}"

RUN_ID="${JOB_ID:-local}"
IMAGE_REPO=chug-docker-proof
TAG="$IMAGE_REPO:$RUN_ID"
CONTAINER_NAME="chug-docker-proof-$RUN_ID"
# Baked into the image by rung 4 and read back out of the container's log by
# rung 5, so rung 5 proves it ran THIS build and not some stale namesake.
MARKER="docker-proof-ok-$RUN_ID-${CHUG_TASK_ID:-notask}"

RUNGS_MAX=6
RUNGS_CLEARED=0
LADDER=""
VERDICT="FAIL — the ladder did not finish"
WORK_DIR=""
BODY_FILE=""
ERR_FILE=""
BASE_IMAGE=""
PULLED_BASE=""
CONTAINER_ID=""
DAEMON_ANSWERED=""
CLEANUP_DONE=""

rung_title() {
	case "$1" in
	1) echo "the socket is bound" ;;
	2) echo "no DOCKER_HOST is set" ;;
	3) echo "the daemon answers" ;;
	4) echo "an image builds" ;;
	5) echo "the built image runs" ;;
	6) echo "cleanup, and prove it" ;;
	esac
}

rung_pass() {
	RUNGS_CLEARED="$1"
	LADDER="$LADDER
  rung $1 $(rung_title "$1"): PASS — $2"
	echo "docker-proof: rung $1 PASS — $2"
}

# Ends the run at the first failure. The ladder itself is printed by the EXIT
# trap, so a failure and a success leave stdout in the same readable shape; the
# rung that failed counts as reached, so the summary never reports it twice.
rung_fail() {
	RUNGS_CLEARED="$1"
	LADDER="$LADDER
  rung $1 $(rung_title "$1"): FAIL — $2"
	VERDICT="FAIL at rung $1 ($(rung_title "$1"))"
	echo "!!! docker-proof: RUNG $1 FAILED — $2" >&2
	exit 1
}

# The environment could not put the ladder in a position to judge this rung.
# Distinct from FAIL on purpose: nothing was disproved. Still a failed run — a
# proof that did not reach its rungs has proved nothing.
rung_unreachable() {
	VERDICT="FAIL — rung $1 ($(rung_title "$1")) was NOT REACHED"
	echo "!!! docker-proof: RUNG $1 NOT REACHED — $2" >&2
	exit 1
}

# The measurement #517 has not decided: DockerGrant::admits matches on
# (JOB_PROJECT, JOB_TYPE), which an EVALUATOR launch stamps too, so the grant
# looks per job type rather than per level. Printed here and by the
# `identity` stage-0 evaluator (.chug/tasks/docker-proof-identity.sh), so one
# release answers it from two containers' logs. Nothing here changes behaviour.
report_identity() {
	echo "docker-proof: identity — JOB_PROJECT=${JOB_PROJECT:-<unset>} JOB_TYPE=${JOB_TYPE:-<unset>} CHUG_PHASE=${CHUG_PHASE:-<unset>} CHUG_EVALUATOR=${CHUG_EVALUATOR:-<none>}"
	if [ -S "$SOCKET" ]; then
		echo "docker-proof: identity — $SOCKET is PRESENT in this container"
	else
		echo "docker-proof: identity — $SOCKET is ABSENT in this container"
	fi
	[ -n "${JOB_TYPE:-}" ] || cat <<-EOF
		!!! docker-proof: JOB_TYPE is UNSET, so DockerGrant::admits matches NOTHING at all
		!!!     (crates/container/src/docker.rs). That is a dispatcher predating design
		!!!     #517 S2 (job #521), not a mis-typed allow-list entry — deploy before
		!!!     reading rung 1 as a node-config problem.
	EOF
}

print_ladder() {
	echo
	echo "docker-proof: LADDER (design #517) — socket=$SOCKET tag=$TAG"
	printf '%s\n' "$LADDER" | sed '/^[[:space:]]*$/d'
	_n=$((RUNGS_CLEARED + 1))
	while [ "$_n" -le "$RUNGS_MAX" ]; do
		echo "  rung $_n $(rung_title "$_n"): NOT REACHED"
		_n=$((_n + 1))
	done
	echo "docker-proof: VERDICT $VERDICT"
}

# POSIX sh allows exactly ONE EXIT trap (the #186 web-publish lesson), so the
# sweep, the summary and the scratch dir share this one function.
cleanup() {
	sweep
	print_ladder
	if [ -n "$WORK_DIR" ]; then
		rm -rf "$WORK_DIR"
	fi
	return 0
}
trap cleanup EXIT INT TERM

# Best-effort removal for the paths that never reached rung 6. It is NOT the
# proof — rung 6 is, and it re-lists — but a ladder that fails at rung 5 must
# still not leave an image on the node.
sweep() {
	if [ -n "$CLEANUP_DONE" ] || [ -z "$DAEMON_ANSWERED" ]; then
		return 0
	fi
	echo "docker-proof: rung 6 never ran — sweeping best-effort so a failed proof leaks nothing"
	if [ -n "$CONTAINER_ID" ]; then
		echo "  DELETE /containers/$CONTAINER_NAME -> $(api "$HTTP_TIMEOUT_SECS" DELETE "/containers/$CONTAINER_ID?force=1&v=1")"
	fi
	echo "  DELETE /images/$TAG -> $(api "$HTTP_TIMEOUT_SECS" DELETE "/images/$TAG?force=1")"
	if [ -n "$PULLED_BASE" ]; then
		echo "  DELETE /images/$PULL_FALLBACK -> $(api "$HTTP_TIMEOUT_SECS" DELETE "/images/$PULL_FALLBACK")"
	fi
	return 0
}

# One request, its bound named by the caller. Every call lands its body and its
# transport error in a file so a rung can quote both, and reports the HTTP
# status rather than an exit code: 000 means curl never got an answer, which no
# rung may read as a refusal.
api() { # api <secs> <method> <path> [curl-args...]
	_secs="$1"
	_method="$2"
	_path="$3"
	shift 3
	_code=$(curl --silent --show-error --max-time "$_secs" \
		--unix-socket "$SOCKET" --request "$_method" \
		--output "$BODY_FILE" --write-out '%{http_code}' \
		"$@" "http://localhost$_path" 2>"$ERR_FILE") || _code=000
	[ -n "$_code" ] || _code=000
	printf '%s' "$_code"
}

api_detail() {
	if [ -s "$BODY_FILE" ]; then
		head -n 5 "$BODY_FILE"
	else
		head -n 5 "$ERR_FILE"
	fi
}

json_string() { # json_string <jq-path>
	jq -r "$1 // empty" <"$BODY_FILE" 2>/dev/null || :
}

rung_1_socket_is_bound() {
	[ -e "$SOCKET" ] || rung_fail 1 "$SOCKET is absent, so this launch was granted no socket.
      THE PREREQUISITE IS A DEPLOY, NOT A MERGE, and this is what it looks like
      unmet: either the node's daemon predates design #517 S3 (job #522) — the
      fleet ran 0.1.0+8da61424 when this job type was written — or its
      WORKER_DOCKER_SOCKET/WORKER_DOCKER_GRANTS do not name
      ${JOB_PROJECT:-<unset>}:${JOB_TYPE:-<unset>} (docs/reference/runbooks/worker-docker-grant.md).
      The node's own docker daemon is untouched by this and is not the suspect."
	[ -S "$SOCKET" ] || rung_fail 1 "$SOCKET exists and is NOT a socket ($(ls -ld "$SOCKET")).
      A regular file here boots the daemon and then hands every granted launch a
      bind no client can dial — the shape build-worker.sh refuses in advance."
	[ -w "$SOCKET" ] || rung_fail 1 "$SOCKET is a socket but is not writable by this container.
      A client cannot connect through a read-only bind, which is why #517 S3
      binds it writable (crates/container/src/docker.rs)."
	ls -l "$SOCKET"
	rung_pass 1 "$SOCKET is a writable socket, at the fixed path a client with no DOCKER_HOST looks for"
}

rung_2_no_docker_host() {
	# Asserted as ABSENT, not as empty: an exported DOCKER_HOST= would send a
	# client somewhere else while reading as unset to a `[ -z ]` test.
	[ -z "${DOCKER_HOST+set}" ] || rung_fail 2 "DOCKER_HOST is set (to '${DOCKER_HOST:-<empty>}').
      The grant injects none by design (#517 S3) so the launch env stays free of
      a name that reads as a promise on every launch that gets nothing — so
      something else put it there, and a client would dial it instead of $SOCKET."
	rung_pass 2 "DOCKER_HOST is absent, so a client finds $SOCKET by convention"
}

rung_3_daemon_answers() {
	for _tool in curl jq tar; do
		command -v "$_tool" >/dev/null 2>&1 ||
			rung_unreachable 3 "$_tool is not on this image's PATH, so nothing above rung 2 can be judged from this container. The Engine API is ordinary HTTP over a unix socket and needs no docker CLI, but it does need an HTTP client, a JSON parser and something to pack a build context"
	done

	_code=$(api "$HTTP_TIMEOUT_SECS" GET /_ping)
	case "$_code" in
	2*) ;;
	000) rung_fail 3 "nothing answered on $SOCKET: $(api_detail)
      The bind is there and the daemon behind it is not — on a node whose socket
      moved, the path is stale rather than absent." ;;
	*) rung_fail 3 "GET /_ping answered HTTP $_code: $(api_detail)" ;;
	esac
	DAEMON_ANSWERED=1

	_code=$(api "$HTTP_TIMEOUT_SECS" GET /version)
	case "$_code" in
	2*) ;;
	*) rung_fail 3 "GET /version answered HTTP $_code: $(api_detail)" ;;
	esac
	_server=$(json_string .Version)
	_api_version=$(json_string .ApiVersion)

	_code=$(api "$HTTP_TIMEOUT_SECS" GET /info)
	case "$_code" in
	2*) ;;
	*) rung_fail 3 "GET /info answered HTTP $_code: $(api_detail)" ;;
	esac
	echo "docker-proof: daemon $(json_string .Name) — $(json_string .OperatingSystem), $(json_string .Images) images, $(json_string .Containers) containers"
	rung_pass 3 "the daemon answered /_ping, /version and /info: Server ${_server:-<unreported>}, API ${_api_version:-<unreported>}"
}

# Prefer an image the node already has; a pull is announced, bounded, and
# remembered so rung 6 removes what this run added and nothing else.
rung_4_base_image() {
	for _candidate in $BASE_CANDIDATES; do
		if [ "$(api "$HTTP_TIMEOUT_SECS" GET "/images/$_candidate/json")" = 200 ]; then
			BASE_IMAGE="$_candidate"
			echo "docker-proof: base $BASE_IMAGE is already on this node — nothing is pulled"
			return 0
		fi
	done
	echo "!!! docker-proof: none of [$BASE_CANDIDATES] is on this node, so the build must PULL"
	echo "!!!     $PULL_FALLBACK, bounded at ${PULL_TIMEOUT_SECS}s. A proof that fails here"
	echo "!!!     measured the registry, not the grant — read a rung-4 failure that way."
	_code=$(api "$PULL_TIMEOUT_SECS" POST "/images/create?fromImage=$PULL_FALLBACK")
	case "$_code" in
	2*) ;;
	*) rung_fail 4 "pulling $PULL_FALLBACK answered HTTP $_code: $(api_detail)" ;;
	esac
	BASE_IMAGE="$PULL_FALLBACK"
	PULLED_BASE=1
}

rung_4_build() {
	rung_4_base_image
	mkdir -p "$WORK_DIR/context"
	cat >"$WORK_DIR/context/Dockerfile" <<EOF
FROM $BASE_IMAGE
RUN printf '%s\n' '$MARKER' > /chug-docker-proof.txt
CMD ["/bin/sh", "-c", "cat /chug-docker-proof.txt"]
EOF
	cat "$WORK_DIR/context/Dockerfile"
	tar -cf "$WORK_DIR/context.tar" -C "$WORK_DIR/context" .

	_code=$(api "$BUILD_TIMEOUT_SECS" POST "/build?t=$TAG&dockerfile=Dockerfile&rm=1&forcerm=1" \
		--header 'Content-Type: application/x-tar' \
		--data-binary "@$WORK_DIR/context.tar")
	case "$_code" in
	2*) ;;
	000) rung_fail 4 "the build never got an answer within its ${BUILD_TIMEOUT_SECS}s bound: $(api_detail)" ;;
	*) rung_fail 4 "POST /build answered HTTP $_code: $(api_detail)" ;;
	esac
	tail -n 20 "$BODY_FILE"
	# The API answers 200 and reports a failed build INSIDE the stream, so the
	# status is evidence and the artifact is the verdict.
	if grep -qF -e '"errorDetail"' "$BODY_FILE"; then
		rung_fail 4 "the build stream reported an error though the request succeeded: $(grep -F -e '"errorDetail"' "$BODY_FILE" | head -n 3)"
	fi
	_code=$(api "$HTTP_TIMEOUT_SECS" GET "/images/$TAG/json")
	[ "$_code" = 200 ] ||
		rung_fail 4 "the build stream carried no error and $TAG does not exist (GET /images/$TAG/json answered HTTP $_code): $(api_detail)"
	if [ -n "$PULLED_BASE" ]; then
		_provenance="which this run pulled"
	else
		_provenance="which the node already had, so nothing was pulled"
	fi
	rung_pass 4 "built $TAG ($(json_string .Id)) FROM $BASE_IMAGE, $_provenance"
}

# create/start/wait/logs rather than the API's own AutoRemove, which is what
# `docker run --rm` sets: it would delete the container before its logs could be
# read, and removing it is rung 6's business and gets PROVED there.
rung_5_run() {
	_code=$(api "$HTTP_TIMEOUT_SECS" POST "/containers/create?name=$CONTAINER_NAME" \
		--header 'Content-Type: application/json' \
		--data "{\"Image\":\"$TAG\"}")
	case "$_code" in
	2*) ;;
	*) rung_fail 5 "creating a container from $TAG answered HTTP $_code: $(api_detail)" ;;
	esac
	CONTAINER_ID=$(json_string .Id)
	[ -n "$CONTAINER_ID" ] || rung_fail 5 "the daemon created a container and named no Id: $(api_detail)"

	_code=$(api "$HTTP_TIMEOUT_SECS" POST "/containers/$CONTAINER_ID/start")
	case "$_code" in
	2*) ;;
	*) rung_fail 5 "starting $CONTAINER_NAME answered HTTP $_code: $(api_detail)" ;;
	esac

	# /wait blocks until the container exits, so its own bound IS the run's.
	_code=$(api "$RUN_TIMEOUT_SECS" POST "/containers/$CONTAINER_ID/wait")
	case "$_code" in
	2*) ;;
	000) rung_fail 5 "$CONTAINER_NAME had not exited within its ${RUN_TIMEOUT_SECS}s bound: $(api_detail)" ;;
	*) rung_fail 5 "waiting on $CONTAINER_NAME answered HTTP $_code: $(api_detail)" ;;
	esac
	_status=$(json_string .StatusCode)
	[ "$_status" = 0 ] || rung_fail 5 "$CONTAINER_NAME exited ${_status:-<unreported>}, not 0"

	_code=$(api "$HTTP_TIMEOUT_SECS" GET "/containers/$CONTAINER_ID/logs?stdout=1&stderr=1")
	case "$_code" in
	2*) ;;
	*) rung_fail 5 "reading $CONTAINER_NAME's logs answered HTTP $_code: $(api_detail)" ;;
	esac
	# The log stream is frame-multiplexed, so the marker is read out of the
	# bytes rather than parsed — an exit code alone would let a container that
	# printed nothing pass this rung.
	grep -qF -e "$MARKER" "$BODY_FILE" ||
		rung_fail 5 "$CONTAINER_NAME exited 0 without printing the marker this build baked in ($MARKER): $(api_detail)"
	rung_pass 5 "$CONTAINER_NAME ran and printed $MARKER, so the image rung 4 built is the one that ran"
}

rung_6_cleanup() {
	# The container first: an image still carrying one cannot be removed.
	_code=$(api "$HTTP_TIMEOUT_SECS" DELETE "/containers/$CONTAINER_ID?force=1&v=1")
	case "$_code" in
	2*) ;;
	*) rung_fail 6 "removing $CONTAINER_NAME answered HTTP $_code: $(api_detail)" ;;
	esac
	_code=$(api "$HTTP_TIMEOUT_SECS" DELETE "/images/$TAG")
	case "$_code" in
	2*) ;;
	*) rung_fail 6 "removing $TAG answered HTTP $_code: $(api_detail)" ;;
	esac
	if [ -n "$PULLED_BASE" ]; then
		echo "docker-proof: removing $PULL_FALLBACK, which this run pulled -> $(api "$HTTP_TIMEOUT_SECS" DELETE "/images/$PULL_FALLBACK")"
	fi
	CLEANUP_DONE=1
	rung_6_prove_it
}

# Removal is the act; this is the proof. A daemon that answered 200 to a DELETE
# and kept the image is exactly the failure a proof exists to catch.
rung_6_prove_it() {
	_code=$(api "$HTTP_TIMEOUT_SECS" GET /images/json)
	case "$_code" in
	2*) ;;
	*) rung_fail 6 "could not re-list images to prove the cleanup (HTTP $_code): $(api_detail)" ;;
	esac
	_images_left=$(jq -r '.[].RepoTags // [] | .[]' <"$BODY_FILE" 2>/dev/null | grep -F -e "$IMAGE_REPO" || :)

	_code=$(api "$HTTP_TIMEOUT_SECS" GET "/containers/json?all=true")
	case "$_code" in
	2*) ;;
	*) rung_fail 6 "could not re-list containers to prove the cleanup (HTTP $_code): $(api_detail)" ;;
	esac
	_containers_left=$(jq -r '.[].Names // [] | .[]' <"$BODY_FILE" 2>/dev/null | grep -F -e "$IMAGE_REPO" || :)

	[ -z "$_images_left" ] && [ -z "$_containers_left" ] ||
		rung_fail 6 "the removals answered 2xx and the node still holds:
      images: ${_images_left:-<none>}
      containers: ${_containers_left:-<none>}
      A proof that leaks onto a node is worse than no proof — remove these by
      hand before releasing this job type again."
	echo "docker-proof: no $IMAGE_REPO image and no $IMAGE_REPO container is left on this node"
	echo "docker-proof: the daemon's own build cache is NOT pruned here — it is the daemon's, shared, and already pruned on the node (#517 B3)"
	rung_pass 6 "$TAG and $CONTAINER_NAME are gone, and the re-list proves it"
}

report_identity
WORK_DIR="$(mktemp -d)"
BODY_FILE="$WORK_DIR/body"
ERR_FILE="$WORK_DIR/err"

rung_1_socket_is_bound
rung_2_no_docker_host
rung_3_daemon_answers
rung_4_build
rung_5_run
rung_6_cleanup
VERDICT="PASS — every rung cleared; the grant works end to end and the node is as it was found"
