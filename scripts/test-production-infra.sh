#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin" "$TEST_DIR/repo/infra/deploy" "$TEST_DIR/repo/scripts"
cp "$ROOT_DIR/scripts/prepare-production-infra.sh" "$TEST_DIR/repo/scripts/"

cat >"$TEST_DIR/repo/scripts/deploy.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "certificate-dns" ]]
printf '%s\n' '{"fly_ipv6_address":"2001:db8::1","fly_ownership_txt":"ownership-test"}'
EOF
chmod +x "$TEST_DIR/repo/scripts/deploy.sh"

cat >"$TEST_DIR/bin/flyctl" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$TEST_DIR/bin/flyctl"

GITHUB_ENV_FILE="$TEST_DIR/github-env"
SHELL_ENV_FILE="$TEST_DIR/shell-env"
: >"$GITHUB_ENV_FILE"
: >"$SHELL_ENV_FILE"

(
    cd "$TEST_DIR/repo"
    PATH="$TEST_DIR/bin:$PATH" \
        GITHUB_ENV="$GITHUB_ENV_FILE" \
        PRODUCTION_SHELL_ENV="$SHELL_ENV_FILE" \
        CLOUDFLARE_API_TOKEN=cloudflare-token \
        CLOUDFLARE_ACCOUNT_ID=account-id \
        FLY_API_TOKEN=fly-token \
        AWS_ACCESS_KEY_ID=access-key \
        AWS_SECRET_ACCESS_KEY=secret-key \
        TF_VAR_state_encryption_passphrase=validation-only-passphrase \
        ./scripts/prepare-production-infra.sh
)

grep -Fxq 'TF_VAR_fly_ipv6_address=2001:db8::1' "$SHELL_ENV_FILE"
grep -Fxq 'TF_VAR_fly_ownership_txt=ownership-test' "$SHELL_ENV_FILE"
[[ ! -s "$GITHUB_ENV_FILE" ]]
grep -Fq 'bucket                      = "wwmtf-opentofu-state"' \
    "$TEST_DIR/repo/infra/deploy/backend.hcl"

echo "production infrastructure preparation tests passed"
