#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${FLY_APP_NAME:-words-with-spouses}"
STATE_BUCKET="${TOFU_STATE_BUCKET:-words-with-spouses-opentofu-state}"
STATE_KEY="${TOFU_STATE_KEY:-words-with-spouses/production.tfstate}"
STATE_ENDPOINT="${TOFU_STATE_S3_ENDPOINT:-https://${CLOUDFLARE_ACCOUNT_ID:?CLOUDFLARE_ACCOUNT_ID is required}.r2.cloudflarestorage.com}"
DEPLOY_DIR="infra/deploy"
GITHUB_ENV_FILE="${GITHUB_ENV:-}"
SHELL_ENV_FILE="${PRODUCTION_SHELL_ENV:-}"

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
require_env AWS_ACCESS_KEY_ID
require_env AWS_SECRET_ACCESS_KEY
require_env TF_VAR_state_encryption_passphrase

for command in flyctl jq; do
    command -v "$command" >/dev/null || {
        echo "${command} is required" >&2
        exit 1
    }
done

fly_values="$(FLY_APP_NAME="$APP_NAME" ./scripts/deploy.sh certificate-dns)"
fly_ipv6_address="$(jq -r '.fly_ipv6_address' <<<"$fly_values")"
fly_ownership_txt="$(jq -r '.fly_ownership_txt' <<<"$fly_values")"

export TF_VAR_cloudflare_api_token="$CLOUDFLARE_API_TOKEN"
export TF_VAR_cloudflare_account_id="$CLOUDFLARE_ACCOUNT_ID"
export TF_VAR_fly_ipv6_address="$fly_ipv6_address"
export TF_VAR_fly_ownership_txt="$fly_ownership_txt"
export TF_VAR_state_bucket_name="$STATE_BUCKET"
export TOFU_STATE_BUCKET="$STATE_BUCKET"
export TOFU_STATE_S3_ENDPOINT="$STATE_ENDPOINT"

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

if [[ -n "$GITHUB_ENV_FILE" || -n "$SHELL_ENV_FILE" ]]; then
    environment_file="${GITHUB_ENV_FILE:-$SHELL_ENV_FILE}"
    {
        printf 'TF_VAR_cloudflare_api_token=%s\n' "$CLOUDFLARE_API_TOKEN"
        printf 'TF_VAR_cloudflare_account_id=%s\n' "$CLOUDFLARE_ACCOUNT_ID"
        printf 'TF_VAR_fly_ipv6_address=%s\n' "$fly_ipv6_address"
        printf 'TF_VAR_fly_ownership_txt=%s\n' "$fly_ownership_txt"
        printf 'TF_VAR_state_bucket_name=%s\n' "$STATE_BUCKET"
        printf 'TOFU_STATE_BUCKET=%s\n' "$STATE_BUCKET"
        printf 'TOFU_STATE_S3_ENDPOINT=%s\n' "$STATE_ENDPOINT"
    } >>"$environment_file"
fi
