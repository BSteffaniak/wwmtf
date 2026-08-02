#!/usr/bin/env bash
set -uo pipefail

backup_dir=/data/backups
app_pid=
nginx_pid=

echo "$$" >/tmp/words-with-spouses-supervisor.pid

shutdown() {
    [[ -z "$app_pid" ]] || kill -TERM "$app_pid" 2>/dev/null || true
    [[ -z "$nginx_pid" ]] || kill -TERM "$nginx_pid" 2>/dev/null || true
}

consistent_backup() {
    [[ -n "$app_pid" ]] || return 1
    mkdir -p "$backup_dir"
    kill -STOP "$app_pid"
    local files=(/data/words-with-spouses.db)
    [[ -f /data/words-with-spouses.db-wal ]] && files+=(/data/words-with-spouses.db-wal)
    [[ -f /data/words-with-spouses.db-shm ]] && files+=(/data/words-with-spouses.db-shm)
    local temp="${backup_dir}/database.tar.gz.tmp"
    if tar -C /data -czf "$temp" "${files[@]#/data/}"; then
        mv "$temp" "${backup_dir}/database.tar.gz"
    else
        rm -f "$temp"
    fi
    kill -CONT "$app_pid"
}

trap shutdown TERM INT
trap consistent_backup USR1

/app/words-with-spouses serve &
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
