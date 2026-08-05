#!/bin/sh
# Shell test for gcp-proof.sh and gcp-proof-negative.sh — no GCP, no token, no
# federation, no network.
#
# It drives the real ladders against a stubbed `curl` on a controlled PATH and
# a sandbox `/chuggernaut/cloud` tree holding a hand-built JWT. What it pins is
# the class of bug a proof script is uniquely bad at revealing — THE ONE THAT
# REPORTS PASS:
#
#   1. THE VERDICT DEFAULTS TO FAIL. Any exit that does not run off the end of
#      the script reports FAIL, so a `set -eu` abort, a missing `base64` or a
#      future edit that adds a bare command cannot be read as a passing proof.
#   2. A FAILING READ IS A FAILING RUNG. Rung 4's status must come from the HTTP
#      status, not from the last command of a pipeline — `head -1` exits 0 on
#      empty input, and `pipefail` is not POSIX.
#   3. THE NEGATIVE RUNG IS REAL. A read of the denied bucket that SUCCEEDS is a
#      finding; a 404 rather than a 403 is inconclusive — an absent object
#      refuses everyone — and a request that never got an answer refuses nobody.
#      Neither may look like a bounded credential.
#   4. A FAILURE NAMES ITS RUNG EXACTLY ONCE, and the rungs after it — never the
#      failing one — read NOT REACHED. The ladder is the deliverable, so it
#      prints on every path, last.
#   5. THE LADDER STOPS AT THE FIRST FAILURE. A rung that failed spends no
#      network call on the rungs above it.
#   6. THE EXCHANGE IS DRIVEN OVER REST, with no gcloud anywhere: rung 3 posts
#      the token to the STS, rung 3b impersonates the SA the adc.json names, and
#      a missing curl or jq is a NAMED NOT REACHED rather than a silent pass.
#   7. RUNG 5B PASSES BY FINDING NOTHING, and fails on a credential, on a stray
#      GOOGLE_APPLICATION_CREDENTIALS, and on any ambient credential — a gcloud
#      that mints, or a GCE metadata server that hands out the node's token.
#   8. RUNG 5B SAYS WHICH PASS IT IS. A metadata server nobody could reach tested
#      nothing, and its line must not read the same as one that refused to mint.
#
# Run:  .chug/tasks/gcp-proof.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
LADDER="$HERE/gcp-proof.sh"
NEGATIVE="$HERE/gcp-proof-negative.sh"

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

BIN="$SANDBOX/bin"
# The gcloud stub sits APART from the curl stub so a case can offer one without
# the other: rung 5b's ambient probe needs curl on a PATH carrying no SDK, which
# is the shape of the real agent image.
SDK="$SANDBOX/sdk"
STATE="$SANDBOX/state"
CLOUD="$SANDBOX/cloud"
CMD_LOG="$SANDBOX/cmd.log"
DIR="$CLOUD/gcp-proof"
mkdir -p "$BIN" "$SDK" "$STATE" "$DIR"

GRANTED=chug-wif-proof-granted
DENIED=chug-wif-proof-denied

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

# --- the token ----------------------------------------------------------------
# A real three-segment JWT whose payload is the claim set crates/auth's mint
# produces for this job type's work container. Only the payload is meaningful:
# rung 2 is offline by design and never checks a signature.
b64url() { base64 | tr -d '\n' | tr '+/' '-_' | tr -d '='; }

# The fourth argument overrides the `project` CLAIM alone, leaving `sub` built
# from the third: the two are assembled separately by the mint, so a token whose
# subject and claim disagree is a shape rung 2 has to be able to tell apart.
write_token() { # write_token <project> <job_type> <container> [project_claim]
	_claim_project="${4-$1}"
	_claims="{\"iss\":\"https://chug.kasofsk.xyz\",\"sub\":\"project:$1:type:$2\",\
\"project\":\"$_claim_project\",\"job_type\":\"$2\",\"container\":\"$3\",\"workload\":\"$1:$2:$3\",\
\"job_seq\":417,\"task_id\":4,\"phase\":\"Work\",\"jti\":\"8c8f0f2e-0000-4000-8000-000000000001\"}"
	printf '%s.%s.%s\n' \
		"$(printf '%s' '{"alg":"RS256","kid":"stub"}' | b64url)" \
		"$(printf '%s' "$_claims" | b64url)" \
		"c2lnbmF0dXJl" >"$DIR/token"
}

