#!/usr/bin/env bash
set -euo pipefail

#
# PostgreSQL Backup Script for Aethyme
#
# Creates compressed backups with timestamps
# Uploads to cloud storage (S3/GCS)
# Maintains retention policy
#

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration
NAMESPACE="${NAMESPACE:-default}"
POD_NAME="${POD_NAME:-aethyme-postgres-0}"
DATABASE="${DATABASE:-aethyme}"
BACKUP_DIR="${BACKUP_DIR:-/tmp/backups/postgres}"
S3_BUCKET="${S3_BUCKET:-}"
GCS_BUCKET="${GCS_BUCKET:-}"
RETENTION_DAYS="${RETENTION_DAYS:-30}"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_FILE="postgres_${DATABASE}_${TIMESTAMP}.sql.gz"

log_info() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [INFO] $1"
}

log_error() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [ERROR] $1" >&2
}

# Create backup directory
mkdir -p "$BACKUP_DIR"

# Perform backup
backup_database() {
    log_info "Starting PostgreSQL backup for database: $DATABASE"

    # Execute pg_dump through kubectl
    if kubectl exec -n "$NAMESPACE" "$POD_NAME" -- \
        pg_dump -U postgres -d "$DATABASE" --clean --if-exists \
        | gzip > "$BACKUP_DIR/$BACKUP_FILE"; then

        local size=$(du -h "$BACKUP_DIR/$BACKUP_FILE" | cut -f1)
        log_info "Backup completed successfully: $BACKUP_FILE (Size: $size)"
        return 0
    else
        log_error "Backup failed"
        return 1
    fi
}

# Verify backup
verify_backup() {
    log_info "Verifying backup integrity..."

    if gunzip -t "$BACKUP_DIR/$BACKUP_FILE" 2>/dev/null; then
        log_info "Backup file integrity verified"
        return 0
    else
        log_error "Backup file is corrupted"
        return 1
    fi
}

# Upload to S3
upload_to_s3() {
    if [[ -z "$S3_BUCKET" ]]; then
        log_info "S3 upload skipped (S3_BUCKET not set)"
        return 0
    fi

    log_info "Uploading backup to S3: s3://$S3_BUCKET/postgres/"

    if command -v aws &> /dev/null; then
        aws s3 cp "$BACKUP_DIR/$BACKUP_FILE" \
            "s3://$S3_BUCKET/postgres/$BACKUP_FILE" \
            --storage-class STANDARD_IA

        log_info "Backup uploaded to S3 successfully"
    else
        log_error "AWS CLI not found, skipping S3 upload"
        return 1
    fi
}

# Upload to GCS
upload_to_gcs() {
    if [[ -z "$GCS_BUCKET" ]]; then
        log_info "GCS upload skipped (GCS_BUCKET not set)"
        return 0
    fi

    log_info "Uploading backup to GCS: gs://$GCS_BUCKET/postgres/"

    if command -v gsutil &> /dev/null; then
        gsutil cp "$BACKUP_DIR/$BACKUP_FILE" \
            "gs://$GCS_BUCKET/postgres/$BACKUP_FILE"

        log_info "Backup uploaded to GCS successfully"
    else
        log_error "gsutil not found, skipping GCS upload"
        return 1
    fi
}

# Cleanup old backups (local)
cleanup_old_backups() {
    log_info "Cleaning up backups older than $RETENTION_DAYS days..."

    find "$BACKUP_DIR" -name "postgres_*.sql.gz" -type f -mtime +"$RETENTION_DAYS" -delete

    log_info "Local cleanup complete"
}

# Cleanup old backups (S3)
cleanup_s3_backups() {
    if [[ -z "$S3_BUCKET" ]] || ! command -v aws &> /dev/null; then
        return 0
    fi

    log_info "Cleaning up S3 backups older than $RETENTION_DAYS days..."

    local cutoff_date=$(date -d "$RETENTION_DAYS days ago" +%Y-%m-%d 2>/dev/null || \
                       date -v-${RETENTION_DAYS}d +%Y-%m-%d)

    aws s3 ls "s3://$S3_BUCKET/postgres/" | while read -r line; do
        local file_date=$(echo "$line" | awk '{print $1}')
        local file_name=$(echo "$line" | awk '{print $4}')

        if [[ "$file_date" < "$cutoff_date" ]]; then
            aws s3 rm "s3://$S3_BUCKET/postgres/$file_name"
            log_info "Deleted old backup: $file_name"
        fi
    done
}

# Main backup flow
main() {
    log_info "=== PostgreSQL Backup Starting ==="

    if ! backup_database; then
        log_error "Backup failed, exiting"
        exit 1
    fi

    if ! verify_backup; then
        log_error "Backup verification failed, exiting"
        exit 1
    fi

    # Upload to cloud storage
    upload_to_s3 || log_error "S3 upload failed (continuing...)"
    upload_to_gcs || log_error "GCS upload failed (continuing...)"

    # Cleanup
    cleanup_old_backups
    cleanup_s3_backups || log_error "S3 cleanup failed (continuing...)"

    log_info "=== PostgreSQL Backup Complete ==="
    log_info "Backup file: $BACKUP_FILE"
}

main "$@"
