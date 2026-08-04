#!/bin/sh
# The workload-identity proof ladder — design #313 half A, slice S6. Run as the
# `work` step of a `gcp-proof` job (.chug/jobs/gcp-proof.yaml), against
# chuggernaut's OWN GCP project (infra/gcp-proof), deliberately before any live
# project's deploy path depends on the mechanism.
#
# THE DELIVERABLE IS STDOUT. The dispatcher harvests a command work task's
# container logs into its `stdout.log` artifact (spec §3.2) and a worker keeps
# only the LAST 700 KiB of them, so the LADDER summary is printed LAST, from the
# EXIT trap, on every path including a failure. Each rung reads PASS, FAIL or
# NOT REACHED, in order, and the run stops at the first failure.
#
# EACH RUNG PROVES SOMETHING DIFFERENT, and a failure names which one broke:
#
#   1. the credential is injected — modes and env, the platform's own promise
#   2. the claims are what we minted — decoded LOCALLY, with no network call, so
#      a wrong claim names its field instead of coming back as a 403
#   3. the STS accepts the exchange — the federation itself
#   4. the granted read succeeds — the binding grants what it says
#   5. the ungranted read is REFUSED — the binding grants NOTHING ELSE
#
# RUNG 5 IS WHY THIS SCRIPT EXISTS. Rungs 1-4 prove that a credential was wired;
# only 5 proves it is BOUNDED, and a ladder that stops at 4 has demonstrated the
# less interesting half. Its sibling rung 5b — that a container declaring no
# identity gets nothing at all — cannot run here by construction and is asserted
# by the `no-identity` stage-0 evaluator (.chug/tasks/gcp-proof-negative.sh).
#
# RUNGS 1-2 ARE MEANINGFUL BEFORE ANY TERRAFORM IS APPLIED: they read the
# injected files and decode the token offline. Rungs 3-5 need the whole sequence
# in infra/README.md §2 — a red rung 3 before that is the sequence being
# incomplete, not a defect, and the runbook says so rather than this script
# guessing. The cloud root is env-overridable for exactly one reason: so
# .chug/tasks/gcp-proof.test.sh can drive the real ladder against stubs.
set -eu

CLOUD_ROOT="${CHUG_CLOUD_ROOT:-/chuggernaut/cloud}"
IDENTITY=gcp-proof
DIR="$CLOUD_ROOT/$IDENTITY"
TOKEN="$DIR/token"
ADC="$DIR/adc.json"
CANARY=canary.txt

# What the issuer mints for this job type's work container (#313 A1,
# crates/auth/src/workload.rs). Hardcoded rather than read from the environment:
# a proof that asserts whatever it was handed asserts nothing.
EXPECT_PROJECT="kasofsk/chuggernaut"
EXPECT_JOB_TYPE="gcp-proof"
EXPECT_CONTAINER="work"
EXPECT_SUB="project:$EXPECT_PROJECT:type:$EXPECT_JOB_TYPE"
EXPECT_WORKLOAD="$EXPECT_PROJECT:$EXPECT_JOB_TYPE:$EXPECT_CONTAINER"
SUBJECT_BYTES_MAX=127

GRANTED_BUCKET="${CHUG_INPUT_GRANTED_BUCKET:-}"
DENIED_BUCKET="${CHUG_INPUT_DENIED_BUCKET:-}"

SUMMARY=""
CLEARED=0
VERDICT="FAIL — the ladder did not finish"

rung_title() {
	case "$1" in
	1) echo "the credential is injected" ;;
	2) echo "the claims are what we minted" ;;
	3) echo "the STS accepts the exchange" ;;
	4) echo "the granted read succeeds" ;;
	5) echo "the UNGRANTED read is refused" ;;
	esac
}

rung_pass() {
	CLEARED="$1"
	SUMMARY="$SUMMARY
  rung $1 $(rung_title "$1"): PASS — $2"
	echo "gcp-proof: rung $1 PASS — $2"
}

# Ends the run at the first failure. The failing rung counts as reached, so the
# summary reports it as FAIL rather than listing it twice.
rung_fail() {
	CLEARED="$1"
	SUMMARY="$SUMMARY
  rung $1 $(rung_title "$1"): FAIL — $2"
	VERDICT="FAIL at rung $1 ($(rung_title "$1"))"
	echo "!!! gcp-proof: RUNG $1 FAILED — $2" >&2
	exit 1
}

