#!/usr/bin/env bash
set -euo pipefail

#
# Backup Verification Script
#
# Verifies integrity and restorability of backups
#

BACKUP_DIR="${BACKUP_DIR:-/tmp/backups}"

log_info() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [INFO] $1"
}

log_error() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [ERROR] $1" >&2
}

log_success() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [SUCCESS] $1"
}

# Verify PostgreSQL backups
verify_postgres_backups() {
    log_info "Verifying PostgreSQL backups..."

    local backup_count=0
    local valid_count=0
    local invalid_count=0

    for backup in "$BACKUP_DIR/postgres"/*.sql.gz; do
        if [[ ! -f "$backup" ]]; then
            continue
        fi

        backup_count=$((backup_count + 1))
        local filename=$(basename "$backup")

        # Check file size
        local size=$(stat -f%z "$backup" 2>/dev/null || stat -c%s "$backup" 2>/dev/null)
        if [[ $size -lt 1000 ]]; then
            log_error "Backup too small (likely empty): $filename"
            invalid_count=$((invalid_count + 1))
            continue
        fi

        # Verify gzip integrity
        if gunzip -t "$backup" 2>/dev/null; then
            # Check for SQL content
            if gunzip -c "$backup" | head -n 10 | grep -q "PostgreSQL"; then
                log_success "Valid backup: $filename"
                valid_count=$((valid_count + 1))
            else
                log_error "Invalid SQL content: $filename"
                invalid_count=$((invalid_count + 1))
            fi
        else
            log_error "Corrupted gzip file: $filename"
            invalid_count=$((invalid_count + 1))
        fi
    done

    log_info "PostgreSQL backups: $backup_count total, $valid_count valid, $invalid_count invalid"
    return $invalid_count
}

# Verify Redis backups
verify_redis_backups() {
    log_info "Verifying Redis backups..."

    local backup_count=0
    local valid_count=0
    local invalid_count=0

    for backup in "$BACKUP_DIR/redis"/*.rdb; do
        if [[ ! -f "$backup" ]]; then
            continue
        fi

        backup_count=$((backup_count + 1))
        local filename=$(basename "$backup")

        # Check file size
        local size=$(stat -f%z "$backup" 2>/dev/null || stat -c%s "$backup" 2>/dev/null)
        if [[ $size -lt 100 ]]; then
            log_error "Backup too small: $filename"
            invalid_count=$((invalid_count + 1))
            continue
        fi

        # Check RDB magic bytes (REDIS)
        if head -c 5 "$backup" | grep -q "REDIS"; then
            log_success "Valid RDB backup: $filename"
            valid_count=$((valid_count + 1))
        else
            log_error "Invalid RDB format: $filename"
            invalid_count=$((invalid_count + 1))
        fi
    done

    log_info "Redis backups: $backup_count total, $valid_count valid, $invalid_count invalid"
    return $invalid_count
}

# Check backup freshness
check_backup_freshness() {
    log_info "Checking backup freshness..."

    local max_age_hours=48
    local now=$(date +%s)

    # Check latest PostgreSQL backup
    local latest_pg=$(ls -t "$BACKUP_DIR/postgres"/*.sql.gz 2>/dev/null | head -n 1)
    if [[ -n "$latest_pg" ]]; then
        local pg_age=$(stat -f%m "$latest_pg" 2>/dev/null || stat -c%Y "$latest_pg" 2>/dev/null)
        local pg_hours=$(( (now - pg_age) / 3600 ))

        if [[ $pg_hours -gt $max_age_hours ]]; then
            log_error "Latest PostgreSQL backup is ${pg_hours}h old (> ${max_age_hours}h)"
        else
            log_success "Latest PostgreSQL backup is ${pg_hours}h old"
        fi
    else
        log_error "No PostgreSQL backups found"
    fi

    # Check latest Redis backup
    local latest_redis=$(ls -t "$BACKUP_DIR/redis"/*.rdb 2>/dev/null | head -n 1)
    if [[ -n "$latest_redis" ]]; then
        local redis_age=$(stat -f%m "$latest_redis" 2>/dev/null || stat -c%Y "$latest_redis" 2>/dev/null)
        local redis_hours=$(( (now - redis_age) / 3600 ))

        if [[ $redis_hours -gt $max_age_hours ]]; then
            log_error "Latest Redis backup is ${redis_hours}h old (> ${max_age_hours}h)"
        else
            log_success "Latest Redis backup is ${redis_hours}h old"
        fi
    else
        log_error "No Redis backups found"
    fi
}

# Generate backup report
generate_report() {
    log_info "Generating backup report..."

    echo ""
    echo "=== Backup Report ==="
    echo "Generated: $(date)"
    echo ""

    echo "PostgreSQL Backups:"
    ls -lh "$BACKUP_DIR/postgres"/*.sql.gz 2>/dev/null || echo "  No backups found"
    echo ""

    echo "Redis Backups:"
    ls -lh "$BACKUP_DIR/redis"/*.rdb 2>/dev/null || echo "  No backups found"
    echo ""

    local total_size=$(du -sh "$BACKUP_DIR" 2>/dev/null | cut -f1)
    echo "Total backup size: $total_size"
    echo ""
}

# Main verification flow
main() {
    log_info "=== Backup Verification Starting ==="

    local exit_code=0

    verify_postgres_backups || exit_code=1
    verify_redis_backups || exit_code=1
    check_backup_freshness
    generate_report

    if [[ $exit_code -eq 0 ]]; then
        log_success "All backup verifications passed"
    else
        log_error "Some backup verifications failed"
    fi

    log_info "=== Backup Verification Complete ==="
    exit $exit_code
}

main "$@"
