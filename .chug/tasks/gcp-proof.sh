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
#   3b. the federated token impersonates the SA — the workloadIdentityUser
#      binding on the principalSet member (#313 D4)
#   4. the granted read succeeds — the binding grants what it says
#   5. the ungranted read is REFUSED — the binding grants NOTHING ELSE
#
# RUNG 5 IS WHY THIS SCRIPT EXISTS. Rungs 1-4 prove that a credential was wired;
# only 5 proves it is BOUNDED, and a ladder that stops at 4 has demonstrated the
# less interesting half. Its sibling rung 5b — that a container declaring no
# identity gets nothing at all — cannot run here by construction and is asserted
# by the `no-identity` stage-0 evaluator (.chug/tasks/gcp-proof-negative.sh).
#
# RUNGS 3-5 SPEAK REST, NOT gcloud. Job #425 reached rung 3 and stopped: no job
# type here pulls a public image and none of the two agent images carries the
# gcloud SDK (#313 gap 11), so the three calls are made with curl + jq, which
# both images do carry. WHAT THAT COSTS IS RECORDED, not hidden: #313 A3 argues
# the ADC file shape on the grounds that every Google client library already
# reads it, and a curl rung proves the STS accepts our token WITHOUT proving
# that claim about tooling. See A3's "Delivery" note.
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

# The scope gcloud's default exchange asked for, kept unchanged so this rewrite
# moves only the transport and not the authorization surface under test.
SCOPE="https://www.googleapis.com/auth/cloud-platform"
GCS_HOST="https://storage.googleapis.com"
HTTP_TIMEOUT=30

# The ladder in order. Rung ids are strings, not a counter, because 3b is a
# rung of its own: an STS refusal and a refused impersonation are different
# findings, and reporting them as one hides which half broke.
RUNGS="1 2 3 3b 4 5"
JUDGED=""
SCRATCH=""

SUMMARY=""
VERDICT="FAIL — the ladder did not finish"

rung_title() {
	case "$1" in
	1) echo "the credential is injected" ;;
	2) echo "the claims are what we minted" ;;
	3) echo "the STS accepts the exchange" ;;
	3b) echo "the federated token impersonates the service account" ;;
	4) echo "the granted read succeeds" ;;
	5) echo "the UNGRANTED read is refused" ;;
	esac
}

rung_pass() {
	JUDGED="$JUDGED $1"
	SUMMARY="$SUMMARY
  rung $1 $(rung_title "$1"): PASS — $2"
	echo "gcp-proof: rung $1 PASS — $2"
}

# Ends the run at the first failure. The failing rung counts as judged, so the
# summary reports it as FAIL rather than listing it twice.
rung_fail() {
	JUDGED="$JUDGED $1"
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
	for _id in $RUNGS; do
		case " $JUDGED " in
		*" $_id "*) continue ;;
		esac
		echo "  rung $_id $(rung_title "$_id"): NOT REACHED"
	done
	echo "  rung 5b a container declaring no identity gets nothing: asserted by the \`no-identity\` stage-0 evaluator"
	echo "gcp-proof: VERDICT $VERDICT"
}

# The summary is the deliverable, so it prints even when the scratch dir holding
# the copied subject token still has to be reclaimed.
on_exit() {
	if [ -n "$SCRATCH" ]; then
		rm -rf "$SCRATCH"
	fi
	print_summary
}
trap on_exit EXIT

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
echo "gcp-proof: rung 3 — the STS accepts the exchange (over curl, not gcloud)"
for tool in curl jq; do
	command -v "$tool" >/dev/null 2>&1 ||
		rung_unreachable 3 "$tool is not on this image's PATH, so nothing above rung 2 can be judged from this container. The exchange is three ordinary HTTPS calls and needs no SDK, but it does need an HTTP client and a JSON parser — digging a token out with sed would be guessing, not proving"
done

# Every request lands its body and its transport error in a file so a rung can
# quote both, and reports the HTTP status rather than an exit code: 000 means
# curl never got an answer, which no rung may read as a refusal.
http_call() {
	_code=$(curl --silent --show-error --max-time "$HTTP_TIMEOUT" \
		--output "$BODY_FILE" --write-out '%{http_code}' "$@" 2>"$ERR_FILE") || _code=000
	[ -n "$_code" ] || _code=000
	printf '%s' "$_code"
}

