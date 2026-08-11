#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin"
cat >"$TEST_DIR/bin/flyctl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

STATE_FILE="${MOCK_FLY_STATE_FILE:?MOCK_FLY_STATE_FILE is required}"
LOG_FILE="${MOCK_FLY_LOG_FILE:?MOCK_FLY_LOG_FILE is required}"

case "$*" in
    "machine list --app wwmtf --json")
        state="$(cat "$STATE_FILE")"
        if [[ "$state" == "starting" ]]; then
            state=started
            printf '%s\n' "$state" >"$STATE_FILE"
        fi
        printf '[{"id":"machine-1","state":"%s"}]\n' "$state"
        ;;
    "machine start --app wwmtf machine-1")
        printf 'start\n' >>"$LOG_FILE"
        printf 'starting\n' >"$STATE_FILE"
        echo "machine still attempting to start" >&2
        exit 1
        ;;
    "machine cordon --app wwmtf machine-1")
        printf 'cordon\n' >>"$LOG_FILE"
        ;;
    "machine uncordon --app wwmtf machine-1")
        printf 'uncordon\n' >>"$LOG_FILE"
        ;;
    "machine stop --app wwmtf --signal SIGTERM --timeout 30 machine-1")
        printf 'stop\n' >>"$LOG_FILE"
        printf 'stopped\n' >"$STATE_FILE"
        ;;
    "volumes list --app wwmtf --json")
        printf '[{"id":"volume-1","name":"wwmtf_data"}]\n'
        ;;
    "ssh console --app wwmtf --command sh -c 'kill -USR1 \$(cat /tmp/wwmtf-supervisor.pid)'")
        printf 'backup\n' >>"$LOG_FILE"
        ;;
    "ssh console --app wwmtf --command test -s /data/backups/database.tar.gz")
        ;;
    "secrets import --app wwmtf --stage")
        cat >"${MOCK_FLY_SECRET_FILE:?MOCK_FLY_SECRET_FILE is required}"
        printf 'secrets-imported\n' >>"$LOG_FILE"
        ;;
    "deploy --app wwmtf --ha=false --strategy immediate --wait-timeout 10m --build-arg WWMTF_RELEASE=test-release")
        printf 'deploy\n' >>"$LOG_FILE"
        ;;
    *)
        echo "unexpected flyctl arguments: $*" >&2
        exit 1
        ;;
esac
MOCK
cat >"$TEST_DIR/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "${*: -1}" in
    */snapshots)
        if [[ "${MOCK_SNAPSHOT_FAIL:-false}" == "true" ]]; then
            exit 22
        fi
        printf '{"snapshot":"created"}\n'
        ;;
    */health/live) printf 'live\n' ;;
    */health/ready) printf 'ready\n' ;;
    */login) printf '<a href="/auth/google/start">Continue with Google</a>\n' ;;
    */auth/google/start) printf 'HTTP/1.1 303 See Other\r\nLocation: https://accounts.google.com/o/oauth2/v2/auth\r\n\r\n' ;;
    */) printf 'home\n' ;;
    *) echo "unexpected curl arguments: $*" >&2; exit 1 ;;
esac
MOCK
chmod +x "$TEST_DIR/bin/flyctl" "$TEST_DIR/bin/curl"
printf 'stopped\n' >"$TEST_DIR/state"
: >"$TEST_DIR/log"
: >"$TEST_DIR/secrets"

PATH="$TEST_DIR/bin:$PATH" \
    MOCK_FLY_STATE_FILE="$TEST_DIR/state" \
    MOCK_FLY_LOG_FILE="$TEST_DIR/log" \
    MOCK_FLY_SECRET_FILE="$TEST_DIR/secrets" \
    "$ROOT_DIR/scripts/deploy.sh" ensure-started machine-1

[[ "$(cat "$TEST_DIR/state")" == "started" ]]
[[ "$(grep -c '^start$' "$TEST_DIR/log")" -eq 1 ]]

