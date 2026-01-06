#!/usr/bin/env bash
set -euo pipefail

#
# Blue-Green Deployment Script for RepoGraph
#
# This script performs a zero-downtime blue-green deployment:
# 1. Deploy new version (green) alongside existing version (blue)
# 2. Run health checks on green deployment
# 3. Switch traffic from blue to green
# 4. Monitor green deployment
# 5. Scale down blue deployment (or rollback if issues detected)
#

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Configuration
NAMESPACE="${NAMESPACE:-default}"
RELEASE_NAME="${RELEASE_NAME:-repograph}"
HELM_CHART="${HELM_CHART:-$PROJECT_ROOT/k8s/helm/repograph}"
VALUES_FILE="${VALUES_FILE:-values-production.yaml}"
NEW_VERSION="${NEW_VERSION:-}"
HEALTH_CHECK_TIMEOUT="${HEALTH_CHECK_TIMEOUT:-300}"
HEALTH_CHECK_INTERVAL="${HEALTH_CHECK_INTERVAL:-10}"
MONITORING_PERIOD="${MONITORING_PERIOD:-300}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

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
        log_error "kubectl not found. Please install kubectl."
        exit 1
    fi

    if ! command -v helm &> /dev/null; then
        log_error "helm not found. Please install Helm 3."
        exit 1
    fi

    if [[ -z "$NEW_VERSION" ]]; then
        log_error "NEW_VERSION environment variable must be set"
        exit 1
    fi

    log_success "Prerequisites validated"
}