http_detail() {
	if [ -s "$BODY_FILE" ]; then
		head -n 5 "$BODY_FILE"
	else
		head -n 5 "$ERR_FILE"
	fi
}

# The document a Google client library would have read, read by hand. Values
# come from it rather than from constants because the audience and the service
# account are what the terraform decides, and a proof that hardcoded them would
# be proving its own copy.
adc_string() {
	jq -r "$1 // empty" <"$ADC" 2>/dev/null || :
}

adc_type=$(adc_string .type)
audience=$(adc_string .audience)
subject_token_type=$(adc_string .subject_token_type)
token_url=$(adc_string .token_url)
impersonation_url=$(adc_string .service_account_impersonation_url)
source_file=$(adc_string .credential_source.file)

[ "$adc_type" = external_account ] ||
	rung_fail 3 "$ADC is not an external_account config (type=${adc_type:-<unparseable>}) — the dispatcher writes one per granted identity (#313 A3)"
[ -n "$audience" ] || rung_fail 3 "$ADC names no audience, so there is nothing to exchange against"
[ -n "$subject_token_type" ] || rung_fail 3 "$ADC names no subject_token_type"
[ -n "$token_url" ] || rung_fail 3 "$ADC names no token_url"
[ -n "$impersonation_url" ] ||
	rung_fail 3 "$ADC names no service_account_impersonation_url, though #313 D4 says the exchanged token impersonates a service account"
[ "$source_file" = "$TOKEN" ] ||
	rung_fail 3 "$ADC sources its subject token from '${source_file:-<unset>}', not the $TOKEN rung 1 checked — unmodified tooling would read a different file"

SCRATCH=$(mktemp -d)
BODY_FILE="$SCRATCH/body"
ERR_FILE="$SCRATCH/err"
SUBJECT_FILE="$SCRATCH/subject_token"
# Copied rather than passed on argv: a JWT in a command line is a JWT in `ps`,
# and the trailing newline a hand-written token file may carry is not part of it.
tr -d '\r\n' <"$TOKEN" >"$SUBJECT_FILE"

code=$(http_call "$token_url" \
	--data-urlencode "audience=$audience" \
	--data-urlencode "grant_type=urn:ietf:params:oauth:grant-type:token-exchange" \
	--data-urlencode "requested_token_type=urn:ietf:params:oauth:token-type:access_token" \
	--data-urlencode "scope=$SCOPE" \
	--data-urlencode "subject_token_type=$subject_token_type" \
	--data-urlencode "subject_token@$SUBJECT_FILE")
case "$code" in
2*) ;;
000)
	rung_fail 3 "no HTTP response from $token_url: $(http_detail)
      Nothing was disproved by a call that never arrived — check egress before
      the federation."
	;;
