#!/bin/sh
# Shell test for docker-proof.sh and docker-proof-identity.sh — no daemon, no
# grant, no node, no build.
#
# It drives the real ladder against a stubbed `curl` on a controlled PATH, over
# a REAL unix socket file (bound and abandoned, so `[ -S ]` answers the way it
# will on the node) and with every bound turned down to seconds. What it pins is
# the class of bug a proof script is uniquely bad at revealing — the one that
# reports PASS:
#
#   1. THE VERDICT DEFAULTS TO FAIL, and the LADDER PRINTS LAST on the failing
#      path as much as the passing one: a worker keeps only the final 700 KiB of
#      a task's logs and the ladder IS the deliverable.
#   2. RUNG 1 DISTINGUISHES ITS TWO CAUSES. An absent socket is a grant that
#      never arrived — a deploy, not a merge — and a socket that is a regular
#      file is the bind no client can dial. Neither may read as a broken daemon.
#   3. THE BUILD'S STATUS IS NOT ITS VERDICT. The Engine API answers 200 and
#      reports a failed build inside the stream, so an `errorDetail` and a
#      missing image are both rung-4 failures (the coverage.test.sh lesson: a
#      stub that only logs lets a script claim work it never did).
#   4. RUNG 5 PROVES IT RAN *THIS* BUILD. The marker travels: the ladder bakes it
#      into the Dockerfile, the stub reads it back out of the build context it
#      was posted, and the container's log must carry that same string.
#   5. CLEANUP IS PROVED, NOT ASSUMED. A daemon that answers 200 to a DELETE and
#      keeps the image is a rung-6 FAILURE, and a ladder that dies at rung 5
#      still sweeps what it created.
#   6. IT DIALS THE SOCKET, NOT A HOST, and never shells out to a `docker` CLI —
#      neither agent image carries one.
#   7. THE IDENTITY REPORT MEASURES AND PASSES. Whether an evaluator should
#      receive the socket is a design question for #517; this evaluator reports
#      a socket as a FINDING and exits 0 either way.
#
# Run:  .chug/tasks/docker-proof.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
LADDER="$HERE/docker-proof.sh"
IDENTITY="$HERE/docker-proof-identity.sh"

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

BIN="$SANDBOX/bin"
STATE="$SANDBOX/state"
CMD_LOG="$SANDBOX/cmd.log"
SOCKET="$SANDBOX/docker.sock"
mkdir -p "$BIN" "$STATE"

RUN_ID=777
TASK_ID=task-1
TAG="chug-docker-proof:$RUN_ID"
CONTAINER_NAME="chug-docker-proof-$RUN_ID"
MARKER="docker-proof-ok-$RUN_ID-$TASK_ID"
BASE=chuggernaut/agent:prod
PULL_FALLBACK=alpine:3.20

BAD=0
verdict() {
	if [ "$2" = "ok" ]; then
		echo "  ok   $1"
	else
		echo "  BAD  $1"
		BAD=$((BAD + 1))
	fi
}
yes_no() { if [ "$1" = 0 ]; then echo ok; else echo no; fi }
non_zero() { if [ "$1" -ne 0 ]; then echo ok; else echo no; fi }
found() { grep -qF -e "$2" "$1" 2>/dev/null && echo ok || echo no; }
missing() { grep -qF -e "$2" "$1" 2>/dev/null && echo no || echo ok; }
line_of() { grep -nF -e "$2" "$1" 2>/dev/null | head -n 1 | cut -d: -f1; }
before() { # before <log> <earlier> <later>
	_a="$(line_of "$1" "$2")"
	_b="$(line_of "$1" "$3")"
	[ -n "$_a" ] && [ -n "$_b" ] && [ "$_a" -lt "$_b" ] && echo ok || echo no
}
# The same question where the two calls differ only in their URL's TAIL — a
# DELETE of the image against the GET that inspected it — which a fixed-string
# search cannot tell apart.
before_re() { # before_re <log> <earlier-ere> <later-ere>
	_a="$(grep -nE -e "$2" "$1" 2>/dev/null | head -n 1 | cut -d: -f1)"
	_b="$(grep -nE -e "$3" "$1" 2>/dev/null | head -n 1 | cut -d: -f1)"
	[ -n "$_a" ] && [ -n "$_b" ] && [ "$_a" -lt "$_b" ] && echo ok || echo no
}