PATH="$TEST_DIR/bin:$PATH" \
    MOCK_FLY_STATE_FILE="$TEST_DIR/state" \
    MOCK_FLY_LOG_FILE="$TEST_DIR/log" \
    MOCK_FLY_SECRET_FILE="$TEST_DIR/secrets" \
    FLY_API_TOKEN=test-token \
    "$ROOT_DIR/scripts/deploy.sh" snapshot

[[ "$(cat "$TEST_DIR/state")" == "started" ]]
[[ "$(grep -c '^cordon$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^backup$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^stop$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^start$' "$TEST_DIR/log")" -eq 2 ]]
[[ "$(grep -c '^uncordon$' "$TEST_DIR/log")" -eq 1 ]]

: >"$TEST_DIR/log"
printf 'started\n' >"$TEST_DIR/state"
if PATH="$TEST_DIR/bin:$PATH" \
    MOCK_FLY_STATE_FILE="$TEST_DIR/state" \
    MOCK_FLY_LOG_FILE="$TEST_DIR/log" \
    MOCK_FLY_SECRET_FILE="$TEST_DIR/secrets" \
    MOCK_SNAPSHOT_FAIL=true \
    FLY_API_TOKEN=test-token \
    "$ROOT_DIR/scripts/deploy.sh" snapshot 2>/dev/null; then
    echo "snapshot unexpectedly succeeded when the Fly API failed" >&2
    exit 1
fi
[[ "$(cat "$TEST_DIR/state")" == "started" ]]
[[ "$(grep -c '^cordon$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^stop$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^start$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^uncordon$' "$TEST_DIR/log")" -eq 1 ]]

PATH="$TEST_DIR/bin:$PATH" \
    MOCK_FLY_STATE_FILE="$TEST_DIR/state" \
    MOCK_FLY_LOG_FILE="$TEST_DIR/log" \
    MOCK_FLY_SECRET_FILE="$TEST_DIR/secrets" \
    GITHUB_SHA=test-release \
    WWMTF_GOOGLE_CLIENT_ID=google-client-id \
    WWMTF_GOOGLE_CLIENT_SECRET=google-client-secret \
    "$ROOT_DIR/scripts/deploy.sh" deploy

[[ "$(grep -c '^secrets-imported$' "$TEST_DIR/log")" -eq 1 ]]
[[ "$(grep -c '^deploy$' "$TEST_DIR/log")" -eq 1 ]]
grep -Fx 'WWMTF_GOOGLE_CLIENT_ID=google-client-id' "$TEST_DIR/secrets" >/dev/null
grep -Fx 'WWMTF_GOOGLE_CLIENT_SECRET=google-client-secret' "$TEST_DIR/secrets" >/dev/null

if PATH="$TEST_DIR/bin:$PATH" \
    MOCK_FLY_STATE_FILE="$TEST_DIR/state" \
    MOCK_FLY_LOG_FILE="$TEST_DIR/log" \
    MOCK_FLY_SECRET_FILE="$TEST_DIR/secrets" \
    GITHUB_SHA=test-release \
    WWMTF_GOOGLE_CLIENT_ID=google-client-id \
    "$ROOT_DIR/scripts/deploy.sh" deploy 2>/dev/null; then
    echo "deploy unexpectedly accepted partial Google credentials" >&2
    exit 1
fi

grep -F 'map $request_method:$http_sec_fetch_site $fetch_site_allowed' "$ROOT_DIR/config/nginx.conf" >/dev/null
grep -F '~^(GET|HEAD|OPTIONS): 1;' "$ROOT_DIR/config/nginx.conf" >/dev/null
grep -F "'POST:same-origin' 1;" "$ROOT_DIR/config/nginx.conf" >/dev/null
grep -F 'if ($fetch_site_allowed = 0)' "$ROOT_DIR/config/nginx.conf" >/dev/null
if grep -Fq '$http_origin $origin_allowed' "$ROOT_DIR/config/nginx.conf"; then
    echo "nginx still requires the optional Origin header for native form POSTs" >&2
    exit 1
fi

echo "deployment restart, secret, and Google smoke tests passed"