*)
	rung_fail 3 "the STS refused the exchange (HTTP $code): $(http_detail)
      If that names \`Error connecting to the given credential's issuer\`, suspect
      the UPLOADED JWK SET before the issuer: GCP does not validate it at create
      time, so the error blames the wrong thing (#313 A4, infra/README.md §5)."
	;;
esac
federated=$(jq -r '.access_token // empty' <"$BODY_FILE" 2>/dev/null || :)
[ -n "$federated" ] ||
	rung_fail 3 "the STS answered HTTP $code with no access_token: $(http_detail)"
rung_pass 3 "the STS exchanged the workload token for a federated access token (${#federated} bytes), scope $SCOPE"

# --- rung 3b: the federated token impersonates the service account ------------
# Split from rung 3 because the two fail for different reasons: rung 3 is the
# provider and its attribute condition, 3b is the workloadIdentityUser binding
# on the principalSet member.
echo "gcp-proof: rung 3b — the federated token impersonates the service account"
service_account=${impersonation_url##*/serviceAccounts/}
service_account=${service_account%%:*}

code=$(http_call "$impersonation_url" \
	--header "Content-Type: application/json; charset=utf-8" \
	--header "Authorization: Bearer $federated" \
	--data "{\"scope\":[\"$SCOPE\"]}")
case "$code" in
2*) ;;
000)
	rung_fail 3b "no HTTP response from $impersonation_url: $(http_detail)"
	;;
*)
	rung_fail 3b "iamcredentials refused to mint for $service_account (HTTP $code): $(http_detail)
      Suspect the principalSet member first. The \`/\` in the project component
      must be percent-encoded as %2F; a literal \`/\` applies cleanly, grants
      NOTHING, and surfaces as an ordinary 403. Compare against
      \`terraform output principal_set\` (infra/README.md)."
	;;
esac
access=$(jq -r '.accessToken // empty' <"$BODY_FILE" 2>/dev/null || :)
[ -n "$access" ] ||
	rung_fail 3b "iamcredentials answered HTTP $code with no accessToken: $(http_detail)"
rung_pass 3b "minted a service-account access token for $service_account (${#access} bytes)"

# One object read, by the SA token rung 3b minted. `alt=media` returns the bytes
# rather than the object's metadata, which is what makes an empty body legible.
gcs_read() {
	http_call "$GCS_HOST/storage/v1/b/$1/o/$CANARY?alt=media" \
		--header "Authorization: Bearer $access"
}

# --- rung 4: the granted read succeeds ----------------------------------------
echo "gcp-proof: rung 4 — the granted read succeeds"
[ -n "$GRANTED_BUCKET" ] ||
	rung_unreachable 4 "no granted_bucket input — set it from \`terraform output granted_bucket\`"

code=$(gcs_read "$GRANTED_BUCKET")
case "$code" in
2*) ;;
000)
	rung_fail 4 "no HTTP response reading gs://$GRANTED_BUCKET/$CANARY: $(http_detail)"
	;;
*)
	rung_fail 4 "could not read gs://$GRANTED_BUCKET/$CANARY (HTTP $code): $(http_detail)
      Suspect the objectViewer binding on the granted bucket: rung 3b already
      proved the principalSet member and the impersonation, so what is left is
      what the service account may read. Compare against
      \`terraform output granted_bucket\` (infra/README.md)."
	;;
esac
# The read is evidence, not a verdict: an empty body means the object is not what
# the terraform wrote, and trusting the status alone is how a proof passes
# without proving anything.
body=$(cat "$BODY_FILE")
[ -n "$body" ] || rung_fail 4 "the read succeeded but returned nothing — gs://$GRANTED_BUCKET/$CANARY is empty, so it is not the canary the terraform writes"
rung_pass 4 "read the canary out of gs://$GRANTED_BUCKET: $(printf '%s' "$body" | head -n 1)"

# --- rung 5: the ungranted read is refused ------------------------------------
# The rung that matters. A grant that reads everything is not the grant we wrote.
echo "gcp-proof: rung 5 — the UNGRANTED read is refused"
[ -n "$DENIED_BUCKET" ] ||
	rung_unreachable 5 "no denied_bucket input, and the negative rung is not skippable — set it from \`terraform output denied_bucket\`"

# A refusal for the wrong reason proves nothing. infra/gcp-proof writes a canary
# into BOTH buckets precisely so that this rung's refusal is a 403 permission
# denial and never a 404, and a request that was never answered refuses nobody.
code=$(gcs_read "$DENIED_BUCKET")
case "$code" in
2*)
	rung_fail 5 "READ THE DENIED BUCKET (HTTP $code). The service account's grant is wider than
      infra/gcp-proof declares, or the token became an identity no binding there
      names. THIS IS A REAL FINDING, not a test bug."
	;;
000)
	rung_fail 5 "the denied read never reached $GCS_HOST: $(http_detail)
      That is inconclusive — a request nobody answered is not a refusal."
	;;
404)
	rung_fail 5 "the denied read was refused with a 404 NOT FOUND, not a permission denial:
      $(http_detail)
      That is inconclusive — an absent object refuses everyone. Confirm
      gs://$DENIED_BUCKET/$CANARY exists (terraform writes it)."
	;;
403) ;;
*)
	rung_fail 5 "the denied read failed with HTTP $code, which is not a permission denial:
      $(http_detail)
      That is inconclusive — only a 403 shows the binding refusing this identity."
	;;
esac
rung_pass 5 "refused with HTTP $code, as declared: $(http_detail | head -n 1)"

VERDICT="PASS — the credential is bounded (rung 5b is the evaluator's to report)"
exit 0
