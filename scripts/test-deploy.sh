#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin"
cat >"$TEST_DIR/bin/flyctl" <<'EOF'
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
    *)
        echo "unexpected flyctl arguments: $*" >&2
        exit 1
        ;;
esac
EOF
chmod +x "$TEST_DIR/bin/flyctl"
printf 'stopped\n' >"$TEST_DIR/state"
: >"$TEST_DIR/log"

PATH="$TEST_DIR/bin:$PATH" \
    MOCK_FLY_STATE_FILE="$TEST_DIR/state" \
    MOCK_FLY_LOG_FILE="$TEST_DIR/log" \
    "$ROOT_DIR/scripts/deploy.sh" ensure-started machine-1

[[ "$(cat "$TEST_DIR/state")" == "started" ]]
[[ "$(grep -c '^start$' "$TEST_DIR/log")" -eq 1 ]]

echo "deployment restart tests passed"