# --- a real socket ------------------------------------------------------------
# `[ -S ]` is rung 1's whole question, so the happy path needs a genuine AF_UNIX
# file rather than something that merely exists. Binding one and letting the
# process exit leaves the file on disk, which is all the ladder inspects; the
# stubbed curl never dials it. Two spellings because the gate's Debian container
# and an operator's laptop do not carry the same interpreters, and a suite that
# silently skipped this would be measuring nothing.
make_socket() { # make_socket <path>
	rm -f "$1"
	if command -v python3 >/dev/null 2>&1; then
		python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$1"
	elif command -v perl >/dev/null 2>&1; then
		perl -e 'use IO::Socket::UNIX; IO::Socket::UNIX->new(Local=>$ARGV[0], Listen=>1) or die $!;' "$1"
	else
		echo "docker-proof.test.sh: neither python3 nor perl is here, so no unix socket can be bound" >&2
		return 1
	fi
	[ -S "$1" ]
}

# --- the curl stub ------------------------------------------------------------
# It answers the Engine API, not a generic URL: every call appends its argv to
# one log, the body goes to `--output` and the status to stdout for
# `--write-out`, and outcomes are steered by marker files under $STATE. It keeps
# state of its own — an image the build created, a container the create created
# — so rung 6's re-listing is answered by what the earlier rungs actually did
# rather than by a constant. A transport failure is an exit 6 with nothing on
# stdout, exactly as a real curl reports one.
cat >"$BIN/curl" <<EOF
#!/bin/sh
echo "curl \$*" >> "$CMD_LOG"
out=/dev/null
url=
method=GET
ctx=
prev=
for a in "\$@"; do
  case "\$prev" in
  --output) out="\$a" ;;
  --request) method="\$a" ;;
  esac
  case "\$a" in
  http://*) [ -z "\$url" ] && url="\$a" ;;
  @*) ctx="\${a#@}" ;;
  esac
  prev="\$a"
done
path="\${url#http://localhost}"
answer() { printf '%s' "\$2" > "\$out"; printf '%s' "\$1"; exit 0; }
unreachable() { echo "curl: (7) Failed to connect to localhost port 80" >&2; exit 7; }

[ -f "$STATE/daemon_down" ] && unreachable

case "\$method \$path" in
"GET /_ping") answer 200 'OK' ;;
"GET /version") answer 200 '{"Version":"29.5.2","ApiVersion":"1.48"}' ;;
"GET /info") answer 200 '{"Name":"gumbo-nuc-0","OperatingSystem":"NixOS","Images":12,"Containers":3}' ;;
"GET /images/json")
  if [ -f "$STATE/image_built" ]; then
    answer 200 '[{"RepoTags":["$BASE"]},{"RepoTags":["$TAG"]}]'
  fi
  answer 200 '[{"RepoTags":["$BASE"]}]' ;;
"GET /containers/json?all=true")
  if [ -f "$STATE/container_created" ]; then
    answer 200 '[{"Names":["/$CONTAINER_NAME"]}]'
  fi
  answer 200 '[]' ;;
"GET /images/$TAG/json")
  [ -f "$STATE/image_built" ] && answer 200 '{"Id":"sha256:stubimage"}'
  answer 404 '{"message":"No such image"}' ;;
"GET /images/$PULL_FALLBACK/json") answer 404 '{"message":"No such image"}' ;;
"GET /images/"*"/json")
  [ -f "$STATE/base_absent" ] && answer 404 '{"message":"No such image"}'
  answer 200 '{"Id":"sha256:stubbase"}' ;;
