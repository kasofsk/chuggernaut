#!/bin/sh
# Shell test for gcp-proof.sh and gcp-proof-negative.sh — no GCP, no token, no
# federation, no network.
#
# It drives the real ladders against a stubbed `gcloud` on a controlled PATH and
# a sandbox `/chuggernaut/cloud` tree holding a hand-built JWT. What it pins is
# the class of bug a proof script is uniquely bad at revealing — THE ONE THAT
# REPORTS PASS:
#
#   1. THE VERDICT DEFAULTS TO FAIL. Any exit that does not run off the end of
#      the script reports FAIL, so a `set -eu` abort, a missing `base64` or a
#      future edit that adds a bare command cannot be read as a passing proof.
#   2. A FAILING READ IS A FAILING RUNG. Rung 4's status must come from gcloud,
#      not from the last command of a pipeline — `head -1` exits 0 on empty
#      input, and `pipefail` is not POSIX.
#   3. THE NEGATIVE RUNG IS REAL. A read of the denied bucket that SUCCEEDS is a
#      finding, and a refusal that is a NOT FOUND rather than a permission
#      denial is inconclusive — an absent object refuses everyone, so it must not
#      be allowed to look like a bounded credential.
#   4. A FAILURE NAMES ITS RUNG EXACTLY ONCE, and the rungs after it — never the
#      failing one — read NOT REACHED. The ladder is the deliverable, so it
#      prints on every path, last.
#   5. THE LADDER STOPS AT THE FIRST FAILURE. A rung that failed spends no
#      network call on the rungs above it.
#   6. RUNG 5B PASSES BY FINDING NOTHING, and fails on a credential, on a stray
#      GOOGLE_APPLICATION_CREDENTIALS, and on a gcloud that mints from ambient
#      credentials.
#
# Run:  .chug/tasks/gcp-proof.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
LADDER="$HERE/gcp-proof.sh"
NEGATIVE="$HERE/gcp-proof-negative.sh"

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

BIN="$SANDBOX/bin"
STATE="$SANDBOX/state"
CLOUD="$SANDBOX/cloud"
CMD_LOG="$SANDBOX/cmd.log"
DIR="$CLOUD/gcp-proof"
mkdir -p "$BIN" "$STATE" "$DIR"

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

# --- the gcloud stub ----------------------------------------------------------
# Every call appends its argv to one log, so the ORDER of the rungs and what a
# stopped ladder never reached are both readable from a single file. Outcomes
# are steered by marker files under $STATE, which is what lets one stub serve
# every case.
cat >"$BIN/gcloud" <<EOF
#!/bin/sh
echo "gcloud \$*" >> "$CMD_LOG"
case "\$1 \$2" in
"auth login")
  [ -f "$STATE/login_fails" ] && { echo "ERROR: Error connecting to the given credential's issuer"; exit 1; }
  exit 0 ;;
"auth print-access-token")
  [ -f "$STATE/sts_fails" ] && { echo "ERROR: unable to acquire impersonated credentials"; exit 1; }
  cat "$STATE/access_token"; exit 0 ;;
"storage cat")
  case "\$3" in
  *$GRANTED*)
    [ -f "$STATE/granted_denied" ] && { echo "ERROR: 403 does not have storage.objects.get access"; exit 1; }
    [ -f "$STATE/granted_empty" ] && exit 0
    echo "chuggernaut gcp-proof canary (granted bucket)"; exit 0 ;;
  *$DENIED*)
    [ -f "$STATE/denied_readable" ] && { echo "chuggernaut gcp-proof canary (denied bucket)"; exit 0; }
    [ -f "$STATE/denied_absent" ] && { echo "ERROR: 404 The following URLs matched no objects or files"; exit 1; }
    echo "ERROR: 403 does not have storage.objects.get access to the object"; exit 1 ;;
  esac
  exit 1 ;;
esac
exit 1
EOF
chmod +x "$BIN/gcloud"

