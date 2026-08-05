#!/bin/sh
# Rung 5b of the workload-identity proof — design #313 half A. Run as the
# `no-identity` stage-0 evaluator of the `gcp-proof` job type.
#
# It is a SEPARATE container because the property under test is
# NON-INHERITANCE: `.chug/jobs/gcp-proof.yaml` declares `workload_identities:`
# on `work` and deliberately not on this evaluator, and #313 A5 says a container
# receives exactly the identities its own block declares. The work container
# cannot test that — only a container that declared none can.
#
# SO THIS EVALUATOR PASSES BY FINDING NOTHING. A credential here is the finding,
# and the verdict defaults to FAIL so that any path out of this script other
# than reaching the end reports one.
#
# ITS THIRD CHECK IS THE ONE THAT ONLY BECOMES LOAD-BEARING ON GCE: that this
# container cannot mint from AMBIENT credentials — the node's own identity, which
# no per-container discipline in the dispatcher prevents. On today's on-prem
# workers there is no metadata server to answer it, so a PASS there tested
# NOTHING; the verdict line says which of the two happened rather than reading as
# a bare PASS, because the day a worker runs on GCE is the day the difference
# matters and nobody will re-read this script to find out.
set -eu

CLOUD_ROOT="${CHUG_CLOUD_ROOT:-/chuggernaut/cloud}"
FILES_LISTED_MAX=20
VERDICT="FAIL — the check did not finish"

# The GCE metadata server, asked over plain HTTP because that is the whole
# interface — no image here carries the gcloud SDK (#313 gap 11) and this needs
# none, the same move rungs 3-5 made in job #426. BOTH spellings are probed: a
# container with its own resolver can fail to resolve the name while the
# link-local address still routes, and a probe that only tried the name would
# report "unreachable" for a node whose identity it can in fact reach.
METADATA_HOSTS="metadata.google.internal 169.254.169.254"
METADATA_PATH="/computeMetadata/v1/instance/service-accounts/default/token"
# Today's workers are on-prem, with no metadata server and often no route to
# that address at all, so every probe is bounded: an unanswered one must cost
# seconds, not a task timeout.
METADATA_CONNECT_TIMEOUT=2
METADATA_MAX_TIME=4

SCRATCH=""

report() {
	if [ -n "$SCRATCH" ]; then
		rm -rf "$SCRATCH"
	fi
	echo
	echo "gcp-proof: rung 5b a container declaring no identity gets nothing: $VERDICT"
}
trap report EXIT

deny() {
	VERDICT="FAIL"
	echo "!!! gcp-proof: RUNG 5b FAILED — $*" >&2
	exit 1
}

echo "gcp-proof: rung 5b — a container declaring no identity gets no credential"

if [ -d "$CLOUD_ROOT" ]; then
	found=$(find "$CLOUD_ROOT" -type f 2>/dev/null | head -n "$FILES_LISTED_MAX" || true)
	[ -z "$found" ] || deny "this evaluator declared no workload_identities but holds
      credential files, so injection is NOT per-container and #313 A5's
      non-inheritance rule is broken:
$found"
	echo "  $CLOUD_ROOT exists but holds no files"
else
	echo "  no $CLOUD_ROOT directory at all"
fi

[ -z "${GOOGLE_APPLICATION_CREDENTIALS:-}" ] ||
	deny "GOOGLE_APPLICATION_CREDENTIALS is set to '${GOOGLE_APPLICATION_CREDENTIALS}' in a container that declared no identity"
echo "  GOOGLE_APPLICATION_CREDENTIALS is unset"

# Belt and braces, and only if an SDK is somehow on this PATH: no image here
# carries one, so this branch is a guard against a future image rather than the
# ambient check itself. That is the probe below.
if command -v gcloud >/dev/null 2>&1; then
	if gcloud auth print-access-token >/dev/null 2>&1; then
		deny "gcloud minted an access token with no declared identity — this container
      is reaching ambient credentials (a metadata server, or a mounted key)."
	fi
	echo "  gcloud is on this PATH and cannot mint a token from ambient credentials"
fi

# The ambient probe. A token here is a REAL finding — the container is wearing
# the node's identity — and an unanswered probe is not a refusal, so the two are
# reported apart and the second never claims to have proved the first.
AMBIENT="no metadata server was reachable, so ambient minting was NOT exercised"

if command -v curl >/dev/null 2>&1; then
	SCRATCH=$(mktemp -d)
	BODY="$SCRATCH/body"
	ERR="$SCRATCH/err"

	for host in $METADATA_HOSTS; do
		code=$(curl --silent --show-error \
			--connect-timeout "$METADATA_CONNECT_TIMEOUT" \
			--max-time "$METADATA_MAX_TIME" \
			--header 'Metadata-Flavor: Google' \
			--output "$BODY" --write-out '%{http_code}' \
			"http://$host$METADATA_PATH" 2>"$ERR") || code=000
		[ -n "$code" ] || code=000

		if [ "$code" = 000 ]; then
			echo "  no answer from http://$host$METADATA_PATH: $(head -n 1 "$ERR")"
			continue
		fi

		case "$(cat "$BODY")" in
		*'"access_token"'*)
			deny "the metadata server at $host MINTED AN ACCESS TOKEN (HTTP $code) for a
      container that declared no workload_identities. This container is wearing the
      NODE's own identity — per-container scoping (#313 A5) is not what bounds it,
      and nothing dispatcher-side prevents this."
			;;
		esac

		AMBIENT="a metadata server answered at $host with HTTP $code and minted nothing"
		break
	done
else
	AMBIENT="curl is absent, so the ambient probe did NOT run"
fi

echo "  $AMBIENT"

VERDICT="PASS — $AMBIENT"
exit 0
