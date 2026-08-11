#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT
mkdir -p "$TEST_DIR/bin" "$TEST_DIR/data"

cat >"$TEST_DIR/bin/nginx" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
trap 'exit 0' TERM INT
while true; do sleep 1; done
MOCK
cat >"$TEST_DIR/bin/fake-app" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
trap 'exit 0' TERM INT
while true; do sleep 1; done
MOCK
chmod +x "$TEST_DIR/bin/nginx" "$TEST_DIR/bin/fake-app"

printf 'database\n' >"$TEST_DIR/data/wwmtf.db"
printf 'wal\n' >"$TEST_DIR/data/wwmtf.db-wal"
printf 'shm\n' >"$TEST_DIR/data/wwmtf.db-shm"

PATH="$TEST_DIR/bin:$PATH" \
WWMTF_BACKUP_DATA_DIR="$TEST_DIR/data" \
WWMTF_APP_COMMAND="$TEST_DIR/bin/fake-app" \
WWMTF_SUPERVISOR_PID_FILE="$TEST_DIR/supervisor.pid" \
    "$ROOT_DIR/scripts/container-entrypoint.sh" &
supervisor=$!
trap 'kill -TERM "$supervisor" 2>/dev/null || true; wait "$supervisor" 2>/dev/null || true; rm -rf "$TEST_DIR"' EXIT

for _ in $(seq 1 50); do
    [[ -s "$TEST_DIR/supervisor.pid" ]] && break
    sleep 0.1
done
kill -USR1 "$(cat "$TEST_DIR/supervisor.pid")"
for _ in $(seq 1 50); do
    [[ -s "$TEST_DIR/data/backups/database.tar.gz" ]] && break
    sleep 0.1
done

tar -tzf "$TEST_DIR/data/backups/database.tar.gz" | sort >"$TEST_DIR/contents"
printf '%s\n' wwmtf.db wwmtf.db-shm wwmtf.db-wal | sort >"$TEST_DIR/expected"
diff -u "$TEST_DIR/expected" "$TEST_DIR/contents"
mkdir "$TEST_DIR/restored"
tar -C "$TEST_DIR/restored" -xzf "$TEST_DIR/data/backups/database.tar.gz"
cmp "$TEST_DIR/data/wwmtf.db" "$TEST_DIR/restored/wwmtf.db"
cmp "$TEST_DIR/data/wwmtf.db-wal" "$TEST_DIR/restored/wwmtf.db-wal"
cmp "$TEST_DIR/data/wwmtf.db-shm" "$TEST_DIR/restored/wwmtf.db-shm"

kill -TERM "$supervisor"
wait "$supervisor" || true
trap 'rm -rf "$TEST_DIR"' EXIT

echo "application-consistent database backup and restore test passed"
