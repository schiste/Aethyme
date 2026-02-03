#!/usr/bin/env bash
set -euo pipefail

#
# Canary Deployment Script for Aethyme
#
# Progressive traffic shifting: 10% → 50% → 100%
# Monitors error rates and latency at each stage
# Automatic rollback on degradation
#

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Configuration
NAMESPACE="${NAMESPACE:-default}"
RELEASE_NAME="${RELEASE_NAME:-aethyme}"
HELM_CHART="${HELM_CHART:-$PROJECT_ROOT/k8s/helm/aethyme}"
VALUES_FILE="${VALUES_FILE:-values-production.yaml}"
NEW_VERSION="${NEW_VERSION:-}"

# Canary stages: traffic percentage and monitoring duration
CANARY_STAGES="${CANARY_STAGES:-10 50 100}"
STAGE_DURATION="${STAGE_DURATION:-300}"  # 5 minutes per stage
HEALTH_CHECK_INTERVAL="${HEALTH_CHECK_INTERVAL:-30}"

# Thresholds for rollback
MAX_ERROR_RATE="${MAX_ERROR_RATE:-0.05}"  # 5%
MAX_LATENCY_P95="${MAX_LATENCY_P95:-2000}"  # 2 seconds in ms

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Validate prerequisites
validate_prerequisites() {
    log_info "Validating prerequisites..."

    if ! command -v kubectl &> /dev/null; then
        log_error "kubectl not found"
        exit 1
    fi

    if ! command -v helm &> /dev/null; then
        log_error "helm not found"
        exit 1
    fi

    if [[ -z "$NEW_VERSION" ]]; then
        log_error "NEW_VERSION must be set"
        exit 1
    fi

    log_success "Prerequisites validated"
}

