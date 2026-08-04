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
set -eu

CLOUD_ROOT="${CHUG_CLOUD_ROOT:-/chuggernaut/cloud}"
FILES_LISTED_MAX=20
VERDICT="FAIL — the check did not finish"

report() {
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

# Belt and braces. With no ADC anywhere, gcloud must not be able to mint a token
# from AMBIENT credentials — on a node with its own metadata server this is the
# check that would catch a container reaching the NODE's identity, which no
# amount of per-container discipline in the dispatcher would prevent.
if command -v gcloud >/dev/null 2>&1; then
	if gcloud auth print-access-token >/dev/null 2>&1; then
		deny "gcloud minted an access token with no declared identity — this container
      is reaching ambient credentials (a metadata server, or a mounted key)."
	fi
	echo "  gcloud cannot mint a token from ambient credentials"
else
	echo "  gcloud is absent, so the ambient-credential check did not run"
fi

VERDICT="PASS"
exit 0
