#!/bin/bash
# ==============================================================================
# Aethyme Cloud - Rollback Script
# ==============================================================================

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Configuration
NAMESPACE="aethyme-cloud"
APP_NAME="aethyme-api"
TIMEOUT="300s"

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

confirm_rollback() {
    log_warn "This will rollback the deployment to the previous version."
    log_warn "Current deployment status:"
    kubectl rollout history deployment/"$APP_NAME" -n "$NAMESPACE"
    echo ""
    read -p "Are you sure you want to rollback? (yes/no): " -r
    if [[ ! $REPLY =~ ^yes$ ]]; then
        log_info "Rollback cancelled."
        exit 0
    fi
}

perform_rollback() {
    log_info "Rolling back deployment..."

    if kubectl rollout undo deployment/"$APP_NAME" -n "$NAMESPACE"; then
        log_info "Rollback initiated ✓"
    else
        log_error "Rollback failed!"
        exit 1
    fi
}

wait_for_rollback() {
    log_info "Waiting for rollback to complete..."

    if kubectl rollout status deployment/"$APP_NAME" -n "$NAMESPACE" --timeout="$TIMEOUT"; then
        log_info "Rollback completed ✓"
    else
        log_error "Rollback did not complete in time!"
        exit 1
    fi
}

health_check() {
    log_info "Running health check on rolled back version..."

    if ! kubectl wait --for=condition=Ready pod \
        -l app="$APP_NAME" \
        -n "$NAMESPACE" \
        --timeout=120s; then
        log_error "Pods not ready after rollback"
        exit 1
    fi

    log_info "Health check passed ✓"
}

show_status() {
    log_info "Rollback status:"
    kubectl get pods -n "$NAMESPACE" -l app="$APP_NAME"
    echo ""
    log_info "Rollback completed successfully!"
}

main() {
    log_info "Starting rollback process..."
    echo ""

    confirm_rollback
    perform_rollback
    wait_for_rollback
    health_check
    show_status
}

main "$@"