# --- the curl stub ------------------------------------------------------------
# Every call appends its argv to one log, so the ORDER of the rungs and what a
# stopped ladder never reached are both readable from a single file. It honours
# the two flags the ladder relies on — the body goes to `--output`, the status
# to stdout for `--write-out` — and steers outcomes off marker files under
# $STATE, which is what lets one stub serve every case. A transport failure is
# an exit 6 with nothing on stdout, exactly as a real curl reports one.
SA=proof-granted@daekon-ai.iam.gserviceaccount.com
STS_URL=https://sts.googleapis.com/v1/token
IMPERSONATION_URL="https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/$SA:generateAccessToken"

cat >"$BIN/curl" <<EOF
#!/bin/sh
echo "curl \$*" >> "$CMD_LOG"
out=/dev/null
url=
prev=
for a in "\$@"; do
  case "\$prev" in --output) out="\$a" ;; esac
  case "\$a" in http://*|https://*) [ -z "\$url" ] && url="\$a" ;; esac
  prev="\$a"
done
answer() { printf '%s' "\$2" > "\$out"; printf '%s' "\$1"; exit 0; }
unreachable() { echo "curl: (6) Could not resolve host" >&2; exit 6; }
case "\$url" in
*sts.googleapis.com*)
  [ -f "$STATE/sts_unreachable" ] && unreachable
  [ -f "$STATE/login_fails" ] && answer 400 '{"error":"invalid_grant","error_description":"Error connecting to the given credential'"'"'s issuer"}'
  [ -f "$STATE/sts_fails" ] && answer 403 '{"error":"permission_denied","error_description":"the attribute condition denied this token"}'
  [ -f "$STATE/sts_empty" ] && answer 200 '{"token_type":"Bearer","expires_in":3600}'
  answer 200 "{\"access_token\":\"\$(cat "$STATE/federated_token")\",\"token_type\":\"Bearer\",\"expires_in\":3600}" ;;
*iamcredentials.googleapis.com*)
  [ -f "$STATE/impersonation_fails" ] && answer 403 '{"error":{"code":403,"message":"caller does not have permission to impersonate"}}'
  answer 200 "{\"accessToken\":\"\$(cat "$STATE/access_token")\",\"expireTime\":\"2026-08-05T12:00:00Z\"}" ;;
*storage.googleapis.com*$GRANTED*)
  [ -f "$STATE/granted_denied" ] && answer 403 '{"error":{"code":403,"message":"does not have storage.objects.get access"}}'
  [ -f "$STATE/granted_empty" ] && answer 200 ''
  answer 200 'chuggernaut gcp-proof canary (granted bucket)' ;;
*storage.googleapis.com*$DENIED*)
  [ -f "$STATE/denied_unreachable" ] && unreachable
  [ -f "$STATE/denied_readable" ] && answer 200 'chuggernaut gcp-proof canary (denied bucket)'
  [ -f "$STATE/denied_absent" ] && answer 404 '{"error":{"code":404,"message":"No such object"}}'
  [ -f "$STATE/denied_unauthorized" ] && answer 401 '{"error":{"code":401,"message":"Invalid Credentials"}}'
  answer 403 '{"error":{"code":403,"message":"does not have storage.objects.get access to the object"}}' ;;
*metadata.google.internal*|*169.254.169.254*)
  [ -f "$STATE/metadata_mints" ] && answer 200 '{"access_token":"ya29.stub-node-token","expires_in":3599,"token_type":"Bearer"}'
  [ -f "$STATE/metadata_answers" ] && answer 404 'Not Found'
  unreachable ;;
esac
echo "curl: stub reached no case for '\$url'" >&2
exit 6
EOF
chmod +x "$BIN/curl"

# The only gcloud left in this suite, and it belongs to rung 5b: the negative
# evaluator still asks whether an SDK on the PATH can mint from AMBIENT
# credentials. The ladder must never call it, which case 1 asserts.
cat >"$SDK/gcloud" <<EOF
#!/bin/sh
echo "gcloud \$*" >> "$CMD_LOG"
case "\$1 \$2" in
"auth print-access-token") echo "ya29.stub-ambient-token"; exit 0 ;;
esac
exit 1
EOF
chmod +x "$SDK/gcloud"

