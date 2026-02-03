#!/usr/bin/env bash
set -euo pipefail

#
# PostgreSQL Restore Script for Aethyme
#
# Restores from backup file or downloads from cloud storage
# Creates pre-restore backup for safety
#

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration
NAMESPACE="${NAMESPACE:-default}"
POD_NAME="${POD_NAME:-aethyme-postgres-0}"
DATABASE="${DATABASE:-aethyme}"
BACKUP_FILE="${BACKUP_FILE:-}"
BACKUP_DIR="${BACKUP_DIR:-/tmp/backups/postgres}"
S3_BUCKET="${S3_BUCKET:-}"
GCS_BUCKET="${GCS_BUCKET:-}"

log_info() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [INFO] $1"
}

log_error() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [ERROR] $1" >&2
}

log_warning() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [WARNING] $1"
}

# Validate prerequisites
validate_prerequisites() {
    if [[ -z "$BACKUP_FILE" ]]; then
        log_error "BACKUP_FILE must be specified"
        echo "Usage: BACKUP_FILE=postgres_aethyme_20240101_120000.sql.gz $0"
        exit 1
    fi

    if ! kubectl get pod -n "$NAMESPACE" "$POD_NAME" &>/dev/null; then
        log_error "PostgreSQL pod not found: $POD_NAME"
        exit 1
    fi
}

# Download backup from S3
download_from_s3() {
    if [[ ! -f "$BACKUP_DIR/$BACKUP_FILE" ]]; then
        if [[ -n "$S3_BUCKET" ]] && command -v aws &> /dev/null; then
            log_info "Downloading backup from S3..."
            aws s3 cp "s3://$S3_BUCKET/postgres/$BACKUP_FILE" \
                "$BACKUP_DIR/$BACKUP_FILE"
            log_info "Download complete"
        fi
    fi
}

# Download backup from GCS
download_from_gcs() {
    if [[ ! -f "$BACKUP_DIR/$BACKUP_FILE" ]]; then
        if [[ -n "$GCS_BUCKET" ]] && command -v gsutil &> /dev/null; then
            log_info "Downloading backup from GCS..."
            gsutil cp "gs://$GCS_BUCKET/postgres/$BACKUP_FILE" \
                "$BACKUP_DIR/$BACKUP_FILE"
            log_info "Download complete"
        fi
    fi
}

# Create pre-restore backup
create_pre_restore_backup() {
    log_info "Creating pre-restore backup for safety..."

    local pre_restore_file="pre_restore_$(date +%Y%m%d_%H%M%S).sql.gz"

    if kubectl exec -n "$NAMESPACE" "$POD_NAME" -- \
        pg_dump -U postgres -d "$DATABASE" | gzip > "$BACKUP_DIR/$pre_restore_file"; then
        log_info "Pre-restore backup created: $pre_restore_file"
    else
        log_warning "Pre-restore backup failed, continuing anyway..."
    fi
}

# Stop application pods
stop_application() {
    log_info "Scaling down application pods..."

    kubectl scale deployment aethyme -n "$NAMESPACE" --replicas=0 || true
    kubectl scale deployment aethyme-indexer -n "$NAMESPACE" --replicas=0 || true

    sleep 10
    log_info "Application pods scaled down"
}

# Restore database
restore_database() {
    log_info "Starting database restore from: $BACKUP_FILE"

    if [[ ! -f "$BACKUP_DIR/$BACKUP_FILE" ]]; then
        log_error "Backup file not found: $BACKUP_DIR/$BACKUP_FILE"
        exit 1
    fi

    # Verify backup file
    if ! gunzip -t "$BACKUP_DIR/$BACKUP_FILE" 2>/dev/null; then
        log_error "Backup file is corrupted"
        exit 1
    fi

    # Terminate existing connections
    log_info "Terminating existing database connections..."
    kubectl exec -n "$NAMESPACE" "$POD_NAME" -- psql -U postgres -c \
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '$DATABASE' AND pid <> pg_backend_pid();" \
        || true

    # Drop and recreate database
    log_info "Dropping and recreating database..."
    kubectl exec -n "$NAMESPACE" "$POD_NAME" -- psql -U postgres -c \
        "DROP DATABASE IF EXISTS ${DATABASE};"
    kubectl exec -n "$NAMESPACE" "$POD_NAME" -- psql -U postgres -c \
        "CREATE DATABASE ${DATABASE};"

    # Restore from backup
    log_info "Restoring database..."
    gunzip -c "$BACKUP_DIR/$BACKUP_FILE" | \
        kubectl exec -i -n "$NAMESPACE" "$POD_NAME" -- \
        psql -U postgres -d "$DATABASE"

    if [[ $? -eq 0 ]]; then
        log_info "Database restore completed successfully"
    else
        log_error "Database restore failed"
        exit 1
    fi
}

# Verify restore
verify_restore() {
    log_info "Verifying database restore..."

    # Check if database exists
    local db_exists=$(kubectl exec -n "$NAMESPACE" "$POD_NAME" -- \
        psql -U postgres -tAc "SELECT 1 FROM pg_database WHERE datname='$DATABASE'")

    if [[ "$db_exists" != "1" ]]; then
        log_error "Database verification failed: database does not exist"
        exit 1
    fi

    # Check table count
    local table_count=$(kubectl exec -n "$NAMESPACE" "$POD_NAME" -- \
        psql -U postgres -d "$DATABASE" -tAc \
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='public';")

    log_info "Database contains $table_count tables"

    if [[ $table_count -eq 0 ]]; then
        log_warning "No tables found in restored database"
    else
        log_info "Database verification passed"
    fi
}

# Restart application
restart_application() {
    log_info "Scaling up application pods..."

    kubectl scale deployment aethyme -n "$NAMESPACE" --replicas=3 || true
    kubectl scale deployment aethyme-indexer -n "$NAMESPACE" --replicas=2 || true

    log_info "Waiting for pods to be ready..."
    kubectl wait --for=condition=available --timeout=300s \
        deployment/aethyme -n "$NAMESPACE" || true

    log_info "Application pods restarted"
}

# Main restore flow
main() {
    log_info "=== PostgreSQL Restore Starting ==="

    validate_prerequisites

    # Prompt for confirmation
    read -p "This will REPLACE the current database. Continue? (yes/no): " confirm
    if [[ "$confirm" != "yes" ]]; then
        log_info "Restore cancelled by user"
        exit 0
    fi

    # Download backup if needed
    download_from_s3
    download_from_gcs

    # Perform restore
    create_pre_restore_backup
    stop_application
    restore_database
    verify_restore
    restart_application

    log_info "=== PostgreSQL Restore Complete ==="
    log_info "Restored from: $BACKUP_FILE"
}

main "$@"
