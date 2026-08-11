#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${FLY_APP_NAME:-wwmtf}"
FLY_ORG="${FLY_ORG:-personal}"
FLY_REGION="${FLY_REGION:-iad}"
VOLUME_NAME="${FLY_VOLUME_NAME:-wwmtf_data}"
VOLUME_SIZE_GB="${FLY_VOLUME_SIZE_GB:-1}"
SNAPSHOT_RETENTION_DAYS="${FLY_SNAPSHOT_RETENTION_DAYS:-60}"
PUBLIC_URL="${WWMTF_PUBLIC_BASE_URL:-https://wwmtf.hyperchad.dev}"

fly() {
    flyctl "$@"
}

app_exists() {
    fly apps list --json | jq -e --arg app "$APP_NAME" '.[] | select((.Name // .name) == $app)' >/dev/null
}

volume_id() {
    fly volumes list --app "$APP_NAME" --json \
        | jq -r --arg name "$VOLUME_NAME" '[.[] | select((.Name // .name) == $name)][0].ID // [ .[] | select((.name // .Name) == $name)][0].id // empty'
}

fly_api_token() {
    printf '%s' "${FLY_API_TOKEN:-$(fly auth token --quiet)}"
}

machine_config() {
    local machine_id="$1"
    curl --fail --silent --show-error \
        --header "Authorization: Bearer $(fly_api_token)" \
        "https://api.machines.dev/v1/apps/${APP_NAME}/machines/${machine_id}" \
        | jq '.config'
}

update_machine_config() {
    local machine_id="$1"
    local config_file="$2"
    jq -n --slurpfile config "$config_file" \
        '{config: $config[0], skip_launch: true}' \
        | curl --fail --silent --show-error \
            --request POST \
            --header "Authorization: Bearer $(fly_api_token)" \
            --header "Content-Type: application/json" \
            --data-binary @- \
            "https://api.machines.dev/v1/apps/${APP_NAME}/machines/${machine_id}"
}

create_volume_snapshot() {
    local volume="$1"
    curl --fail --silent --show-error \
        --request POST \
        --header "Authorization: Bearer $(fly_api_token)" \
        --header "Content-Type: application/json" \
        "https://api.machines.dev/v1/apps/${APP_NAME}/volumes/${volume}/snapshots"
}

machine_ids() {
    fly machine list --app "$APP_NAME" --json | jq -r '.[].id'
}

machine_state() {
    local machine_id="$1"
    fly machine list --app "$APP_NAME" --json \
        | jq -r --arg id "$machine_id" '.[] | select(.id == $id) | .state'
}

ensure_machine_started() {
    local machine_id="$1"
    local state

    for _ in $(seq 1 30); do
        state="$(machine_state "$machine_id")"
        if [[ "$state" == "started" ]]; then
            return
        fi
        if [[ "$state" == "stopped" ]]; then
            fly machine start --app "$APP_NAME" "$machine_id" || true
        fi
        sleep 2
    done

    state="$(machine_state "$machine_id")"
    [[ "$state" == "started" ]] || {
        echo "Machine ${machine_id} did not reach started state; current state: ${state:-missing}" >&2
        return 1
    }
}

restart_machine_best_effort() {
    ensure_machine_started "$1" >/dev/null 2>&1 || true
}

restore_machine_after_snapshot_best_effort() {
    local machine_id="$1"
    local config_file="$2"
    update_machine_config "$machine_id" "$config_file" >/dev/null 2>&1 || true
    restart_machine_best_effort "$machine_id"
}

ensure_app() {
    if app_exists; then
        echo "Fly app ${APP_NAME} already exists"
    else
        fly apps create "$APP_NAME" --org "$FLY_ORG" --yes
    fi
}

ensure_volume() {
    local id
    id="$(volume_id)"
    if [[ -n "$id" ]]; then
        echo "Fly volume ${VOLUME_NAME} already exists: ${id}"
    else
        fly volumes create "$VOLUME_NAME" \
            --app "$APP_NAME" \
            --region "$FLY_REGION" \
            --size "$VOLUME_SIZE_GB" \
            --snapshot-retention "$SNAPSHOT_RETENTION_DAYS" \
            --scheduled-snapshots \
            --yes
    fi
}

ensure_ips() {
    local ips
    ips="$(fly ips list --app "$APP_NAME" --json)"
    if ! jq -e '.[] | select((.Type // .type) == "shared_v4" or (.Type // .type) == "v4")' <<<"$ips" >/dev/null; then
        fly ips allocate-v4 --app "$APP_NAME" --shared --yes
    fi
    if ! jq -e '.[] | select((.Type // .type) == "v6")' <<<"$ips" >/dev/null; then
        fly ips allocate-v6 --app "$APP_NAME"
    fi
}

ensure_certificate() {
    local hostname="wwmtf.hyperchad.dev"
    if fly certs list --app "$APP_NAME" --json \
        | jq -e --arg hostname "$hostname" '.[] | select((.Hostname // .hostname) == $hostname)' >/dev/null; then
        echo "Fly certificate ${hostname} already exists"
    else
        fly certs add --app "$APP_NAME" "$hostname"
    fi
}

