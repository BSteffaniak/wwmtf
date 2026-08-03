#!/usr/bin/env bash
set -euo pipefail

ZONE_NAME="${CLOUDFLARE_ZONE_NAME:-hyperchad.dev}"
PHASE="http_request_dynamic_redirect"
OUTPUT="${CLOUDFLARE_REDIRECT_INVENTORY_OUTPUT:-artifacts/cloudflare-redirect-ruleset.json}"

require_env() {
    local name="$1"
    [[ -n "${!name:-}" ]] || {
        echo "${name} is required" >&2
        exit 1
    }
}

require_env CLOUDFLARE_API_TOKEN
require_env CLOUDFLARE_ACCOUNT_ID

for command in curl jq; do
    command -v "$command" >/dev/null || {
        echo "${command} is required" >&2
        exit 1
    }
done

cloudflare_get() {
    curl --fail --silent --show-error \
        --header "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
        "$1"
}

zone_response="$(cloudflare_get \
    "https://api.cloudflare.com/client/v4/zones?account.id=${CLOUDFLARE_ACCOUNT_ID}&name=${ZONE_NAME}")"
zone_id="$(jq -r '.result | if length == 1 then .[0].id else empty end' <<<"$zone_response")"
[[ -n "$zone_id" ]] || {
    echo "Expected exactly one Cloudflare zone named ${ZONE_NAME}" >&2
    exit 1
}

ruleset_response="$(cloudflare_get \
    "https://api.cloudflare.com/client/v4/zones/${zone_id}/rulesets/phases/${PHASE}/entrypoint")"

mkdir -p "$(dirname "$OUTPUT")"
jq -e '
    select(.success == true)
    | .result
    | {
        id,
        name,
        description,
        kind,
        phase,
        rules
    }
' <<<"$ruleset_response" >"$OUTPUT"

rule_count="$(jq -r '.rules | length' "$OUTPUT")"
echo "Exported ${rule_count} redirect rules to ${OUTPUT}"