# The environment could not put the ladder in a position to judge this rung.
# Distinct from FAIL on purpose: nothing was disproved, and the rung and every
# rung above it read NOT REACHED. Still a failed run — a proof that did not
# reach its negatives has not proved anything.
rung_unreachable() {
	VERDICT="FAIL — rung $1 ($(rung_title "$1")) was NOT REACHED"
	echo "!!! gcp-proof: RUNG $1 NOT REACHED — $2" >&2
	exit 1
}

print_summary() {
	echo
	echo "gcp-proof: LADDER (design #313 half A) — identity=$IDENTITY root=$CLOUD_ROOT"
	printf '%s\n' "$SUMMARY" | sed '/^[[:space:]]*$/d'
	_next=$((CLEARED + 1))
	while [ "$_next" -le 5 ]; do
		echo "  rung $_next $(rung_title "$_next"): NOT REACHED"
		_next=$((_next + 1))
	done
	echo "  rung 5b a container declaring no identity gets nothing: asserted by the \`no-identity\` stage-0 evaluator"
	echo "gcp-proof: VERDICT $VERDICT"
}
trap print_summary EXIT

# --- rung 1: the credential is injected ---------------------------------------
echo "gcp-proof: rung 1 — the credential is injected"
[ -f "$TOKEN" ] || rung_fail 1 "no token at $TOKEN. Either #313 S4 injection is not deployed (infra/README.md §2 step a), or this job type lost its \`workload_identities:\`"
[ -f "$ADC" ] || rung_fail 1 "no external-account config at $ADC, though the token beside it exists"

# 0600 for the bearer token, 0644 for the config that names it (#313 A3). A token
# at 0644 is a finding even though every process here shares one uid: the mode is
# what the dispatcher promised, and a drift means the promise moved.
token_mode=$(stat -c '%a' "$TOKEN" 2>/dev/null || stat -f '%Lp' "$TOKEN")
adc_mode=$(stat -c '%a' "$ADC" 2>/dev/null || stat -f '%Lp' "$ADC")
[ "$token_mode" = "600" ] || rung_fail 1 "the token is mode $token_mode, and the dispatcher promised 600"
[ "$adc_mode" = "644" ] || rung_fail 1 "adc.json is mode $adc_mode, and the dispatcher promised 644"
[ "${GOOGLE_APPLICATION_CREDENTIALS:-}" = "$ADC" ] ||
	rung_fail 1 "GOOGLE_APPLICATION_CREDENTIALS is '${GOOGLE_APPLICATION_CREDENTIALS:-<unset>}', not $ADC — unmodified tooling would find no credential"
rung_pass 1 "token 0600, adc.json 0644, GOOGLE_APPLICATION_CREDENTIALS points at it"

# --- rung 2: the claims are what we minted ------------------------------------
# Deliberately offline. A claim set that is wrong locally is wrong at the STS
# too, and finding out here names the field instead of returning a 403.
echo "gcp-proof: rung 2 — the claims are what we minted (offline)"

claims_of() {
	_payload=$(cut -d. -f2 <"$1" | tr '_-' '/+')
	case $((${#_payload} % 4)) in
	2) _payload="$_payload==" ;;
	3) _payload="$_payload=" ;;
	esac
	printf '%s' "$_payload" | base64 -d 2>/dev/null || printf '%s' "$_payload" | base64 --decode
}

claim_is() {
	case "$1" in
	*"\"$2\":\"$3\""*) return 0 ;;
	esac
	return 1
}

claims=$(claims_of "$TOKEN") || rung_fail 2 "the token's payload is not decodable base64url — is $TOKEN a JWT?"
echo "  claims: $claims"

claim_is "$claims" sub "$EXPECT_SUB" || rung_fail 2 "sub is not \"$EXPECT_SUB\" — a binding on the subject would match nothing"
claim_is "$claims" project "$EXPECT_PROJECT" || rung_fail 2 "the project claim is not \"$EXPECT_PROJECT\" — the provider's attribute condition refuses this token before any binding is consulted"
claim_is "$claims" job_type "$EXPECT_JOB_TYPE" || rung_fail 2 "the job_type claim is not \"$EXPECT_JOB_TYPE\""
claim_is "$claims" container "$EXPECT_CONTAINER" || rung_fail 2 "the container claim is not \"$EXPECT_CONTAINER\" — per-container scoping (#313 A5) is not expressible cloud-side without it"
claim_is "$claims" workload "$EXPECT_WORKLOAD" || rung_fail 2 "the workload composite is not \"$EXPECT_WORKLOAD\" — this is the exact string the principalSet member percent-encodes"

