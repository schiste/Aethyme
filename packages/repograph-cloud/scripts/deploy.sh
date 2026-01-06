#!/bin/bash
# ==============================================================================
# RepoGraph Cloud - Production Deployment Script
# ==============================================================================

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
NAMESPACE="repograph-cloud"
APP_NAME="repograph-api"
IMAGE_TAG="${IMAGE_TAG:-latest}"
TIMEOUT="${TIMEOUT:-300s}"

# Functions
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

check_prerequisites() {
    log_info "Checking prerequisites..."

    # Check kubectl
    if ! command -v kubectl &> /dev/null; then
        log_error "kubectl not found. Please install kubectl first."
        exit 1
    fi

    # Check kubectl connection
    if ! kubectl cluster-info &> /dev/null; then
        log_error "Cannot connect to Kubernetes cluster. Check your kubeconfig."
        exit 1
    fi

    # Check namespace exists
    if ! kubectl get namespace "$NAMESPACE" &> /dev/null; then
        log_warn "Namespace $NAMESPACE does not exist. Creating..."
        kubectl create namespace "$NAMESPACE"
    fi

    log_info "Prerequisites check passed ✓"
}

create_secrets() {
    log_info "Setting up secrets..."

    # Check if .env.production exists
    if [ ! -f ".env.production" ]; then
        log_error ".env.production file not found!"
        log_error "Copy .env.production.example to .env.production and fill in values."
        exit 1
    fi

    # Source environment variables
    set -a
    source .env.production
    set +a

    # Create database credentials secret
    if ! kubectl get secret db-credentials -n "$NAMESPACE" &> /dev/null; then
        log_info "Creating db-credentials secret..."
        kubectl create secret generic db-credentials \
            --from-literal=url="$DATABASE_URL" \
            -n "$NAMESPACE"
    else
        log_info "db-credentials secret already exists"
    fi

    # Create JWT secrets
    if ! kubectl get secret jwt-secrets -n "$NAMESPACE" &> /dev/null; then
        log_info "Creating jwt-secrets secret..."
        kubectl create secret generic jwt-secrets \
            --from-literal=secret-key="$JWT_SECRET_KEY" \
            --from-literal=refresh-secret="$REFRESH_TOKEN_SECRET_KEY" \
            -n "$NAMESPACE"
    else
        log_info "jwt-secrets secret already exists"
    fi

    # Create encryption key secret
    if ! kubectl get secret encryption-key -n "$NAMESPACE" &> /dev/null; then
        log_info "Creating encryption-key secret..."
        kubectl create secret generic encryption-key \
            --from-literal=key="$ENCRYPTION_KEY" \
            -n "$NAMESPACE"
    else
        log_info "encryption-key secret already exists"
    fi

    # Create Sentry DSN secret (optional)
    if [ -n "${SENTRY_DSN:-}" ]; then
        if ! kubectl get secret sentry-dsn -n "$NAMESPACE" &> /dev/null; then
            log_info "Creating sentry-dsn secret..."
            kubectl create secret generic sentry-dsn \
                --from-literal=dsn="$SENTRY_DSN" \
                -n "$NAMESPACE"
        fi
    fi

    log_info "Secrets setup completed ✓"
}

apply_manifests() {
    log_info "Applying Kubernetes manifests..."

    # Apply in order
    kubectl apply -f k8s/namespace.yaml
    kubectl apply -f k8s/configmap.yaml
    kubectl apply -f k8s/api-deployment.yaml
    kubectl apply -f k8s/api-service.yaml
    kubectl apply -f k8s/api-hpa.yaml
    kubectl apply -f k8s/worker-deployment.yaml

    # Apply ingress if it exists
    if [ -f "k8s/ingress.yaml" ]; then
        kubectl apply -f k8s/ingress.yaml
    fi

    log_info "Manifests applied ✓"
}

wait_for_rollout() {
    log_info "Waiting for deployment rollout..."

    if kubectl rollout status deployment/"$APP_NAME" -n "$NAMESPACE" --timeout="$TIMEOUT"; then
        log_info "Deployment rollout successful ✓"
    else
        log_error "Deployment rollout failed!"
        log_error "Rolling back..."
        kubectl rollout undo deployment/"$APP_NAME" -n "$NAMESPACE"
        exit 1
    fi
}

health_check() {
    log_info "Running health checks..."

    # Wait for pods to be ready
    if ! kubectl wait --for=condition=Ready pod \
        -l app="$APP_NAME" \
        -n "$NAMESPACE" \
        --timeout=120s; then
        log_error "Pods not ready after 2 minutes"
        exit 1
    fi

    # Get service endpoint
    SERVICE_IP=$(kubectl get service "$APP_NAME" -n "$NAMESPACE" -o jsonpath='{.status.loadBalancer.ingress[0].ip}')

    if [ -z "$SERVICE_IP" ]; then
        log_warn "LoadBalancer IP not yet assigned, using port-forward for health check..."
        kubectl port-forward -n "$NAMESPACE" service/"$APP_NAME" 8080:80 &
        PORT_FORWARD_PID=$!
        sleep 5
        HEALTH_URL="http://localhost:8080/health"
    else
        HEALTH_URL="http://$SERVICE_IP/health"
    fi

    # Health check
    if curl -f "$HEALTH_URL" &> /dev/null; then
        log_info "Health check passed ✓"
    else
        log_error "Health check failed!"
        [ -n "${PORT_FORWARD_PID:-}" ] && kill "$PORT_FORWARD_PID" 2>/dev/null || true
        exit 1
    fi

    # Clean up port-forward if used
    [ -n "${PORT_FORWARD_PID:-}" ] && kill "$PORT_FORWARD_PID" 2>/dev/null || true

    log_info "Deployment successful ✓"
}

show_status() {
    log_info "Deployment status:"
    kubectl get pods -n "$NAMESPACE" -l app="$APP_NAME"
    echo ""
    kubectl get service "$APP_NAME" -n "$NAMESPACE"
    echo ""
    log_info "To view logs: kubectl logs -f deployment/$APP_NAME -n $NAMESPACE"
    log_info "To get service URL: kubectl get service $APP_NAME -n $NAMESPACE"
}

# Main deployment flow
main() {
    log_info "Starting deployment of $APP_NAME to $NAMESPACE namespace..."
    echo ""

    check_prerequisites
    create_secrets
    apply_manifests
    wait_for_rollout
    health_check
    show_status

    echo ""
    log_info "🎉 Deployment completed successfully!"
}

# Run main function
main "$@"
