#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin" "$TEST_DIR/repo/scripts" "$TEST_DIR/repo/infra/deploy"
cp "$ROOT_DIR/scripts/adopt-cloudflare-redirects.sh" "$TEST_DIR/repo/scripts/"

cat >"$TEST_DIR/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=
url=
while (($#)); do
    case "$1" in
        --output)
            output="$2"
            shift 2
            ;;
        --write-out|--header)
            shift 2
            ;;
        --silent|--show-error)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done
case "$url" in
    *'/zones?'*)
        printf '%s\n' '{"success":true,"result":[{"id":"zone-id"}]}' >"$output"
        printf '200'
        ;;
    *'/entrypoint')
        case "${MOCK_RULESET_SCENARIO:?}" in
            missing)
                printf '%s\n' '{"success":false,"errors":[{"message":"not found"}]}' >"$output"
                printf '404'
                ;;
            empty)
                printf '%s\n' '{"success":true,"result":{"id":"ruleset-id","rules":[]}}' >"$output"
                printf '200'
                ;;
            populated)
                printf '%s\n' '{"success":true,"result":{"id":"ruleset-id","rules":[{"description":"Existing redirect"}]}}' >"$output"
                printf '200'
                ;;
        esac
        ;;
    *)
        echo "unexpected URL: $url" >&2
        exit 1
        ;;
esac
EOF
chmod +x "$TEST_DIR/bin/curl"

cat >"$TEST_DIR/bin/tofu" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
    "-chdir=infra/deploy state list")
        if [[ "${MOCK_RULESET_SCENARIO:?}" == "managed" ]]; then
            printf '%s\n' 'cloudflare_ruleset.redirects[0]'
        else
            exit 1
        fi
        ;;
    "-chdir=infra/deploy import -input=false cloudflare_ruleset.redirects[0] zones/zone-id/ruleset-id")
        printf 'imported\n' >>"${MOCK_IMPORT_LOG:?}"
        ;;
    *)
        echo "unexpected tofu arguments: $*" >&2
        exit 1
        ;;
esac
EOF
chmod +x "$TEST_DIR/bin/tofu"

run_scenario() {
    local scenario="$1"
    (
        cd "$TEST_DIR/repo"
        PATH="$TEST_DIR/bin:$PATH" \
            MOCK_RULESET_SCENARIO="$scenario" \
            MOCK_IMPORT_LOG="$TEST_DIR/imports" \
            CLOUDFLARE_API_TOKEN=cloudflare-token \
            CLOUDFLARE_ACCOUNT_ID=account-id \
            ./scripts/adopt-cloudflare-redirects.sh
    )
}

: >"$TEST_DIR/imports"
run_scenario missing
[[ ! -s "$TEST_DIR/imports" ]]

run_scenario empty
grep -Fxq imported "$TEST_DIR/imports"

if run_scenario populated; then
    echo "Populated unmanaged ruleset was not rejected" >&2
    exit 1
fi

echo "Cloudflare redirect adoption tests passed"