"POST /images/create"*)
  : > "$STATE/pulled"
  answer 200 '{"status":"Downloaded newer image"}' ;;
"POST /build"*)
  [ -n "\$ctx" ] && tar -xOf "\$ctx" ./Dockerfile > "$STATE/dockerfile" 2>/dev/null
  [ -f "$STATE/build_refused" ] && answer 500 '{"message":"no such base image"}'
  [ -f "$STATE/build_stream_error" ] && answer 200 '{"stream":"Step 1/3"}
{"errorDetail":{"message":"returned a non-zero code: 1"},"error":"returned a non-zero code: 1"}'
  [ -f "$STATE/build_no_image" ] || : > "$STATE/image_built"
  answer 200 '{"stream":"Step 1/3 : FROM base"}
{"stream":"Successfully tagged $TAG"}' ;;
"POST /containers/create"*)
  : > "$STATE/container_created"
  answer 201 '{"Id":"stubcontainerid","Warnings":[]}' ;;
"POST /containers/"*"/start") answer 204 '' ;;
"POST /containers/"*"/wait")
  [ -f "$STATE/run_hangs" ] && unreachable
  [ -f "$STATE/run_fails" ] && answer 200 '{"StatusCode":2}'
  answer 200 '{"StatusCode":0}' ;;
"GET /containers/"*"/logs"*)
  [ -f "$STATE/no_marker" ] && answer 200 'some other container output'
  answer 200 "\$(grep -o 'docker-proof-ok-[A-Za-z0-9_.-]*' "$STATE/dockerfile" 2>/dev/null)" ;;
"DELETE /containers/"*)
  rm -f "$STATE/container_created"
  answer 204 '' ;;
"DELETE /images/"*)
  [ -f "$STATE/delete_lies" ] && answer 200 '[{"Untagged":"$TAG"}]'
  rm -f "$STATE/image_built"
  answer 200 '[{"Untagged":"$TAG"},{"Deleted":"sha256:stubimage"}]' ;;
esac
echo "curl: stub reached no case for '\$method \$path'" >&2
exit 7
EOF
chmod +x "$BIN/curl"

# Never invoked, and case 1 asserts that: neither agent image carries a docker
# CLI, so a ladder that reached for one would pass here and fail on the node.
cat >"$BIN/docker" <<EOF
#!/bin/sh
echo "docker \$*" >> "$CMD_LOG"
exit 0
EOF
chmod +x "$BIN/docker"

# A PATH carrying what rungs 1-2 and the summary need and NOTHING ELSE, so a
# case can withhold `curl`, `jq` or `tar` without withdrawing `sed` with them.
# The real /usr/bin holds all three, which is why "no curl" cannot be spelled by
# dropping $BIN from the PATH.
MINIMAL="$SANDBOX/minimal"
mkdir -p "$MINIMAL"
for tool in sh sed head cat mktemp rm ls grep mkdir printf; do
	if real=$(command -v "$tool" 2>/dev/null); then
		ln -sf "$real" "$MINIMAL/$tool"
	fi
done

