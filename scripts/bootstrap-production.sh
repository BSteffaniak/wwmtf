#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${FLY_APP_NAME:-words-with-spouses}"
STATE_BUCKET="${TOFU_STATE_BUCKET:-words-with-spouses-opentofu-state}"
STATE_KEY="${TOFU_STATE_KEY:-words-with-spouses/production.tfstate}"
STATE_ENDPOINT="${TOFU_STATE_S3_ENDPOINT:-https://${CLOUDFLARE_ACCOUNT_ID:?CLOUDFLARE_ACCOUNT_ID is required}.r2.cloudflarestorage.com}"
DEPLOY_DIR="infra/deploy"

require_env() {
    local name="$1"
    [[ -n "${!name:-}" ]] || {
        echo "${name} is required" >&2
        exit 1
    }
}

require_env CLOUDFLARE_API_TOKEN
require_env CLOUDFLARE_ACCOUNT_ID
require_env FLY_API_TOKEN
require_env FLY_ORG
require_env AWS_ACCESS_KEY_ID
require_env AWS_SECRET_ACCESS_KEY
require_env TF_VAR_state_encryption_passphrase

for command in curl flyctl grep jq tofu; do
    command -v "$command" >/dev/null || {
        echo "${command} is required" >&2
        exit 1
    }
done

export TF_VAR_cloudflare_api_token="$CLOUDFLARE_API_TOKEN"
export TF_VAR_cloudflare_account_id="$CLOUDFLARE_ACCOUNT_ID"
export TF_VAR_state_bucket_name="$STATE_BUCKET"
export TOFU_STATE_BUCKET="$STATE_BUCKET"
export TOFU_STATE_S3_ENDPOINT="$STATE_ENDPOINT"

cloudflare_api() {
    curl --fail --silent --show-error \
        --header "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
        --header "Content-Type: application/json" \
        "$@"
}

ensure_state_bucket() {
    local url="https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/r2/buckets/${STATE_BUCKET}"
    local status
    local response_file
    response_file="$(mktemp)"
    status="$(curl --silent --show-error \
        --output "$response_file" \
        --write-out '%{http_code}' \
        --header "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
        "$url")"
    if [[ "$status" == "200" ]] && jq -e '.success == true' "$response_file" >/dev/null; then
        rm -f "$response_file"
        echo "R2 state bucket ${STATE_BUCKET} already exists"
        return
    fi
    if [[ "$status" != "404" ]]; then
        jq -r '.errors[]?.message' "$response_file" >&2
        rm -f "$response_file"
        echo "Unable to inspect R2 state bucket (HTTP ${status})" >&2
        exit 1
    fi
    rm -f "$response_file"

    local response
    response="$(cloudflare_api \
        --request POST \
        --data "$(jq -n --arg name "$STATE_BUCKET" '{name: $name, locationHint: "enam", storageClass: "Standard"}')" \
        "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/r2/buckets")"
    if ! jq -e '.success == true' <<<"$response" >/dev/null; then
        jq -r '.errors[]?.message' <<<"$response" >&2
        exit 1
    fi
    echo "Created R2 state bucket ${STATE_BUCKET}"
}

write_backend_config() {
    cat >"${DEPLOY_DIR}/backend.hcl" <<EOF
bucket                      = "${STATE_BUCKET}"
key                         = "${STATE_KEY}"
region                      = "auto"
endpoint                    = "${STATE_ENDPOINT}"
use_lockfile                = true
skip_credentials_validation = true
skip_region_validation      = true
skip_requesting_account_id  = true
skip_s3_checksum            = true
skip_metadata_api_check     = true
EOF
}

ensure_state_bucket
write_backend_config

FLY_APP_NAME="$APP_NAME" FLY_ORG="$FLY_ORG" ./scripts/deploy.sh bootstrap
environment_file="$(mktemp)"
trap 'rm -f "$environment_file"' EXIT
PRODUCTION_SHELL_ENV="$environment_file" ./scripts/prepare-production-infra.sh
set -a
# This file contains shell-safe single-line values discovered from trusted APIs.
source "$environment_file"
set +a

tofu -chdir="$DEPLOY_DIR" init -input=false -backend-config=backend.hcl

if ! tofu -chdir="$DEPLOY_DIR" state list | grep -Fxq cloudflare_r2_bucket.state; then
    tofu -chdir="$DEPLOY_DIR" import -input=false \
        cloudflare_r2_bucket.state \
        "${CLOUDFLARE_ACCOUNT_ID}/${STATE_BUCKET}/default"
fi

tofu -chdir="$DEPLOY_DIR" plan -input=false -out=tfplan
tofu -chdir="$DEPLOY_DIR" apply -input=false -auto-approve tfplan

FLY_APP_NAME="$APP_NAME" ./scripts/deploy.sh deploy
FLY_APP_NAME="$APP_NAME" ./scripts/deploy.sh smoke

./scripts/archive-opentofu-state.sh archive

echo "Production bootstrap completed successfully"
