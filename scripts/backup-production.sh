#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${FLY_APP_NAME:-words-with-spouses}"
VOLUME_NAME="${FLY_VOLUME_NAME:-wwmtf_data}"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

volume_id="$(flyctl volumes list --app "$APP_NAME" --json \
    | jq -r --arg name "$VOLUME_NAME" '[.[] | select((.Name // .name) == $name)][0].ID // [ .[] | select((.name // .Name) == $name)][0].id // empty')"
[[ -n "$volume_id" ]] || { echo "Volume ${VOLUME_NAME} does not exist" >&2; exit 1; }

machines=()
while IFS= read -r machine; do
    [[ -z "$machine" ]] || machines+=("$machine")
done < <(flyctl machine list --app "$APP_NAME" --json | jq -r '.[].id')
((${#machines[@]} == 1)) || { echo "Expected exactly one production Machine" >&2; exit 1; }

flyctl ssh console --app "$APP_NAME" --command "sh -c 'kill -USR1 \$(cat /tmp/words-with-spouses-supervisor.pid)'"
for _ in $(seq 1 30); do
    if flyctl ssh console --app "$APP_NAME" --command "test -s /data/backups/database.tar.gz"; then
        break
    fi
    sleep 1
done
flyctl ssh console --app "$APP_NAME" --command "test -s /data/backups/database.tar.gz"

backup_dir="$(mktemp -d)"
trap 'rm -rf "$backup_dir"' EXIT
archive="${backup_dir}/words-with-spouses-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"
flyctl ssh sftp get --app "$APP_NAME" /data/backups/database.tar.gz "$archive"

if [[ -n "${BACKUP_AGE_RECIPIENT:-}" ]]; then
    age -r "$BACKUP_AGE_RECIPIENT" -o "${archive}.age" "$archive"
    archive="${archive}.age"
fi

case "${BACKUP_DESTINATION:-}" in
    s3://*)
        [[ -n "${BACKUP_AGE_RECIPIENT:-}" ]] || {
            echo "BACKUP_AGE_RECIPIENT is required for remote backups" >&2
            exit 1
        }
        aws s3 cp "$archive" "${BACKUP_DESTINATION%/}/$(basename "$archive")"
        ;;
    "")
        output="${BACKUP_OUTPUT:-$(pwd)/$(basename "$archive")}"
        cp "$archive" "$output"
        echo "Backup written to ${output}"
        ;;
    *)
        echo "BACKUP_DESTINATION must be an s3:// URL when set" >&2
        exit 1
        ;;
esac