# Get current deployment info
get_current_deployment() {
    log_info "Getting current deployment information..."

    CURRENT_VERSION=$(kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" \
        -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null | cut -d: -f2 || echo "none")

    CURRENT_REPLICAS=$(kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" \
        -o jsonpath='{.spec.replicas}' 2>/dev/null || echo "0")

    log_info "Current version: $CURRENT_VERSION"
    log_info "Current replicas: $CURRENT_REPLICAS"
}

# Deploy green environment
deploy_green() {
    log_info "Deploying green environment with version $NEW_VERSION..."

    # Create green deployment
    helm upgrade --install "${RELEASE_NAME}-green" "$HELM_CHART" \
        --namespace "$NAMESPACE" \
        --create-namespace \
        --values "$HELM_CHART/$VALUES_FILE" \
        --set image.tag="$NEW_VERSION" \
        --set fullnameOverride="${RELEASE_NAME}-green" \
        --set service.selector.version="green" \
        --wait \
        --timeout 10m

    log_success "Green deployment created"
}

# Wait for green deployment to be ready
wait_for_green_ready() {
    log_info "Waiting for green deployment to be ready..."

    kubectl wait --for=condition=available \
        --timeout="${HEALTH_CHECK_TIMEOUT}s" \
        deployment/"${RELEASE_NAME}-green" \
        -n "$NAMESPACE"

    log_success "Green deployment is ready"
}

# Run health checks on green deployment
health_check_green() {
    log_info "Running health checks on green deployment..."

    local elapsed=0
    local healthy=false

    while [[ $elapsed -lt $HEALTH_CHECK_TIMEOUT ]]; do
        # Get a pod from green deployment
        GREEN_POD=$(kubectl get pods -n "$NAMESPACE" \
            -l "app.kubernetes.io/instance=${RELEASE_NAME}-green" \
            -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")

        if [[ -z "$GREEN_POD" ]]; then
            log_warning "No green pods found yet, waiting..."
            sleep "$HEALTH_CHECK_INTERVAL"
            elapsed=$((elapsed + HEALTH_CHECK_INTERVAL))
            continue
        fi

        # Check readiness probe
        if kubectl exec -n "$NAMESPACE" "$GREEN_POD" -- \
            curl -sf http://localhost:8000/health/ready > /dev/null 2>&1; then
            log_success "Green deployment health check passed"
            healthy=true
            break
        else
            log_warning "Health check failed, retrying... ($elapsed/$HEALTH_CHECK_TIMEOUT seconds)"
            sleep "$HEALTH_CHECK_INTERVAL"
            elapsed=$((elapsed + HEALTH_CHECK_INTERVAL))
        fi
    done

    if [[ "$healthy" == "false" ]]; then
        log_error "Green deployment failed health checks after $HEALTH_CHECK_TIMEOUT seconds"
        return 1
    fi

    return 0
}

# Run smoke tests
run_smoke_tests() {
    log_info "Running smoke tests on green deployment..."

    # Get green service endpoint
    GREEN_SERVICE="${RELEASE_NAME}-green"

    # Port forward to green service for testing
    kubectl port-forward -n "$NAMESPACE" "service/$GREEN_SERVICE" 8080:80 &
    PF_PID=$!
    sleep 5

    # Run basic API tests
    if curl -sf http://localhost:8080/health/detailed > /dev/null; then
        log_success "Smoke tests passed"
        kill $PF_PID 2>/dev/null || true
        return 0
    else
        log_error "Smoke tests failed"
        kill $PF_PID 2>/dev/null || true
        return 1
    fi
}

# Switch traffic from blue to green
switch_traffic() {
    log_info "Switching traffic from blue to green..."

    # Update main service to point to green deployment
    kubectl patch service "$RELEASE_NAME" -n "$NAMESPACE" \
        -p '{"spec":{"selector":{"version":"green"}}}'

    log_success "Traffic switched to green deployment"
}

# Monitor green deployment
monitor_green() {
    log_info "Monitoring green deployment for $MONITORING_PERIOD seconds..."

    local start_time=$(date +%s)
    local error_count=0
    local max_errors=5

    while true; do
        local current_time=$(date +%s)
        local elapsed=$((current_time - start_time))

        if [[ $elapsed -ge $MONITORING_PERIOD ]]; then
            log_success "Monitoring period complete, deployment stable"
            return 0
        fi

        # Check pod status
        local failing_pods=$(kubectl get pods -n "$NAMESPACE" \
            -l "app.kubernetes.io/instance=${RELEASE_NAME}-green" \
            -o json | jq -r '.items[] | select(.status.phase != "Running") | .metadata.name' | wc -l)

        if [[ $failing_pods -gt 0 ]]; then
            error_count=$((error_count + 1))
            log_warning "Detected $failing_pods failing pods (error count: $error_count/$max_errors)"

            if [[ $error_count -ge $max_errors ]]; then
                log_error "Too many errors detected during monitoring"
                return 1
            fi
        else
            # Reset error count if no failures
            error_count=0
        fi

        sleep 30
    done
}

# Scale down blue deployment
scale_down_blue() {
    log_info "Scaling down blue deployment..."

    if kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" &> /dev/null; then
        kubectl scale deployment "$RELEASE_NAME" -n "$NAMESPACE" --replicas=0
        log_success "Blue deployment scaled to 0 replicas"
    else
        log_info "No blue deployment found to scale down"
    fi
}

# Rollback to blue deployment
rollback() {
    log_error "Rolling back to blue deployment..."

    # Switch traffic back to blue
    kubectl patch service "$RELEASE_NAME" -n "$NAMESPACE" \
        -p '{"spec":{"selector":{"version":"blue"}}}' || true

    # Scale blue back up if it exists
    if kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" &> /dev/null; then
        kubectl scale deployment "$RELEASE_NAME" -n "$NAMESPACE" --replicas="$CURRENT_REPLICAS"
        log_info "Waiting for blue deployment to be ready..."
        kubectl wait --for=condition=available --timeout=300s \
            deployment/"$RELEASE_NAME" -n "$NAMESPACE" || true
    fi

    # Delete green deployment
    helm uninstall "${RELEASE_NAME}-green" -n "$NAMESPACE" || true

    log_error "Rollback complete"
    exit 1
}

# Cleanup old blue deployment
cleanup() {
    log_info "Cleaning up old blue deployment..."

    if kubectl get deployment "$RELEASE_NAME" -n "$NAMESPACE" &> /dev/null; then
        helm uninstall "$RELEASE_NAME" -n "$NAMESPACE" || true
        log_success "Old blue deployment removed"
    fi

    # Rename green to blue for next deployment
    helm upgrade --install "$RELEASE_NAME" "$HELM_CHART" \
        --namespace "$NAMESPACE" \
        --values "$HELM_CHART/$VALUES_FILE" \
        --set image.tag="$NEW_VERSION" \
        --set service.selector.version="blue" \
        --reuse-values

    log_success "Green deployment renamed to blue"
}

# Main deployment flow
main() {
    log_info "Starting blue-green deployment for RepoGraph"
    log_info "New version: $NEW_VERSION"
    log_info "Namespace: $NAMESPACE"

    validate_prerequisites
    get_current_deployment

    # Deploy and verify green
    if ! deploy_green; then
        log_error "Failed to deploy green environment"
        exit 1
    fi

    if ! wait_for_green_ready; then
        log_error "Green deployment not ready"
        rollback
    fi

    if ! health_check_green; then
        log_error "Green deployment failed health checks"
        rollback
    fi

    if ! run_smoke_tests; then
        log_error "Smoke tests failed"
        rollback
    fi

    # Switch traffic
    switch_traffic

    # Monitor and finalize
    if ! monitor_green; then
        log_error "Green deployment unstable during monitoring"
        rollback
    fi

    scale_down_blue
    cleanup

    log_success "Blue-green deployment completed successfully!"
    log_success "Version $NEW_VERSION is now live"
}

# Trap errors and rollback
trap rollback ERR

main "$@"