snapshot_volume() {
    local id
    id="$(volume_id)"
    [[ -n "$id" ]] || { echo "Volume ${VOLUME_NAME} does not exist" >&2; exit 1; }

    local -a machines=()
    local machine
    while IFS= read -r machine; do
        [[ -z "$machine" ]] || machines+=("$machine")
    done < <(machine_ids)
    ((${#machines[@]} == 1)) || {
        echo "Expected exactly one production Machine" >&2
        exit 1
    }

    local snapshot
    echo "Creating an application-consistent backup before the volume snapshot"
    fly ssh console --app "$APP_NAME" --command "sh -c 'kill -USR1 \$(cat /tmp/wwmtf-supervisor.pid)'"
    for _ in $(seq 1 30); do
        if fly ssh console --app "$APP_NAME" --command "test -s /data/backups/database.tar.gz"; then
            break
        fi
        sleep 1
    done
    fly ssh console --app "$APP_NAME" --command "test -s /data/backups/database.tar.gz"

    local machine_id="${machines[0]}"
    local original_config
    local quiesced_config
    original_config="$(mktemp)"
    quiesced_config="$(mktemp)"
    machine_config "$machine_id" >"$original_config"
    # Updating with skip_launch stops the Machine. Disable both proxy triggers
    # first so it remains stopped while the attached volume is snapshotted.
    jq '
        .services = [
            .services[]
            | .autostart = false
            | .min_machines_running = 0
        ]
    ' "$original_config" >"$quiesced_config"
    trap "restore_machine_after_snapshot_best_effort '$machine_id' '$original_config'; rm -f '$original_config' '$quiesced_config'" EXIT

    update_machine_config "$machine_id" "$quiesced_config" >/dev/null
    for _ in $(seq 1 30); do
        if [[ "$(machine_state "$machine_id")" == "stopped" ]]; then
            break
        fi
        sleep 1
    done
    [[ "$(machine_state "$machine_id")" == "stopped" ]]

    snapshot="$(create_volume_snapshot "$id")"
    echo "$snapshot"

    update_machine_config "$machine_id" "$original_config" >/dev/null
    ensure_machine_started "$machine_id"
    rm -f "$original_config" "$quiesced_config"
    trap - EXIT
    smoke_test "https://${APP_NAME}.fly.dev" false
}

output_fly_ipv6() {
    local ips
    local ipv6
    ips="$(fly ips list --app "$APP_NAME" --json)"
    ipv6="$(jq -r '[.[] | select((.Type // .type) == "v6") | (.Address // .address)][0] // empty' <<<"$ips")"
    [[ -n "$ipv6" ]] || { echo "Fly IPv6 address is unavailable" >&2; exit 1; }
    printf '%s\n' "$ipv6"
}

output_certificate_dns() {
    local hostname="wwmtf.hyperchad.dev"
    local certificate
    local ownership
    certificate="$(fly certs check --app "$APP_NAME" --json "$hostname")"
    ownership="$(jq -r '.dns_requirements.ownership.app_value // empty' <<<"$certificate")"
    [[ -n "$ownership" ]] || { echo "Fly ownership TXT value is unavailable" >&2; exit 1; }
    jq -n \
        --arg ipv6 "$(output_fly_ipv6)" \
        --arg ownership "$ownership" \
        '{fly_ipv6_address: $ipv6, fly_ownership_txt: $ownership}'
}

smoke_test() {
    local url="${1:-$PUBLIC_URL}"
    local require_google="${2:-true}"
    local headers
    local login
    curl --fail --silent --show-error --retry 12 --retry-all-errors --retry-delay 5 \
        "${url%/}/health/live"
    curl --fail --silent --show-error --retry 12 --retry-all-errors --retry-delay 5 \
        "${url%/}/health/ready"
    curl --fail --silent --show-error --retry 12 --retry-all-errors --retry-delay 5 \
        "${url%/}/" >/dev/null
    login="$(curl --fail --silent --show-error --retry 12 --retry-all-errors --retry-delay 5 \
        "${url%/}/login")"
    if [[ "$require_google" == "true" ]]; then
        grep -F 'Continue with Google' <<<"$login" >/dev/null
        grep -F '/auth/google/start' <<<"$login" >/dev/null
        headers="$(curl --silent --show-error --dump-header - --output /dev/null \
            "${url%/}/auth/google/start")"
        grep -Eiq '^location: https://accounts\.google\.com/' <<<"$headers"
        if grep -Eiq 'wwmtf-google-client-secret|authorization_code|id_token' <<<"$login$headers"; then
            echo "Smoke response exposed forbidden Google authentication material" >&2
            exit 1
        fi
    fi
    echo "Smoke tests passed for ${url}"
}

sync_google_secrets() {
    : "${WWMTF_GOOGLE_CLIENT_ID:?WWMTF_GOOGLE_CLIENT_ID is required}"
    : "${WWMTF_GOOGLE_CLIENT_SECRET:?WWMTF_GOOGLE_CLIENT_SECRET is required}"
    printf '%s\n' \
        "WWMTF_GOOGLE_CLIENT_ID=${WWMTF_GOOGLE_CLIENT_ID}" \
        "WWMTF_GOOGLE_CLIENT_SECRET=${WWMTF_GOOGLE_CLIENT_SECRET}" \
        | fly secrets import --app "$APP_NAME" --stage
}

bootstrap() {
    command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
    ensure_app
    ensure_volume
    ensure_ips
    ensure_certificate
}

deploy() {
    local release
    release="${GITHUB_SHA:-$(git rev-parse HEAD)}"
    sync_google_secrets
    fly deploy --app "$APP_NAME" --ha=false --strategy immediate --wait-timeout 10m \
        --build-arg "WWMTF_RELEASE=${release}"
    smoke_test "https://${APP_NAME}.fly.dev"
}

case "${1:-help}" in
    bootstrap) bootstrap ;;
    certificate-dns) output_certificate_dns ;;
    deploy) deploy ;;
    ensure-started) ensure_machine_started "${2:?Machine ID is required}" ;;
    snapshot) snapshot_volume ;;
    smoke) smoke_test "${2:-$PUBLIC_URL}" ;;
    *)
        echo "Usage: $0 {bootstrap|certificate-dns|deploy|ensure-started machine-id|snapshot|smoke [url]}" >&2
        exit 2
        ;;
esac