reset_case() {
	rm -f "$STATE"/* "$CMD_LOG"
	make_socket "$SOCKET"
}

run_ladder() { # run_ladder <outfile> [PATH override] [extra env...]
	STATUS=0
	env -i \
		PATH="${2-$BIN:/usr/bin:/bin}" \
		HOME="$SANDBOX" \
		JOB_ID="$RUN_ID" \
		JOB_PROJECT=kasofsk/chuggernaut \
		JOB_TYPE=docker-proof \
		CHUG_PHASE=Work \
		CHUG_TASK_ID="$TASK_ID" \
		CHUG_DOCKER_SOCKET="${SOCKET_OVERRIDE:-$SOCKET}" \
		CHUG_DOCKER_HTTP_TIMEOUT_SECS=5 \
		CHUG_DOCKER_BUILD_TIMEOUT_SECS=5 \
		CHUG_DOCKER_RUN_TIMEOUT_SECS=5 \
		CHUG_DOCKER_PULL_TIMEOUT_SECS=5 \
		sh "$LADDER" >"$1" 2>&1 || STATUS=$?
}

echo "docker-proof.test.sh: driving the real ladder against a stub daemon"

# --- case 1: the whole ladder --------------------------------------------------
echo "case 1: a granted container clears all six rungs and leaves nothing behind"
reset_case
SOCKET_OVERRIDE=
run_ladder "$SANDBOX/out1.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "rung 1 passes on the bound socket" "$(found "$SANDBOX/out1.txt" "rung 1 the socket is bound: PASS")"
verdict "rung 2 passes on the absent DOCKER_HOST" "$(found "$SANDBOX/out1.txt" "rung 2 no DOCKER_HOST is set: PASS")"
verdict "rung 3 passes on the daemon" "$(found "$SANDBOX/out1.txt" "rung 3 the daemon answers: PASS")"
verdict "rung 4 passes on the build" "$(found "$SANDBOX/out1.txt" "rung 4 an image builds: PASS")"
verdict "rung 5 passes on the run" "$(found "$SANDBOX/out1.txt" "rung 5 the built image runs: PASS")"
verdict "rung 6 passes on the cleanup" "$(found "$SANDBOX/out1.txt" "rung 6 cleanup, and prove it: PASS")"
verdict "nothing reads NOT REACHED" "$(missing "$SANDBOX/out1.txt" ": NOT REACHED")"
verdict "prints the identity it runs under" "$(found "$SANDBOX/out1.txt" "JOB_PROJECT=kasofsk/chuggernaut JOB_TYPE=docker-proof")"
verdict "dials the socket, never a host" "$(found "$CMD_LOG" "--unix-socket $SOCKET")"
verdict "shells out to no docker CLI" "$(missing "$CMD_LOG" "docker version")"
verdict "pulls nothing when the base is already there" "$(missing "$CMD_LOG" "/images/create")"
verdict "says so in as many words" "$(found "$SANDBOX/out1.txt" "is already on this node — nothing is pulled")"
verdict "builds before it runs" "$(before "$CMD_LOG" "/build?t=$TAG" "/containers/create")"
verdict "removes the container before the image" \
	"$(before_re "$CMD_LOG" "/containers/stubcontainerid\?force=1" "/images/$TAG\$")"
verdict "re-lists the images to prove the cleanup" "$(before "$CMD_LOG" "DELETE" "/images/json")"
verdict "the VERDICT is the very last line" \
	"$(tail -n 1 "$SANDBOX/out1.txt" | grep -q 'VERDICT PASS' && echo ok || echo no)"

# --- case 2: no socket at all ---------------------------------------------------
echo "case 2: an ungranted container fails rung 1 and blames the deploy, not the daemon"
reset_case
SOCKET_OVERRIDE="$SANDBOX/absent.sock"
run_ladder "$SANDBOX/out2.txt"
SOCKET_OVERRIDE=

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 1" "$(found "$SANDBOX/out2.txt" "RUNG 1 FAILED")"
verdict "says the prerequisite is a deploy" "$(found "$SANDBOX/out2.txt" "THE PREREQUISITE IS A DEPLOY, NOT A MERGE")"
verdict "names the daemon slice it needs" "$(found "$SANDBOX/out2.txt" "job #522")"
verdict "names the node config it needs" "$(found "$SANDBOX/out2.txt" "WORKER_DOCKER_SOCKET/WORKER_DOCKER_GRANTS")"
verdict "points at the runbook" "$(found "$SANDBOX/out2.txt" "docs/reference/runbooks/worker-docker-grant.md")"
verdict "clears the node's own daemon of suspicion" "$(found "$SANDBOX/out2.txt" "is not the suspect")"
verdict "rung 4 reads NOT REACHED" "$(found "$SANDBOX/out2.txt" "rung 4 an image builds: NOT REACHED")"
verdict "the failed rung is not ALSO NOT REACHED" "$(missing "$SANDBOX/out2.txt" "rung 1 the socket is bound: NOT REACHED")"
verdict "spends no HTTP call at all" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"
verdict "still prints the ladder last" \
	"$(tail -n 1 "$SANDBOX/out2.txt" | grep -q 'VERDICT FAIL at rung 1' && echo ok || echo no)"

# --- case 3: a bind that is not a socket ------------------------------------------
echo "case 3: a regular file at the socket path is its own rung-1 failure"
reset_case
SOCKET_OVERRIDE="$SANDBOX/regular-file"
: >"$SOCKET_OVERRIDE"
run_ladder "$SANDBOX/out3.txt"
SOCKET_OVERRIDE=

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 1" "$(found "$SANDBOX/out3.txt" "RUNG 1 FAILED")"
verdict "says it is not a socket" "$(found "$SANDBOX/out3.txt" "exists and is NOT a socket")"
verdict "names the bind nobody can dial" "$(found "$SANDBOX/out3.txt" "no client can dial")"
verdict "does not read as the absent case" "$(missing "$SANDBOX/out3.txt" "granted no socket")"

# --- case 4: a DOCKER_HOST nobody promised -----------------------------------------
echo "case 4: a DOCKER_HOST in the launch env fails rung 2, even when empty"
reset_case
STATUS=0
env -i PATH="$BIN:/usr/bin:/bin" HOME="$SANDBOX" JOB_ID="$RUN_ID" \
	JOB_PROJECT=kasofsk/chuggernaut JOB_TYPE=docker-proof CHUG_TASK_ID="$TASK_ID" \
	CHUG_DOCKER_SOCKET="$SOCKET" DOCKER_HOST= \
	sh "$LADDER" >"$SANDBOX/out4.txt" 2>&1 || STATUS=$?

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 2" "$(found "$SANDBOX/out4.txt" "RUNG 2 FAILED")"
verdict "rung 1 still passes" "$(found "$SANDBOX/out4.txt" "rung 1 the socket is bound: PASS")"
verdict "says the grant injects none" "$(found "$SANDBOX/out4.txt" "The grant injects none by design")"
verdict "spends no HTTP call" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

# --- case 5: the socket is there and the daemon is not ------------------------------
echo "case 5: a bound socket nothing answers on is a rung-3 failure"
reset_case
: >"$STATE/daemon_down"
run_ladder "$SANDBOX/out5.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out5.txt" "RUNG 3 FAILED")"
verdict "says nothing answered" "$(found "$SANDBOX/out5.txt" "nothing answered on $SOCKET")"
verdict "quotes the transport error" "$(found "$SANDBOX/out5.txt" "Failed to connect")"
verdict "builds nothing" "$(missing "$CMD_LOG" "/build")"
verdict "sweeps nothing, having created nothing" "$(missing "$SANDBOX/out5.txt" "sweeping best-effort")"

# --- case 6: the build reports an error inside a 200 ---------------------------------
echo "case 6: an errorDetail in the build stream is a rung-4 FAILURE, not a 200"
reset_case
: >"$STATE/build_stream_error"
run_ladder "$SANDBOX/out6.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 4" "$(found "$SANDBOX/out6.txt" "RUNG 4 FAILED")"
verdict "quotes the stream's own error" "$(found "$SANDBOX/out6.txt" "returned a non-zero code")"
verdict "creates no container" "$(missing "$CMD_LOG" "/containers/create")"
verdict "verdict is not PASS" "$(missing "$SANDBOX/out6.txt" "VERDICT PASS")"

echo "case 6b: a build that reports nothing and produces no image still fails rung 4"
reset_case
: >"$STATE/build_no_image"
run_ladder "$SANDBOX/out6b.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 4" "$(found "$SANDBOX/out6b.txt" "RUNG 4 FAILED")"
verdict "names the absent image" "$(found "$SANDBOX/out6b.txt" "does not exist")"

# --- case 7: the image runs and proves nothing ------------------------------------------
echo "case 7: a container that exits 0 without the marker is a rung-5 failure"
reset_case
: >"$STATE/no_marker"
run_ladder "$SANDBOX/out7.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 5" "$(found "$SANDBOX/out7.txt" "RUNG 5 FAILED")"
verdict "names the marker it baked in" "$(found "$SANDBOX/out7.txt" "$MARKER")"
verdict "sweeps the image it created" "$(found "$SANDBOX/out7.txt" "sweeping best-effort")"
verdict "sweeps the container too" "$(found "$SANDBOX/out7.txt" "DELETE /containers/$CONTAINER_NAME")"
verdict "rung 6 reads NOT REACHED" "$(found "$SANDBOX/out7.txt" "rung 6 cleanup, and prove it: NOT REACHED")"

echo "case 7b: a non-zero exit inside the container is a rung-5 failure"
reset_case
: >"$STATE/run_fails"
run_ladder "$SANDBOX/out7b.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 5" "$(found "$SANDBOX/out7b.txt" "RUNG 5 FAILED")"
verdict "names the exit code" "$(found "$SANDBOX/out7b.txt" "exited 2, not 0")"

# --- case 8: the marker really travels ----------------------------------------------------
echo "case 8: rung 5 reads back the marker rung 4 baked into the build context"
reset_case
run_ladder "$SANDBOX/out8.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "the Dockerfile it posted carries the marker" "$(found "$STATE/dockerfile" "$MARKER")"
verdict "it builds FROM a base the node already has" "$(found "$STATE/dockerfile" "FROM $BASE")"
verdict "rung 5 names the marker it found" "$(found "$SANDBOX/out8.txt" "printed $MARKER")"

# --- case 9: the cleanup lies ---------------------------------------------------------------
echo "case 9: a DELETE that answers 200 and keeps the image is a rung-6 FAILURE"
reset_case
: >"$STATE/delete_lies"
run_ladder "$SANDBOX/out9.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 6" "$(found "$SANDBOX/out9.txt" "RUNG 6 FAILED")"
verdict "lists what is left on the node" "$(found "$SANDBOX/out9.txt" "images: $TAG")"
verdict "says a leak is worse than no proof" "$(found "$SANDBOX/out9.txt" "worse than no proof")"
verdict "rung 5 still reads PASS" "$(found "$SANDBOX/out9.txt" "rung 5 the built image runs: PASS")"
verdict "verdict is not PASS" "$(missing "$SANDBOX/out9.txt" "VERDICT PASS")"

# --- case 10: a node without the base image ----------------------------------------------------
echo "case 10: with no base on the node the pull is announced, bounded, and removed again"
reset_case
: >"$STATE/base_absent"
run_ladder "$SANDBOX/out10.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "shouts that it must pull" "$(found "$SANDBOX/out10.txt" "so the build must PULL")"
verdict "says a failure there measured the registry" "$(found "$SANDBOX/out10.txt" "measured the registry")"
verdict "pulls the fallback" "$(found "$CMD_LOG" "/images/create?fromImage=$PULL_FALLBACK")"
verdict "bounds the pull" "$(found "$CMD_LOG" "--max-time 5")"
verdict "removes what it pulled" "$(found "$SANDBOX/out10.txt" "removing $PULL_FALLBACK, which this run pulled")"
verdict "rung 6 still passes" "$(found "$SANDBOX/out10.txt" "rung 6 cleanup, and prove it: PASS")"

# --- case 11: the image is wrong ------------------------------------------------------------------
echo "case 11: an image with no curl reports NOT REACHED, and still fails"
reset_case
run_ladder "$SANDBOX/out11.txt" "$MINIMAL"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "rung 3 reads NOT REACHED" "$(found "$SANDBOX/out11.txt" "rung 3 the daemon answers: NOT REACHED")"
verdict "names curl" "$(found "$SANDBOX/out11.txt" "curl is not on this image's PATH")"
verdict "says no docker CLI is needed" "$(found "$SANDBOX/out11.txt" "needs no docker CLI")"
verdict "rungs 1 and 2 were still judged" "$(found "$SANDBOX/out11.txt" "rung 2 no DOCKER_HOST is set: PASS")"
verdict "verdict is a FAIL" "$(found "$SANDBOX/out11.txt" "VERDICT FAIL")"

# --- case 12: a dispatcher that stamps no job type ---------------------------------------------------
# The fleet ran 0.1.0+8da61424 when this was written, which predates #517 S2, so
# an absent JOB_TYPE is the first thing a released proof will meet.
echo "case 12: with JOB_TYPE unset, the ladder says the allow-list can match nothing"
reset_case
STATUS=0
env -i PATH="$BIN:/usr/bin:/bin" HOME="$SANDBOX" JOB_ID="$RUN_ID" \
	JOB_PROJECT=kasofsk/chuggernaut CHUG_TASK_ID="$TASK_ID" \
	CHUG_DOCKER_SOCKET="$SANDBOX/absent.sock" \
	sh "$LADDER" >"$SANDBOX/out12.txt" 2>&1 || STATUS=$?

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "reports JOB_TYPE unset" "$(found "$SANDBOX/out12.txt" "JOB_TYPE=<unset>")"
verdict "says admits matches nothing" "$(found "$SANDBOX/out12.txt" "matches NOTHING at all")"
verdict "names the dispatcher slice, not the allow-list" "$(found "$SANDBOX/out12.txt" "#517 S2")"

# --- the identity evaluator ----------------------------------------------------------------------------
run_identity() { # run_identity <outfile> <socket>
	ISTATUS=0
	env -i PATH="$BIN:/usr/bin:/bin" HOME="$SANDBOX" \
		JOB_PROJECT=kasofsk/chuggernaut JOB_TYPE=docker-proof \
		CHUG_PHASE=Evaluation CHUG_EVALUATOR=identity \
		CHUG_DOCKER_SOCKET="$2" \
		sh "$IDENTITY" >"$1" 2>&1 || ISTATUS=$?
}

echo "case 13: an evaluator STILL holding the socket after #543 S3 is a FINDING, reported and passed"
reset_case
run_identity "$SANDBOX/out13.txt" "$SOCKET"

verdict "exits 0" "$(yes_no "$ISTATUS")"
verdict "calls it a finding for #543 S3" "$(found "$SANDBOX/out13.txt" "FINDING for design #543 S3")"
verdict "names the level it measured" "$(found "$SANDBOX/out13.txt" "CHUG_EVALUATOR=identity")"
verdict "names the scope it contradicts" "$(found "$SANDBOX/out13.txt" "CHUG_PHASE=Work alone")"
verdict "names the likelier cause first" "$(found "$SANDBOX/out13.txt" "predates S3")"
verdict "names the appended evaluator it would reach" "$(found "$SANDBOX/out13.txt" "appended \`ci\` one")"
verdict "changes nothing on the strength of it" "$(found "$SANDBOX/out13.txt" "is changed on the strength of it")"
verdict "reports a measurement, not a verdict" "$(found "$SANDBOX/out13.txt" "MEASURED — an evaluator DOES receive the socket")"

echo "case 14: an evaluator without the socket is what S3 intends, and passes"
reset_case
run_identity "$SANDBOX/out14.txt" "$SANDBOX/absent.sock"

verdict "exits 0" "$(yes_no "$ISTATUS")"
verdict "says the grant is work-level" "$(found "$SANDBOX/out14.txt" "work-level as #543 S3 scoped it")"
verdict "raises no finding" "$(missing "$SANDBOX/out14.txt" "FINDING for design #543")"
verdict "still prints the identity" "$(found "$SANDBOX/out14.txt" "JOB_TYPE=docker-proof")"

echo
if [ "$BAD" -eq 0 ]; then
	echo "docker-proof.test.sh: all cases pass"
else
	echo "docker-proof.test.sh: $BAD check(s) FAILED"
	exit 1
fi
