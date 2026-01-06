#!/usr/bin/env bash
set -euo pipefail

#
# Redis Backup Script for RepoGraph
#
# Creates RDB snapshots and uploads to cloud storage
#

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration
NAMESPACE="${NAMESPACE:-default}"
POD_NAME="${POD_NAME:-repograph-redis-0}"
BACKUP_DIR="${BACKUP_DIR:-/tmp/backups/redis}"
S3_BUCKET="${S3_BUCKET:-}"
GCS_BUCKET="${GCS_BUCKET:-}"
RETENTION_DAYS="${RETENTION_DAYS:-30}"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_FILE="redis_${TIMESTAMP}.rdb"

log_info() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [INFO] $1"
}

log_error() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [ERROR] $1" >&2
}

# Create backup directory
mkdir -p "$BACKUP_DIR"

# Trigger Redis save
trigger_save() {
    log_info "Triggering Redis BGSAVE..."

    kubectl exec -n "$NAMESPACE" "$POD_NAME" -- redis-cli BGSAVE

    # Wait for save to complete
    local timeout=300
    local elapsed=0

    while [[ $elapsed -lt $timeout ]]; do
        local save_status=$(kubectl exec -n "$NAMESPACE" "$POD_NAME" -- \
            redis-cli LASTSAVE)

        sleep 5
        elapsed=$((elapsed + 5))

        local new_save_status=$(kubectl exec -n "$NAMESPACE" "$POD_NAME" -- \
            redis-cli LASTSAVE)

        if [[ "$new_save_status" != "$save_status" ]]; then
            log_info "BGSAVE completed"
            return 0
        fi
    done

    log_error "BGSAVE timeout"
    return 1
}

# Copy RDB file
copy_rdb() {
    log_info "Copying RDB file from pod..."

    kubectl cp "$NAMESPACE/$POD_NAME:/data/dump.rdb" \
        "$BACKUP_DIR/$BACKUP_FILE"

    local size=$(du -h "$BACKUP_DIR/$BACKUP_FILE" | cut -f1)
    log_info "RDB file copied: $BACKUP_FILE (Size: $size)"
}

# Upload to S3
upload_to_s3() {
    if [[ -z "$S3_BUCKET" ]] || ! command -v aws &> /dev/null; then
        log_info "S3 upload skipped"
        return 0
    fi

    log_info "Uploading to S3..."

    aws s3 cp "$BACKUP_DIR/$BACKUP_FILE" \
        "s3://$S3_BUCKET/redis/$BACKUP_FILE" \
        --storage-class STANDARD_IA

    log_info "Upload complete"
}

# Upload to GCS
upload_to_gcs() {
    if [[ -z "$GCS_BUCKET" ]] || ! command -v gsutil &> /dev/null; then
        log_info "GCS upload skipped"
        return 0
    fi

    log_info "Uploading to GCS..."

    gsutil cp "$BACKUP_DIR/$BACKUP_FILE" \
        "gs://$GCS_BUCKET/redis/$BACKUP_FILE"

    log_info "Upload complete"
}

# Cleanup old backups
cleanup_old_backups() {
    log_info "Cleaning up old backups..."

    find "$BACKUP_DIR" -name "redis_*.rdb" -type f -mtime +"$RETENTION_DAYS" -delete

    log_info "Cleanup complete"
}

# Main backup flow
main() {
    log_info "=== Redis Backup Starting ==="

    trigger_save
    copy_rdb
    upload_to_s3
    upload_to_gcs
    cleanup_old_backups

    log_info "=== Redis Backup Complete ==="
    log_info "Backup file: $BACKUP_FILE"
}

main "$@"
