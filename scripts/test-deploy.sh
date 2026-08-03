#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin"
cp /tmp/wwmtf-deploy-test-bin/flyctl "$TEST_DIR/bin/flyctl"
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
