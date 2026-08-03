#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOTSTRAP_DIR="${ROOT_DIR}/infra/bootstrap"
DEPLOY_DIR="${ROOT_DIR}/infra/deploy"

require_env() {
    local name="$1"
    [[ -n "${!name:-}" ]] || {
        echo "${name} is required" >&2
        exit 1
    }
}

require_env TF_VAR_cloudflare_api_token
require_env TF_VAR_cloudflare_account_id
require_env TF_VAR_state_bucket_name
require_env TF_VAR_backup_bucket_name

command -v tofu >/dev/null || {
    echo "tofu is required" >&2
    exit 1
}

tofu -chdir="$BOOTSTRAP_DIR" init -input=false
tofu -chdir="$BOOTSTRAP_DIR" plan -input=false -out=bootstrap.tfplan

echo
echo "Review the bootstrap plan above. It creates two private R2 buckets and retention policies."
read -r -p "Apply this bootstrap plan? [y/N] " answer
[[ "$answer" == "y" || "$answer" == "Y" ]] || {
    echo "Bootstrap cancelled"
    exit 0
}

tofu -chdir="$BOOTSTRAP_DIR" apply -input=false bootstrap.tfplan
tofu -chdir="$BOOTSTRAP_DIR" output -raw backend_hcl >"${DEPLOY_DIR}/backend.hcl"

echo
echo "Bootstrap complete. Non-secret backend configuration was written to infra/deploy/backend.hcl."
echo "The bootstrap state remains local at infra/bootstrap/terraform.tfstate and is gitignored."
echo "Store an encrypted offline copy before removing it. It is needed to update or destroy the buckets."
echo
echo "Next, create two separate bucket-scoped R2 API tokens in the Cloudflare dashboard:"
echo "  1. State token: Object Read & Write for ${TF_VAR_state_bucket_name} only."
echo "  2. Backup token: Object Read & Write for ${TF_VAR_backup_bucket_name} only."
echo "Do not reuse the Cloudflare management token or either bucket token for the other purpose."
