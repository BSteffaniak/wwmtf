#!/usr/bin/env bash
set -uo pipefail

backup_data_dir="${WWMTF_BACKUP_DATA_DIR:-/data}"
backup_dir="${backup_data_dir}/backups"
database_path="${backup_data_dir}/wwmtf.db"
supervisor_pid_file="${WWMTF_SUPERVISOR_PID_FILE:-/tmp/wwmtf-supervisor.pid}"
app_command="${WWMTF_APP_COMMAND:-/app/wwmtf serve}"
app_pid=
nginx_pid=

echo "$$" >"$supervisor_pid_file"

shutdown() {
    [[ -z "$app_pid" ]] || kill -TERM "$app_pid" 2>/dev/null || true
    [[ -z "$nginx_pid" ]] || kill -TERM "$nginx_pid" 2>/dev/null || true
}

consistent_backup() {
    [[ -n "$app_pid" ]] || return 1
    mkdir -p "$backup_dir"
    kill -STOP "$app_pid"
    local files=("$database_path")
    [[ -f "${database_path}-wal" ]] && files+=("${database_path}-wal")
    [[ -f "${database_path}-shm" ]] && files+=("${database_path}-shm")
    local temp="${backup_dir}/database.tar.gz.tmp"
    if tar -C "$backup_data_dir" -czf "$temp" "${files[@]#${backup_data_dir}/}"; then
        mv "$temp" "${backup_dir}/database.tar.gz"
    else
        rm -f "$temp"
    fi
    kill -CONT "$app_pid"
}

trap shutdown TERM INT
trap consistent_backup USR1

sh -c "$app_command" &
app_pid=$!
nginx -g 'daemon off;' &
nginx_pid=$!

while kill -0 "$app_pid" 2>/dev/null && kill -0 "$nginx_pid" 2>/dev/null; do
    sleep 1 || true
done

shutdown
wait "$app_pid" 2>/dev/null || true
wait "$nginx_pid" 2>/dev/null || true
exit 1
