#!/usr/bin/env bash
set -euo pipefail

ZONE_NAME="${CLOUDFLARE_ZONE_NAME:-hyperchad.dev}"
PHASE="http_request_dynamic_redirect"
EXPECTED_RULESET_ID="67478f03550248edb045997f49ded3ce"
EXPECTED_RULES_SHA256="b4ab8c12aef7a1d787eaf1497be3c89173ffedacd3623fff8769300c3fd0a3a8"
DEPLOY_DIR="infra/deploy"
RESOURCE_ADDRESS='cloudflare_ruleset.redirects[0]'

require_env() {
    local name="$1"
    [[ -n "${!name:-}" ]] || {
        echo "${name} is required" >&2
        exit 1
    }
}

require_env CLOUDFLARE_API_TOKEN
require_env CLOUDFLARE_ACCOUNT_ID

for command in curl jq sha256sum tofu; do
    command -v "$command" >/dev/null || {
        echo "${command} is required" >&2
        exit 1
    }
done

cloudflare_get() {
    local url="$1"
    local output_file="$2"
    curl --silent --show-error \
        --output "$output_file" \
        --write-out '%{http_code}' \
        --header "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
        "$url"
}

zone_file="$(mktemp)"
ruleset_file="$(mktemp)"
trap 'rm -f "$zone_file" "$ruleset_file"' EXIT

zone_status="$(cloudflare_get \
    "https://api.cloudflare.com/client/v4/zones?account.id=${CLOUDFLARE_ACCOUNT_ID}&name=${ZONE_NAME}" \
    "$zone_file")"
if [[ "$zone_status" != "200" ]] || ! jq -e '.success == true' "$zone_file" >/dev/null; then
    jq -r '.errors[]?.message' "$zone_file" >&2
    echo "Unable to resolve Cloudflare zone ${ZONE_NAME} (HTTP ${zone_status})" >&2
    exit 1
fi

zone_id="$(jq -r '.result | if length == 1 then .[0].id else empty end' "$zone_file")"
[[ -n "$zone_id" ]] || {
    echo "Expected exactly one Cloudflare zone named ${ZONE_NAME}" >&2
    exit 1
}

if tofu -chdir="$DEPLOY_DIR" state list 2>/dev/null | grep -Fxq "$RESOURCE_ADDRESS"; then
    echo "OpenTofu already manages the ${PHASE} entry-point ruleset"
    exit 0
fi

ruleset_status="$(cloudflare_get \
    "https://api.cloudflare.com/client/v4/zones/${zone_id}/rulesets/phases/${PHASE}/entrypoint" \
    "$ruleset_file")"
case "$ruleset_status" in
    404)
        echo "No existing ${PHASE} entry-point ruleset; OpenTofu will create it"
        ;;
    200)
        if ! jq -e '.success == true' "$ruleset_file" >/dev/null; then
            jq -r '.errors[]?.message' "$ruleset_file" >&2
            exit 1
        fi
        ruleset_id="$(jq -r '.result.id // empty' "$ruleset_file")"
        rule_count="$(jq -r '.result.rules | length' "$ruleset_file")"
        [[ -n "$ruleset_id" ]] || {
            echo "Existing redirect ruleset has no ID" >&2
            exit 1
        }
        if [[ "$rule_count" != "0" ]]; then
            actual_rules_sha256="$(
                jq -S -c \
                    '[.result.rules[] | {action,action_parameters,description,enabled,expression,ref}] | sort_by(.ref)' \
                    "$ruleset_file" \
                    | sha256sum \
                    | cut -d ' ' -f 1
            )"
            if [[ "$ruleset_id" != "$EXPECTED_RULESET_ID" || "$actual_rules_sha256" != "$EXPECTED_RULES_SHA256" ]]; then
                echo "Refusing to replace an unknown unmanaged shared Cloudflare redirect ruleset." >&2
                echo "Ruleset ID: ${ruleset_id}; existing rules: ${rule_count}." >&2
                echo "Expected reviewed ruleset ID: ${EXPECTED_RULESET_ID}." >&2
                jq -r '.result.rules[] | "- " + (.description // .ref // .id)' "$ruleset_file" >&2
                exit 1
            fi
            echo "Existing redirect ruleset matches the reviewed planning-poker baseline"
        fi
        tofu -chdir="$DEPLOY_DIR" import -input=false \
            "$RESOURCE_ADDRESS" \
            "zones/${zone_id}/${ruleset_id}"
        echo "Adopted reviewed ${PHASE} entry-point ruleset ${ruleset_id}"
        ;;
    *)
        jq -r '.errors[]?.message' "$ruleset_file" >&2
        echo "Unable to inspect ${PHASE} entry-point ruleset (HTTP ${ruleset_status})" >&2
        exit 1
        ;;
esac