# The 127-byte cap is a hard error at exchange time, so spend a comparison here
# rather than a round trip discovering it (#313 A1).
[ "${#EXPECT_SUB}" -le "$SUBJECT_BYTES_MAX" ] ||
	rung_fail 2 "sub is ${#EXPECT_SUB} bytes, over the ${SUBJECT_BYTES_MAX}-byte google.subject cap"
rung_pass 2 "sub, project, job_type, container and workload all as minted; sub is ${#EXPECT_SUB} bytes"

# --- rung 3: the STS accepts the exchange -------------------------------------
echo "gcp-proof: rung 3 — the STS accepts the exchange"
command -v gcloud >/dev/null 2>&1 ||
	rung_unreachable 3 "gcloud is not on this image's PATH, so nothing above rung 2 can be judged from this container"

if ! login=$(gcloud auth login --cred-file="$ADC" --quiet 2>&1); then
	rung_fail 3 "gcloud refused the external-account config: $login
      If that says \`Error connecting to the given credential's issuer\`, suspect
      the UPLOADED JWK SET before the issuer: GCP does not validate it at create
      time, so the error blames the wrong thing (#313 A4, infra/README.md §5)."
fi
if ! access=$(gcloud auth print-access-token 2>&1); then
	rung_fail 3 "the STS refused the exchange: $access
      Same trap as above: an issuer-connection error here is usually the JWKS
      upload (infra/README.md §5), not the issuer."
fi
[ -n "$access" ] || rung_fail 3 "the STS returned an empty access token without reporting an error"
rung_pass 3 "exchanged the workload token for an access token (${#access} bytes)"

# --- rung 4: the granted read succeeds ----------------------------------------
echo "gcp-proof: rung 4 — the granted read succeeds"
[ -n "$GRANTED_BUCKET" ] ||
	rung_unreachable 4 "no granted_bucket input — set it from \`terraform output granted_bucket\`"

if ! body=$(gcloud storage cat "gs://$GRANTED_BUCKET/$CANARY" 2>&1); then
	rung_fail 4 "could not read gs://$GRANTED_BUCKET/$CANARY: $body
      Suspect the principalSet member first. The \`/\` in the project component
      must be percent-encoded as %2F; a literal \`/\` applies cleanly, grants
      NOTHING, and surfaces as an ordinary 403. Compare against
      \`terraform output principal_set\` (infra/README.md)."
fi
# The read is evidence, not a verdict: an empty body means the object is not what
# the terraform wrote, and trusting the exit code alone is how a proof passes
# without proving anything.
[ -n "$body" ] || rung_fail 4 "the read succeeded but returned nothing — gs://$GRANTED_BUCKET/$CANARY is empty, so it is not the canary the terraform writes"
rung_pass 4 "read the canary out of gs://$GRANTED_BUCKET: $(printf '%s' "$body" | head -n 1)"

# --- rung 5: the ungranted read is refused ------------------------------------
# The rung that matters. A grant that reads everything is not the grant we wrote.
echo "gcp-proof: rung 5 — the UNGRANTED read is refused"
[ -n "$DENIED_BUCKET" ] ||
	rung_unreachable 5 "no denied_bucket input, and the negative rung is not skippable — set it from \`terraform output denied_bucket\`"

if refusal=$(gcloud storage cat "gs://$DENIED_BUCKET/$CANARY" 2>&1); then
	rung_fail 5 "READ THE DENIED BUCKET. The service account's grant is wider than
      infra/gcp-proof declares, or the token became an identity no binding there
      names. THIS IS A REAL FINDING, not a test bug."
fi
# A refusal for the wrong reason proves nothing. infra/gcp-proof writes a canary
# into BOTH buckets precisely so that this rung's refusal is a permission denial
# and never a missing object.
case "$refusal" in
*[Nn]"ot "[Ff]ound* | *404* | *"does not exist"*)
	rung_fail 5 "the denied read was refused with a NOT FOUND, not a permission denial:
      $refusal
      That is inconclusive — an absent object refuses everyone. Confirm
      gs://$DENIED_BUCKET/$CANARY exists (terraform writes it)."
	;;
esac
rung_pass 5 "refused, as declared: $(printf '%s' "$refusal" | head -n 1)"

VERDICT="PASS — the credential is bounded (rung 5b is the evaluator's to report)"
exit 0