reset_case() {
	rm -f "$STATE"/* "$CMD_LOG"
	rm -rf "$CLOUD"
	mkdir -p "$DIR"
	printf 'ya29.stub-access-token\n' >"$STATE/access_token"
	write_token kasofsk/chuggernaut gcp-proof work
	printf '{"type":"external_account"}\n' >"$DIR/adc.json"
	chmod 600 "$DIR/token"
	chmod 644 "$DIR/adc.json"
}

run_ladder() { # run_ladder <outfile> [PATH override]
	STATUS=0
	env -i \
		PATH="${2-$BIN:/usr/bin:/bin}" \
		HOME="$SANDBOX" \
		CHUG_CLOUD_ROOT="$CLOUD" \
		GOOGLE_APPLICATION_CREDENTIALS="$DIR/adc.json" \
		CHUG_INPUT_GRANTED_BUCKET="$GRANTED" \
		CHUG_INPUT_DENIED_BUCKET="$DENIED" \
		sh "$LADDER" >"$1" 2>&1 || STATUS=$?
}

echo "gcp-proof.test.sh: driving the real ladder against stubs"

# --- case 1: everything works --------------------------------------------------
echo "case 1: a wired, bounded credential climbs all five rungs"
reset_case
run_ladder "$SANDBOX/out1.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "verdict is PASS" "$(found "$SANDBOX/out1.txt" "VERDICT PASS")"
verdict "rung 1 PASS" "$(found "$SANDBOX/out1.txt" "rung 1 the credential is injected: PASS")"
verdict "rung 2 PASS" "$(found "$SANDBOX/out1.txt" "rung 2 the claims are what we minted: PASS")"
verdict "rung 3 PASS" "$(found "$SANDBOX/out1.txt" "rung 3 the STS accepts the exchange: PASS")"
verdict "rung 4 PASS" "$(found "$SANDBOX/out1.txt" "rung 4 the granted read succeeds: PASS")"
verdict "rung 5 PASS" "$(found "$SANDBOX/out1.txt" "rung 5 the UNGRANTED read is refused: PASS")"
verdict "nothing reads NOT REACHED" "$(missing "$SANDBOX/out1.txt" ": NOT REACHED")"
verdict "names the evaluator as 5b's owner" "$(found "$SANDBOX/out1.txt" "rung 5b")"
verdict "read the granted bucket" "$(found "$CMD_LOG" "gs://$GRANTED/canary.txt")"
verdict "read the denied bucket" "$(found "$CMD_LOG" "gs://$DENIED/canary.txt")"

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
verdict "rung 5 reads NOT REACHED" "$(found "$SANDBOX/out2.txt" "rung 5 the UNGRANTED read is refused: NOT REACHED")"
verdict "the failed rung is not ALSO NOT REACHED" "$(missing "$SANDBOX/out2.txt" "rung 1 the credential is injected: NOT REACHED")"
verdict "runs no gcloud at all" "$([ -f "$CMD_LOG" ] && echo no || echo ok)"

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
verdict "reads no bucket" "$(missing "$CMD_LOG" "storage cat")"

# --- case 7: the STS refuses ----------------------------------------------------------
echo "case 7: an STS refusal stops the ladder at rung 3"
reset_case
: >"$STATE/sts_fails"
run_ladder "$SANDBOX/out7.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 3" "$(found "$SANDBOX/out7.txt" "RUNG 3 FAILED")"
verdict "rung 4 reads NOT REACHED" "$(found "$SANDBOX/out7.txt" "rung 4 the granted read succeeds: NOT REACHED")"
verdict "reads no bucket" "$(missing "$CMD_LOG" "storage cat")"

# --- case 8: the granted read is refused ------------------------------------------------
# The regression that matters most: a pipeline's status is its LAST command's, so
# `gcloud ... | head -1` would take the `if` branch and report PASS here.
echo "case 8: a 403 on the granted bucket is a rung-4 FAILURE, never a pass"
reset_case
: >"$STATE/granted_denied"
run_ladder "$SANDBOX/out8.txt"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "names rung 4" "$(found "$SANDBOX/out8.txt" "RUNG 4 FAILED")"
verdict "rung 4 reads FAIL, not PASS" "$(found "$SANDBOX/out8.txt" "rung 4 the granted read succeeds: FAIL")"
verdict "verdict is not PASS" "$(missing "$SANDBOX/out8.txt" "VERDICT PASS")"
verdict "names the %2F member as the first suspect" "$(found "$SANDBOX/out8.txt" "%2F")"
verdict "never reaches the denied bucket" "$(missing "$CMD_LOG" "gs://$DENIED")"

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
verdict "does not claim a bounded credential" "$(missing "$SANDBOX/out11.txt" "VERDICT PASS")"

# --- case 12: no gcloud ----------------------------------------------------------------------
echo "case 12: an image with no gcloud reports NOT REACHED, and still fails"
reset_case
run_ladder "$SANDBOX/out12.txt" "/usr/bin:/bin"

verdict "exits non-zero" "$(non_zero "$STATUS")"
verdict "rung 3 reads NOT REACHED" "$(found "$SANDBOX/out12.txt" "rung 3 the STS accepts the exchange: NOT REACHED")"
verdict "rung 3 does not read PASS" "$(missing "$SANDBOX/out12.txt" "rung 3 the STS accepts the exchange: PASS")"
verdict "verdict is a FAIL" "$(found "$SANDBOX/out12.txt" "VERDICT FAIL")"
verdict "rungs 1 and 2 still judged" "$(found "$SANDBOX/out12.txt" "rung 2 the claims are what we minted: PASS")"

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
run_negative() { # run_negative <outfile> <cloud-root> [gac] [PATH]
	NSTATUS=0
	env -i \
		PATH="${4-$BIN:/usr/bin:/bin}" \
		HOME="$SANDBOX" \
		CHUG_CLOUD_ROOT="$2" \
		${3:+GOOGLE_APPLICATION_CREDENTIALS="$3"} \
		sh "$NEGATIVE" >"$1" 2>&1 || NSTATUS=$?
}

echo "case 14: rung 5b passes by finding nothing"
reset_case
: >"$STATE/no_ambient"
run_negative "$SANDBOX/out14.txt" "$SANDBOX/absent-root" "" "/usr/bin:/bin"

verdict "exits 0" "$(yes_no "$NSTATUS")"
verdict "reports PASS" "$(found "$SANDBOX/out14.txt" "rung 5b a container declaring no identity gets nothing: PASS")"
verdict "notes the absent root" "$(found "$SANDBOX/out14.txt" "no $SANDBOX/absent-root directory at all")"

echo "case 15: a credential file in a container that declared none is the finding"
reset_case
run_negative "$SANDBOX/out15.txt" "$CLOUD" "" "/usr/bin:/bin"

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names rung 5b" "$(found "$SANDBOX/out15.txt" "RUNG 5b FAILED")"
verdict "names non-inheritance" "$(found "$SANDBOX/out15.txt" "non-inheritance")"
verdict "lists the file it found" "$(found "$SANDBOX/out15.txt" "$DIR/token")"
verdict "reports FAIL" "$(found "$SANDBOX/out15.txt" "gets nothing: FAIL")"

echo "case 16: a stray GOOGLE_APPLICATION_CREDENTIALS is the finding"
reset_case
run_negative "$SANDBOX/out16.txt" "$SANDBOX/absent-root" "$DIR/adc.json" "/usr/bin:/bin"

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names the variable" "$(found "$SANDBOX/out16.txt" "GOOGLE_APPLICATION_CREDENTIALS is set to")"
verdict "reports FAIL" "$(found "$SANDBOX/out16.txt" "gets nothing: FAIL")"

echo "case 17: a gcloud minting from ambient credentials is the finding"
reset_case
run_negative "$SANDBOX/out17.txt" "$SANDBOX/absent-root" ""

verdict "exits non-zero" "$(non_zero "$NSTATUS")"
verdict "names ambient credentials" "$(found "$SANDBOX/out17.txt" "reaching ambient credentials")"
verdict "reports FAIL" "$(found "$SANDBOX/out17.txt" "gets nothing: FAIL")"

echo
if [ "$BAD" -eq 0 ]; then
	echo "gcp-proof.test.sh: all cases pass"
else
	echo "gcp-proof.test.sh: $BAD check(s) FAILED"
	exit 1
fi
