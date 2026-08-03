#!/usr/bin/env bash
set -euo pipefail

BUCKET="${TOFU_STATE_BUCKET:?TOFU_STATE_BUCKET is required}"
ENDPOINT="${TOFU_STATE_S3_ENDPOINT:?TOFU_STATE_S3_ENDPOINT is required}"
REGION="${TOFU_STATE_REGION:-auto}"
STATE_KEY="${TOFU_STATE_KEY:-words-with-spouses/production.tfstate}"
LOCK_KEY="${STATE_KEY}.tflock"
ACTION="${1:-archive}"

command -v aws >/dev/null || {
    echo "aws is required" >&2
    exit 1
}
command -v jq >/dev/null || {
    echo "jq is required" >&2
    exit 1
}

object_exists() {
    local key="$1"
    aws s3api list-objects-v2 \
        --bucket "$BUCKET" \
        --prefix "$key" \
        --max-keys 2 \
        --endpoint-url "$ENDPOINT" \
        --region "$REGION" \
        --output json \
        | jq -e --arg key "$key" 'any(.Contents[]?; .Key == $key)' >/dev/null
}

assert_unlocked() {
    if object_exists "$LOCK_KEY"; then
        echo "Refusing to modify state while s3://${BUCKET}/${LOCK_KEY} exists" >&2
        exit 1
    fi
}

case "$ACTION" in
    archive)
        assert_unlocked
        if object_exists "$STATE_KEY"; then
            timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
            archive_key="history/${timestamp}/production.tfstate"
            aws s3api copy-object \
                --bucket "$BUCKET" \
                --copy-source "${BUCKET}/${STATE_KEY}" \
                --key "$archive_key" \
                --endpoint-url "$ENDPOINT" \
                --region "$REGION" >/dev/null
            echo "Archived encrypted OpenTofu state to s3://${BUCKET}/${archive_key}"
        else
            echo "No existing OpenTofu state to archive"
        fi
        ;;
    list)
        aws s3api list-objects-v2 \
            --bucket "$BUCKET" \
            --prefix history/ \
            --endpoint-url "$ENDPOINT" \
            --region "$REGION" \
            --query 'Contents[].{Key:Key,LastModified:LastModified,Size:Size}' \
            --output table
        ;;
    restore)
        archive_key="${2:?Usage: $0 restore history/<timestamp>/production.tfstate}"
        [[ "$archive_key" == history/*/production.tfstate ]] || {
            echo "Archive key must match history/<timestamp>/production.tfstate" >&2
            exit 2
        }
        assert_unlocked
        object_exists "$archive_key" || {
            echo "State archive does not exist: s3://${BUCKET}/${archive_key}" >&2
            exit 1
        }
        echo "This overwrites the live encrypted OpenTofu state object."
        read -r -p "Restore ${archive_key}? [y/N] " answer
        [[ "$answer" == "y" || "$answer" == "Y" ]] || {
            echo "Restore cancelled"
            exit 0
        }
        aws s3api copy-object \
            --bucket "$BUCKET" \
            --copy-source "${BUCKET}/${archive_key}" \
            --key "$STATE_KEY" \
            --endpoint-url "$ENDPOINT" \
            --region "$REGION" >/dev/null
        echo "Restored s3://${BUCKET}/${archive_key} to ${STATE_KEY}"
        ;;
    *)
        echo "Usage: $0 {archive|list|restore history/<timestamp>/production.tfstate}" >&2
        exit 2
        ;;
esac