# The document the dispatcher writes beside the token (crates/auth's
# `adc_document`). Rung 3 reads its audience, endpoints and service account out
# of this file rather than off constants, so the stub ships the real shape.
write_adc() { # write_adc [credential_source_file]
	cat >"$DIR/adc.json" <<JSON
{
  "type": "external_account",
  "audience": "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/chug/providers/chuggernaut",
  "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
  "token_url": "$STS_URL",
  "service_account_impersonation_url": "$IMPERSONATION_URL",
  "credential_source": {
    "file": "${1-$DIR/token}"
  }
}
JSON
}

reset_case() {
	rm -f "$STATE"/* "$CMD_LOG"
	rm -rf "$CLOUD"
	mkdir -p "$DIR"
	printf 'ya29.stub-federated-token' >"$STATE/federated_token"
	printf 'ya29.stub-access-token' >"$STATE/access_token"
	write_token kasofsk/chuggernaut gcp-proof work
	write_adc
	chmod 600 "$DIR/token"
	chmod 644 "$DIR/adc.json"
}

# A PATH carrying everything rungs 1-2 and the summary need and NOTHING ELSE, so
# a case can withhold `curl` or `jq` without withdrawing `base64` with them. The
# real /usr/bin holds a curl, which is exactly why "no curl" cannot be spelled by
# dropping $BIN from the PATH.
MINIMAL="$SANDBOX/minimal"
mkdir -p "$MINIMAL"
for tool in sh stat cut tr base64 sed head cat mktemp rm; do
	if real=$(command -v "$tool" 2>/dev/null); then
		ln -sf "$real" "$MINIMAL/$tool"
	fi
done

run_ladder() { # run_ladder <outfile> [PATH override]
	STATUS=0
	env -i \
		PATH="${2-$BIN:$SDK:/usr/bin:/bin}" \
		HOME="$SANDBOX" \
		CHUG_CLOUD_ROOT="$CLOUD" \
		GOOGLE_APPLICATION_CREDENTIALS="$DIR/adc.json" \
		CHUG_INPUT_GRANTED_BUCKET="$GRANTED" \
		CHUG_INPUT_DENIED_BUCKET="$DENIED" \
		sh "$LADDER" >"$1" 2>&1 || STATUS=$?
}

echo "gcp-proof.test.sh: driving the real ladder against stubs"

# --- case 1: everything works --------------------------------------------------
echo "case 1: a wired, bounded credential climbs every rung, over curl alone"
reset_case
run_ladder "$SANDBOX/out1.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "verdict is PASS" "$(found "$SANDBOX/out1.txt" "VERDICT PASS")"
verdict "rung 1 PASS" "$(found "$SANDBOX/out1.txt" "rung 1 the credential is injected: PASS")"
verdict "rung 2 PASS" "$(found "$SANDBOX/out1.txt" "rung 2 the claims are what we minted: PASS")"
verdict "rung 3 PASS" "$(found "$SANDBOX/out1.txt" "rung 3 the STS accepts the exchange: PASS")"
verdict "rung 3b PASS" "$(found "$SANDBOX/out1.txt" "rung 3b the federated token impersonates the service account: PASS")"
verdict "rung 4 PASS" "$(found "$SANDBOX/out1.txt" "rung 4 the granted read succeeds: PASS")"
verdict "rung 5 PASS" "$(found "$SANDBOX/out1.txt" "rung 5 the UNGRANTED read is refused: PASS")"
verdict "nothing reads NOT REACHED" "$(missing "$SANDBOX/out1.txt" ": NOT REACHED")"
verdict "names the evaluator as 5b's owner" "$(found "$SANDBOX/out1.txt" "rung 5b")"
verdict "posted the exchange to the STS" "$(found "$CMD_LOG" "$STS_URL")"
verdict "asked for a token exchange" "$(found "$CMD_LOG" "grant_type=urn:ietf:params:oauth:grant-type:token-exchange")"
verdict "declared the subject token type" "$(found "$CMD_LOG" "subject_token_type=urn:ietf:params:oauth:token-type:jwt")"
verdict "sent the audience the adc.json names" "$(found "$CMD_LOG" "audience=//iam.googleapis.com/projects/1/")"
verdict "sent the subject token from a file, not argv" "$(found "$CMD_LOG" "subject_token@")"
verdict "kept the JWT off the command line" "$(missing "$CMD_LOG" "subject_token=eyJ")"
verdict "impersonated the SA the adc.json names" "$(found "$CMD_LOG" "$IMPERSONATION_URL")"
verdict "read the granted bucket" "$(found "$CMD_LOG" "https://storage.googleapis.com/storage/v1/b/$GRANTED/o/canary.txt?alt=media")"
verdict "read the denied bucket" "$(found "$CMD_LOG" "https://storage.googleapis.com/storage/v1/b/$DENIED/o/canary.txt?alt=media")"
verdict "bore the SA token, not the federated one" "$(found "$CMD_LOG" "Authorization: Bearer ya29.stub-access-token")"
verdict "ran no gcloud" "$(missing "$CMD_LOG" "gcloud")"

# --- case 2: the modes drift ----------------------------------------------------
echo "case 2: a token at 0644 fails rung 1, and spends no network call"
reset_case
chmod 644 "$DIR/token"
run_ladder "$SANDBOX/out2.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 1" "$(found "$SANDBOX/out2.txt" "RUNG 1 FAILED")"
verdict "names the promised mode" "$(found "$SANDBOX/out2.txt" "the dispatcher promised 600")"
verdict "rung 1 reads FAIL" "$(found "$SANDBOX/out2.txt" "rung 1 the credential is injected: FAIL")"
verdict "rung 2 reads NOT REACHED" "$(found "$SANDBOX/out2.txt" "rung 2 the claims are what we minted: NOT REACHED")"
verdict "rung 3b reads NOT REACHED" "$(found "$SANDBOX/out2.txt" "rung 3b the federated token impersonates the service account: NOT REACHED")"
verdict "rung 5 reads NOT REACHED" "$(found "$SANDBOX/out2.txt" "rung 5 the UNGRANTED read is refused: NOT REACHED")"
verdict "the failed rung is not ALSO NOT REACHED" "$(missing "$SANDBOX/out2.txt" "rung 1 the credential is injected: NOT REACHED")"
verdict "spends no HTTP call at all" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

# --- case 3: no credential at all -------------------------------------------------
echo "case 3: with injection undeployed, rung 1 fails and names the sequence"
reset_case
rm -f "$DIR/token"
run_ladder "$SANDBOX/out3.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 1" "$(found "$SANDBOX/out3.txt" "RUNG 1 FAILED")"
verdict "points at the runbook step" "$(found "$SANDBOX/out3.txt" "infra/README.md §2 step a")"

# --- case 4: the claims are wrong ---------------------------------------------------
echo "case 4: a token minted for another project fails rung 2, offline"
reset_case
write_token kasofsk/beacon gcp-proof work
chmod 600 "$DIR/token"
run_ladder "$SANDBOX/out4.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 2" "$(found "$SANDBOX/out4.txt" "RUNG 2 FAILED")"
verdict "names the subject, which is checked first" "$(found "$SANDBOX/out4.txt" "sub is not")"
verdict "decoded the payload" "$(found "$SANDBOX/out4.txt" "kasofsk/beacon")"
verdict "spent no network call" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

# --- case 4b: sub and project disagree -------------------------------------------------
echo "case 4b: a right subject over a wrong project claim is still a rung-2 failure"
reset_case
write_token kasofsk/chuggernaut gcp-proof work kasofsk/beacon
chmod 600 "$DIR/token"
run_ladder "$SANDBOX/out4b.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names the project claim" "$(found "$SANDBOX/out4b.txt" "the project claim is not")"
verdict "names the attribute condition it would trip" "$(found "$SANDBOX/out4b.txt" "attribute condition")"

# --- case 5: an evaluator's token in the work container ------------------------------
echo "case 5: a token whose container claim is not \`work\` fails rung 2"
reset_case
write_token kasofsk/chuggernaut gcp-proof eval:no-identity
chmod 600 "$DIR/token"
run_ladder "$SANDBOX/out5.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names the container claim" "$(found "$SANDBOX/out5.txt" "the container claim is not")"

# --- case 6: the JWKS trap -----------------------------------------------------------
echo "case 6: an issuer-connection error at rung 3 blames the upload, not the issuer"
reset_case
: >"$STATE/login_fails"
run_ladder "$SANDBOX/out6.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out6.txt" "RUNG 3 FAILED")"
verdict "names the misleading error" "$(found "$SANDBOX/out6.txt" "Error connecting to the given credential's issuer")"
verdict "redirects to the JWK set" "$(found "$SANDBOX/out6.txt" "suspect")"
verdict "points at the runbook" "$(found "$SANDBOX/out6.txt" "infra/README.md §5")"
verdict "names the HTTP status" "$(found "$SANDBOX/out6.txt" "HTTP 400")"
verdict "reads no bucket" "$(missing "$CMD_LOG" "storage.googleapis.com")"
verdict "impersonates nothing" "$(missing "$CMD_LOG" "iamcredentials")"

# --- case 7: the STS refuses ----------------------------------------------------------
echo "case 7: an STS refusal stops the ladder at rung 3"
reset_case
: >"$STATE/sts_fails"
run_ladder "$SANDBOX/out7.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out7.txt" "RUNG 3 FAILED")"
verdict "quotes the STS body" "$(found "$SANDBOX/out7.txt" "the attribute condition denied this token")"
verdict "rung 3b reads NOT REACHED" "$(found "$SANDBOX/out7.txt" "rung 3b the federated token impersonates the service account: NOT REACHED")"
verdict "rung 4 reads NOT REACHED" "$(found "$SANDBOX/out7.txt" "rung 4 the granted read succeeds: NOT REACHED")"
verdict "reads no bucket" "$(missing "$CMD_LOG" "storage.googleapis.com")"

# --- case 7b: a 200 that carries no token ------------------------------------------------
echo "case 7b: an STS 200 with no access_token is a rung-3 FAILURE, not a pass"
reset_case
: >"$STATE/sts_empty"
run_ladder "$SANDBOX/out7b.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out7b.txt" "RUNG 3 FAILED")"
verdict "names the missing field" "$(found "$SANDBOX/out7b.txt" "no access_token")"
verdict "impersonates nothing" "$(missing "$CMD_LOG" "iamcredentials")"

# --- case 7c: the STS is unreachable -------------------------------------------------------
echo "case 7c: a call that never got an answer is a rung-3 FAILURE, and says so"
reset_case
: >"$STATE/sts_unreachable"
run_ladder "$SANDBOX/out7c.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out7c.txt" "RUNG 3 FAILED")"
verdict "says no response arrived" "$(found "$SANDBOX/out7c.txt" "no HTTP response")"
verdict "quotes the transport error" "$(found "$SANDBOX/out7c.txt" "Could not resolve host")"

# --- case 7d: impersonation is refused -------------------------------------------------------
# Split from rung 3 on purpose: the STS accepting the token and the SA refusing
# the impersonation are different findings, and only 3b's is about the member.
echo "case 7d: a refused impersonation fails rung 3b, and names the member first"
reset_case
: >"$STATE/impersonation_fails"
run_ladder "$SANDBOX/out7d.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3b" "$(found "$SANDBOX/out7d.txt" "RUNG 3b FAILED")"
verdict "rung 3 still reads PASS" "$(found "$SANDBOX/out7d.txt" "rung 3 the STS accepts the exchange: PASS")"
verdict "rung 3b reads FAIL" "$(found "$SANDBOX/out7d.txt" "rung 3b the federated token impersonates the service account: FAIL")"
verdict "names the principalSet member as the first suspect" "$(found "$SANDBOX/out7d.txt" "Suspect the principalSet member first")"
verdict "says the slash is literal, not encoded" "$(found "$SANDBOX/out7d.txt" "is LITERAL")"
verdict "names the ordered next suspects" "$(found "$SANDBOX/out7d.txt" "serviceAccountTokenCreator")"
verdict "names the service account" "$(found "$SANDBOX/out7d.txt" "$SA")"
verdict "rung 4 reads NOT REACHED" "$(found "$SANDBOX/out7d.txt" "rung 4 the granted read succeeds: NOT REACHED")"
verdict "reads no bucket" "$(missing "$CMD_LOG" "storage.googleapis.com")"

# --- case 7e: the adc.json is not the document the dispatcher writes ---------------------------
echo "case 7e: an adc.json with no audience fails rung 3 before any HTTP call"
reset_case
printf '{"type":"external_account"}\n' >"$DIR/adc.json"
chmod 644 "$DIR/adc.json"
run_ladder "$SANDBOX/out7e.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out7e.txt" "RUNG 3 FAILED")"
verdict "names the missing field" "$(found "$SANDBOX/out7e.txt" "names no audience")"
verdict "spends no HTTP call" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

echo "case 7f: an adc.json sourcing a different token file fails rung 3"
reset_case
write_adc /chuggernaut/cloud/somewhere-else/token
chmod 644 "$DIR/adc.json"
run_ladder "$SANDBOX/out7f.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out7f.txt" "RUNG 3 FAILED")"
verdict "names the file it would have read" "$(found "$SANDBOX/out7f.txt" "/chuggernaut/cloud/somewhere-else/token")"
verdict "spends no HTTP call" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

# --- case 8: the granted read is refused ------------------------------------------------
# The regression that matters most: a pipeline's status is its LAST command's, so
# `curl ... | head -1` would take the `if` branch and report PASS here.
echo "case 8: a 403 on the granted bucket is a rung-4 FAILURE, never a pass"
reset_case
: >"$STATE/granted_denied"
run_ladder "$SANDBOX/out8.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 4" "$(found "$SANDBOX/out8.txt" "RUNG 4 FAILED")"
verdict "rung 4 reads FAIL, not PASS" "$(found "$SANDBOX/out8.txt" "rung 4 the granted read succeeds: FAIL")"
verdict "verdict is not PASS" "$(missing "$SANDBOX/out8.txt" "VERDICT PASS")"
verdict "names the objectViewer binding, the member being proved" "$(found "$SANDBOX/out8.txt" "objectViewer binding")"
verdict "never reaches the denied bucket" "$(missing "$CMD_LOG" "/b/$DENIED/")"

# --- case 9: the granted read succeeds and returns nothing --------------------------------
echo "case 9: an empty canary is a rung-4 failure — the read is evidence, not a verdict"
reset_case
: >"$STATE/granted_empty"
run_ladder "$SANDBOX/out9.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 4" "$(found "$SANDBOX/out9.txt" "RUNG 4 FAILED")"
verdict "names the empty object" "$(found "$SANDBOX/out9.txt" "returned nothing")"

# --- case 10: THE NEGATIVE RUNG ------------------------------------------------------------
echo "case 10: reading the denied bucket is a FINDING, not a pass"
reset_case
: >"$STATE/denied_readable"
run_ladder "$SANDBOX/out10.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 5" "$(found "$SANDBOX/out10.txt" "RUNG 5 FAILED")"
verdict "calls it a real finding" "$(found "$SANDBOX/out10.txt" "REAL FINDING")"
verdict "rung 4 still reads PASS" "$(found "$SANDBOX/out10.txt" "rung 4 the granted read succeeds: PASS")"
verdict "verdict is not PASS" "$(missing "$SANDBOX/out10.txt" "VERDICT PASS")"

# --- case 11: refused for the wrong reason --------------------------------------------------
echo "case 11: a NOT FOUND refusal is inconclusive — an absent object refuses everyone"
reset_case
: >"$STATE/denied_absent"
run_ladder "$SANDBOX/out11.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 5" "$(found "$SANDBOX/out11.txt" "RUNG 5 FAILED")"
verdict "says it is inconclusive" "$(found "$SANDBOX/out11.txt" "inconclusive")"
verdict "names the status it got" "$(found "$SANDBOX/out11.txt" "404 NOT FOUND")"
verdict "does not claim a bounded credential" "$(missing "$SANDBOX/out11.txt" "VERDICT PASS")"

echo "case 11b: a denied read that never got an answer refuses nobody"
reset_case
: >"$STATE/denied_unreachable"
run_ladder "$SANDBOX/out11b.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 5" "$(found "$SANDBOX/out11b.txt" "RUNG 5 FAILED")"
verdict "says it is inconclusive" "$(found "$SANDBOX/out11b.txt" "inconclusive")"
verdict "does not claim a bounded credential" "$(missing "$SANDBOX/out11b.txt" "VERDICT PASS")"

echo "case 11c: a 401 is not a permission denial, so rung 5 stays unproved"
reset_case
: >"$STATE/denied_unauthorized"
run_ladder "$SANDBOX/out11c.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 5" "$(found "$SANDBOX/out11c.txt" "RUNG 5 FAILED")"
verdict "says only a 403 proves it" "$(found "$SANDBOX/out11c.txt" "only a 403")"
verdict "does not claim a bounded credential" "$(missing "$SANDBOX/out11c.txt" "VERDICT PASS")"

# --- case 12: no HTTP client, no JSON parser -------------------------------------------------
echo "case 12: an image with no curl reports NOT REACHED, and still fails"
reset_case
run_ladder "$SANDBOX/out12.txt" "$MINIMAL"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "rung 3 reads NOT REACHED" "$(found "$SANDBOX/out12.txt" "rung 3 the STS accepts the exchange: NOT REACHED")"
verdict "rung 3 does not read PASS" "$(missing "$SANDBOX/out12.txt" "rung 3 the STS accepts the exchange: PASS")"
verdict "names curl" "$(found "$SANDBOX/out12.txt" "curl is not on this image's PATH")"
verdict "rung 3b reads NOT REACHED" "$(found "$SANDBOX/out12.txt" "rung 3b the federated token impersonates the service account: NOT REACHED")"
verdict "verdict is a FAIL" "$(found "$SANDBOX/out12.txt" "VERDICT FAIL")"
verdict "rungs 1 and 2 still judged" "$(found "$SANDBOX/out12.txt" "rung 2 the claims are what we minted: PASS")"

echo "case 12b: an image with curl but no jq is a NAMED NOT REACHED, not a sed parser"
reset_case
run_ladder "$SANDBOX/out12b.txt" "$BIN:$MINIMAL"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "rung 3 reads NOT REACHED" "$(found "$SANDBOX/out12b.txt" "rung 3 the STS accepts the exchange: NOT REACHED")"
verdict "names jq" "$(found "$SANDBOX/out12b.txt" "jq is not on this image's PATH")"
verdict "spends no HTTP call" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"
verdict "verdict is a FAIL" "$(found "$SANDBOX/out12b.txt" "VERDICT FAIL")"

# --- case 13: a bucket input that was never supplied -------------------------------------------
echo "case 13: a missing denied_bucket cannot silently skip the negative rung"
reset_case
STATUS=0
env -i PATH="$BIN:/usr/bin:/bin" HOME="$SANDBOX" CHUG_CLOUD_ROOT="$CLOUD" \
	GOOGLE_APPLICATION_CREDENTIALS="$DIR/adc.json" \
	CHUG_INPUT_GRANTED_BUCKET="$GRANTED" \
	sh "$LADDER" >"$SANDBOX/out13.txt" 2>&1 || STATUS=$?

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "rung 5 reads NOT REACHED" "$(found "$SANDBOX/out13.txt" "rung 5 the UNGRANTED read is refused: NOT REACHED")"
verdict "says it is not skippable" "$(found "$SANDBOX/out13.txt" "not skippable")"
verdict "verdict is a FAIL" "$(found "$SANDBOX/out13.txt" "VERDICT FAIL")"

# --- rung 5b: the evaluator that must find nothing ------------------------------------------------
# The default PATH carries the stub curl and NO gcloud — the real agent image's
# shape. It is a stub rather than the host's curl on purpose: the ambient probe
# names a real address, and a suite that reached for it would be asking the
# machine it runs on what the answer is.
run_negative() { # run_negative <outfile> <cloud-root> [gac] [PATH]
	NSTATUS=0
	env -i \
		PATH="${4-$BIN:/usr/bin:/bin}" \
		HOME="$SANDBOX" \
		CHUG_CLOUD_ROOT="$2" \
		${3:+GOOGLE_APPLICATION_CREDENTIALS="$3"} \
		sh "$NEGATIVE" >"$1" 2>&1 || NSTATUS=$?
}

echo "case 14: rung 5b passes by finding nothing, and says the ambient probe found no server"
reset_case
run_negative "$SANDBOX/out14.txt" "$SANDBOX/absent-root"

verdict "exits 0" "$(yes_no "$NSTATUS")"
verdict "reports PASS" "$(found "$SANDBOX/out14.txt" "rung 5b a container declaring no identity gets nothing: PASS")"
verdict "notes the absent root" "$(found "$SANDBOX/out14.txt" "no $SANDBOX/absent-root directory at all")"
verdict "the ladder line says nothing was exercised" "$(found "$SANDBOX/out14.txt" "gets nothing: PASS — no metadata server was reachable, so ambient minting was NOT exercised")"
verdict "does not claim the server refused" "$(missing "$SANDBOX/out14.txt" "minted nothing")"
verdict "asked the metadata server by name" "$(found "$CMD_LOG" "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")"
verdict "also tried the link-local address" "$(found "$CMD_LOG" "http://169.254.169.254/computeMetadata/v1/")"
verdict "sent the required header" "$(found "$CMD_LOG" "Metadata-Flavor: Google")"
verdict "bounded the connect" "$(found "$CMD_LOG" "--connect-timeout 2")"
verdict "bounded the whole request" "$(found "$CMD_LOG" "--max-time 4")"
verdict "ran no gcloud" "$(missing "$CMD_LOG" "gcloud")"

echo "case 15: a credential file in a container that declared none is the finding"
reset_case
run_negative "$SANDBOX/out15.txt" "$CLOUD"

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names rung 5b" "$(found "$SANDBOX/out15.txt" "RUNG 5b FAILED")"
verdict "names non-inheritance" "$(found "$SANDBOX/out15.txt" "non-inheritance")"
verdict "lists the file it found" "$(found "$SANDBOX/out15.txt" "$DIR/token")"
verdict "reports FAIL" "$(found "$SANDBOX/out15.txt" "gets nothing: FAIL")"

echo "case 16: a stray GOOGLE_APPLICATION_CREDENTIALS is the finding"
reset_case
run_negative "$SANDBOX/out16.txt" "$SANDBOX/absent-root" "$DIR/adc.json"

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names the variable" "$(found "$SANDBOX/out16.txt" "GOOGLE_APPLICATION_CREDENTIALS is set to")"
verdict "reports FAIL" "$(found "$SANDBOX/out16.txt" "gets nothing: FAIL")"

echo "case 17: a gcloud minting from ambient credentials is the finding"
reset_case
run_negative "$SANDBOX/out17.txt" "$SANDBOX/absent-root" "" "$BIN:$SDK:/usr/bin:/bin"

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names ambient credentials" "$(found "$SANDBOX/out17.txt" "reaching ambient credentials")"
verdict "reports FAIL" "$(found "$SANDBOX/out17.txt" "gets nothing: FAIL")"

# --- case 18: THE RUNG THAT ONLY BITES ON GCE -----------------------------------------------------
# The finding rung 5b exists for once a worker runs on GCE: no file, no env var,
# and the node's own identity handed over by the metadata server anyway.
echo "case 18: a metadata server that mints the node's token is the finding"
reset_case
: >"$STATE/metadata_mints"
run_negative "$SANDBOX/out18.txt" "$SANDBOX/absent-root"

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names rung 5b" "$(found "$SANDBOX/out18.txt" "RUNG 5b FAILED")"
verdict "says a token was minted" "$(found "$SANDBOX/out18.txt" "MINTED AN ACCESS TOKEN")"
verdict "names the node's identity" "$(found "$SANDBOX/out18.txt" "NODE's own identity")"
verdict "reports FAIL" "$(found "$SANDBOX/out18.txt" "gets nothing: FAIL")"
verdict "does not report PASS" "$(missing "$SANDBOX/out18.txt" "gets nothing: PASS")"

echo "case 19: a metadata server that answers but mints nothing is a PASS that says so"
reset_case
: >"$STATE/metadata_answers"
run_negative "$SANDBOX/out19.txt" "$SANDBOX/absent-root"

verdict "exits 0" "$(yes_no "$NSTATUS")"
verdict "the ladder line says the server refused" "$(found "$SANDBOX/out19.txt" "gets nothing: PASS — a metadata server answered")"
verdict "names the status it got" "$(found "$SANDBOX/out19.txt" "HTTP 404 and minted nothing")"
verdict "does not claim it was unreachable" "$(missing "$SANDBOX/out19.txt" "NOT exercised")"
verdict "stops at the first server that answered" "$(missing "$CMD_LOG" "169.254.169.254")"

echo "case 20: with no curl the probe cannot run, and the ladder line says that, not PASS"
reset_case
run_negative "$SANDBOX/out20.txt" "$SANDBOX/absent-root" "" "$MINIMAL"

verdict "exits 0" "$(yes_no "$NSTATUS")"
verdict "names the missing tool" "$(found "$SANDBOX/out20.txt" "curl is absent, so the ambient probe did NOT run")"
verdict "does not claim a server refused" "$(missing "$SANDBOX/out20.txt" "minted nothing")"
verdict "spends no HTTP call" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

echo
if [ "$BAD" -eq 0 ]; then
	echo "gcp-proof.test.sh: all cases pass"
else
	echo "gcp-proof.test.sh: $BAD check(s) FAILED"
	exit 1
fi
