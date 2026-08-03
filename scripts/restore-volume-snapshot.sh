#!/usr/bin/env bash
set -euo pipefail

: "${SNAPSHOT_ID:?Set SNAPSHOT_ID to a Fly volume snapshot ID}"

APP_NAME="${FLY_APP_NAME:-wwmtf}"
REGION="${FLY_REGION:-iad}"
VOLUME_SIZE_GB="${FLY_VOLUME_SIZE_GB:-1}"
RESTORE_VOLUME_NAME="${RESTORE_VOLUME_NAME:-wwmtf_restore_$(date -u +%Y%m%d%H%M%S)}"

flyctl volumes create "$RESTORE_VOLUME_NAME" \
    --app "$APP_NAME" \
    --region "$REGION" \
    --size "$VOLUME_SIZE_GB" \
    --snapshot-id "$SNAPSHOT_ID" \
    --snapshot-retention 60 \
    --scheduled-snapshots \
    --yes

cat <<EOF
Created restored volume ${RESTORE_VOLUME_NAME}.

Do not attach it to production immediately. To drill recovery:
1. Stop the production Machine.
2. Clone or create a temporary Machine configured to mount ${RESTORE_VOLUME_NAME} at /data.
3. start the application and verify /health/ready, accounts, sessions, games, journals, and a normal turn;
4. rebuild projections and compare expected state;
5. remove the temporary Machine and restored volume after retaining drill evidence.

Fly Volumes cannot be mounted by two Machines simultaneously.
EOF