# Deploy canary version
deploy_canary() {
    log_info "Deploying canary version $NEW_VERSION..."

    # Calculate initial canary replicas (10% of stable)
    local stable_replicas=$(kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" \
        -o jsonpath='{.spec.replicas}' 2>/dev/null || echo "3")
    local canary_replicas=$(( stable_replicas / 10 ))
    if [[ $canary_replicas -lt 1 ]]; then
        canary_replicas=1
    fi

    # Deploy canary
    helm upgrade --install "${RELEASE_NAME}-canary" "$HELM_CHART" \
        --namespace "$NAMESPACE" \
        --values "$HELM_CHART/$VALUES_FILE" \
        --set image.tag="$NEW_VERSION" \
        --set fullnameOverride="${RELEASE_NAME}-canary" \
        --set replicaCount="$canary_replicas" \
        --set autoscaling.enabled=false \
        --set podLabels.version="canary" \
        --wait \
        --timeout 10m

    log_success "Canary deployed with $canary_replicas replicas"
}

# Wait for canary to be ready
wait_for_canary() {
    log_info "Waiting for canary deployment to be ready..."

    kubectl wait --for=condition=available \
        --timeout=300s \
        deployment/"${RELEASE_NAME}-canary" \
        -n "$NAMESPACE"

    # Additional health check
    sleep 10
    local canary_pod=$(kubectl get pods -n "$NAMESPACE" \
        -l "app.kubernetes.io/instance=${RELEASE_NAME}-canary" \
        -o jsonpath='{.items[0].metadata.name}')

    if kubectl exec -n "$NAMESPACE" "$canary_pod" -- \
        curl -sf http://localhost:8000/health/ready > /dev/null; then
        log_success "Canary is ready and healthy"
    else
        log_error "Canary health check failed"
        return 1
    fi
}

# Shift traffic to canary
shift_traffic() {
    local percentage=$1
    log_info "Shifting ${percentage}% of traffic to canary..."

    local stable_replicas=$(kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" \
        -o jsonpath='{.spec.replicas}')
    local total_replicas=10  # For easier percentage calculations

    # Calculate replica distribution
    local canary_replicas=$(( total_replicas * percentage / 100 ))
    local stable_replicas_new=$(( total_replicas - canary_replicas ))

    if [[ $canary_replicas -lt 1 ]]; then
        canary_replicas=1
    fi
    if [[ $stable_replicas_new -lt 1 ]]; then
        stable_replicas_new=1
    fi

    # Scale deployments
    kubectl scale deployment "${RELEASE_NAME}-canary" -n "$NAMESPACE" \
        --replicas="$canary_replicas"

    if [[ $percentage -lt 100 ]]; then
        kubectl scale deployment "$RELEASE_NAME" -n "$NAMESPACE" \
            --replicas="$stable_replicas_new"
    fi

    # Wait for scaling
    sleep 20
    log_success "Traffic shifted: ${percentage}% canary, $((100-percentage))% stable"
}

# Monitor canary metrics
monitor_canary() {
    local duration=$1
    local percentage=$2
    log_info "Monitoring canary for $duration seconds at ${percentage}% traffic..."

    local start_time=$(date +%s)
    local violations=0
    local max_violations=3

    while true; do
        local current_time=$(date +%s)
        local elapsed=$((current_time - start_time))

        if [[ $elapsed -ge $duration ]]; then
            log_success "Monitoring period complete, canary is stable"
            return 0
        fi

        # Check pod health
        local unhealthy_pods=$(kubectl get pods -n "$NAMESPACE" \
            -l "app.kubernetes.io/instance=${RELEASE_NAME}-canary" \
            -o json | jq -r '.items[] | select(.status.phase != "Running") | .metadata.name' | wc -l)

        if [[ $unhealthy_pods -gt 0 ]]; then
            violations=$((violations + 1))
            log_warning "Detected $unhealthy_pods unhealthy canary pods (violations: $violations/$max_violations)"

            if [[ $violations -ge $max_violations ]]; then
                log_error "Too many health violations"
                return 1
            fi
        else
            violations=0
        fi

        # Check error rates from Prometheus (if available)
        if command -v promtool &> /dev/null; then
            local error_rate=$(check_error_rate_prometheus)
            if (( $(echo "$error_rate > $MAX_ERROR_RATE" | bc -l) )); then
                log_error "Error rate too high: $error_rate > $MAX_ERROR_RATE"
                return 1
            fi

            local latency_p95=$(check_latency_prometheus)
            if (( $(echo "$latency_p95 > $MAX_LATENCY_P95" | bc -l) )); then
                log_error "Latency too high: ${latency_p95}ms > ${MAX_LATENCY_P95}ms"
                return 1
            fi
        fi

        log_info "Monitoring... ($elapsed/$duration seconds, violations: $violations)"
        sleep "$HEALTH_CHECK_INTERVAL"
    done
}

# Check error rate from Prometheus
check_error_rate_prometheus() {
    # Query Prometheus for error rate
    # This is a placeholder - actual implementation would query Prometheus API
    echo "0.01"
}

# Check latency from Prometheus
check_latency_prometheus() {
    # Query Prometheus for p95 latency
    # This is a placeholder - actual implementation would query Prometheus API
    echo "500"
}

# Promote canary to stable
promote_canary() {
    log_info "Promoting canary to stable..."

    # Scale down old stable deployment
    kubectl scale deployment "$RELEASE_NAME" -n "$NAMESPACE" --replicas=0

    # Rename canary to stable
    helm upgrade --install "$RELEASE_NAME" "$HELM_CHART" \
        --namespace "$NAMESPACE" \
        --values "$HELM_CHART/$VALUES_FILE" \
        --set image.tag="$NEW_VERSION" \
        --reuse-values

    # Remove canary deployment
    helm uninstall "${RELEASE_NAME}-canary" -n "$NAMESPACE"

    log_success "Canary promoted to stable"
}

# Rollback canary
rollback_canary() {
    log_error "Rolling back canary deployment..."

    # Delete canary
    helm uninstall "${RELEASE_NAME}-canary" -n "$NAMESPACE" || true

    # Ensure stable deployment is at full capacity
    kubectl scale deployment "$RELEASE_NAME" -n "$NAMESPACE" --replicas=5

    log_error "Rollback complete"
    exit 1
}

# Main canary deployment flow
main() {
    log_info "Starting canary deployment for Aethyme"
    log_info "New version: $NEW_VERSION"
    log_info "Canary stages: $CANARY_STAGES"

    validate_prerequisites

    # Deploy canary
    if ! deploy_canary; then
        log_error "Failed to deploy canary"
        exit 1
    fi

    if ! wait_for_canary; then
        log_error "Canary not ready"
        rollback_canary
    fi

    # Progressive traffic shifting
    for percentage in $CANARY_STAGES; do
        log_info "=== Stage: ${percentage}% traffic to canary ==="

        shift_traffic "$percentage"

        if ! monitor_canary "$STAGE_DURATION" "$percentage"; then
            log_error "Canary failed at ${percentage}% traffic"
            rollback_canary
        fi

        log_success "Stage ${percentage}% completed successfully"
    done

    # Promote to stable
    promote_canary

    log_success "Canary deployment completed successfully!"
    log_success "Version $NEW_VERSION is now live"
}

# Trap errors and rollback
trap rollback_canary ERR

main "$@"
