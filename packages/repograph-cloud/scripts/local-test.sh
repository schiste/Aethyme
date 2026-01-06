#!/bin/bash
# ==============================================================================
# RepoGraph Cloud - Local Production Testing Script
# Tests the production Docker setup locally before deploying
# ==============================================================================

set -euo pipefail

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

check_prerequisites() {
    log_info "Checking prerequisites..."

    if ! command -v docker &> /dev/null; then
        log_error "Docker not found. Please install Docker first."
        exit 1
    fi

    if ! command -v docker-compose &> /dev/null; then
        log_error "docker-compose not found. Please install docker-compose first."
        exit 1
    fi

    log_info "Prerequisites check passed ✓"
}

setup_environment() {
    log_info "Setting up environment..."

    if [ ! -f ".env.production" ]; then
        log_warn ".env.production not found. Creating from example..."
        cp .env.production.example .env.production

        # Generate test secrets
        JWT_SECRET=$(openssl rand -hex 32)
        REFRESH_SECRET=$(openssl rand -hex 32)
        ENCRYPTION_KEY=$(python3 -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())")
        POSTGRES_PASSWORD=$(openssl rand -hex 16)

        # Update .env.production
        sed -i.bak "s/REPLACE_WITH_64_CHAR_HEX_STRING/$JWT_SECRET/" .env.production
        sed -i.bak "s/REPLACE_WITH_ANOTHER_64_CHAR_HEX_STRING/$REFRESH_SECRET/" .env.production
        sed -i.bak "s/REPLACE_WITH_FERNET_KEY/$ENCRYPTION_KEY/" .env.production
        sed -i.bak "s/POSTGRES_PASSWORD=.*/POSTGRES_PASSWORD=$POSTGRES_PASSWORD/" .env.production

        log_warn "Generated test secrets in .env.production"
        log_warn "DO NOT use these in production!"
    fi

    log_info "Environment setup complete ✓"
}

build_images() {
    log_info "Building Docker images..."

    if docker-compose -f docker-compose.production.yml build; then
        log_info "Docker images built ✓"
    else
        log_error "Docker build failed!"
        exit 1
    fi
}

start_services() {
    log_info "Starting services..."

    if docker-compose -f docker-compose.production.yml up -d; then
        log_info "Services started ✓"
    else
        log_error "Failed to start services!"
        exit 1
    fi
}

wait_for_services() {
    log_info "Waiting for services to be ready..."

    local max_attempts=60
    local attempt=0

    while [ $attempt -lt $max_attempts ]; do
        if curl -f http://localhost:8000/health &> /dev/null; then
            log_info "API is ready ✓"
            return 0
        fi

        attempt=$((attempt + 1))
        echo -n "."
        sleep 2
    done

    echo ""
    log_error "Services did not become ready in time"
    return 1
}

run_health_checks() {
    log_info "Running health checks..."

    # Basic health check
    if curl -f http://localhost:8000/health; then
        log_info "Basic health check passed ✓"
    else
        log_error "Basic health check failed!"
        return 1
    fi

    echo ""

    # Detailed health check
    if curl -f http://localhost:8000/health/detailed | jq .; then
        log_info "Detailed health check passed ✓"
    else
        log_error "Detailed health check failed!"
        return 1
    fi

    echo ""

    # Readiness check
    if curl -f http://localhost:8000/health/ready; then
        log_info "Readiness check passed ✓"
    else
        log_error "Readiness check failed!"
        return 1
    fi

    log_info "All health checks passed ✓"
}

run_smoke_tests() {
    log_info "Running smoke tests..."

    # Test API root
    if curl -f http://localhost:8000/ | jq .; then
        log_info "API root accessible ✓"
    else
        log_error "API root not accessible!"
        return 1
    fi

    # Check security headers
    log_info "Checking security headers..."
    HEADERS=$(curl -I http://localhost:8000/ 2>/dev/null)

    if echo "$HEADERS" | grep -q "X-Frame-Options: DENY"; then
        log_info "X-Frame-Options header present ✓"
    else
        log_warn "X-Frame-Options header missing"
    fi

    if echo "$HEADERS" | grep -q "X-Content-Type-Options: nosniff"; then
        log_info "X-Content-Type-Options header present ✓"
    else
        log_warn "X-Content-Type-Options header missing"
    fi

    log_info "Smoke tests completed ✓"
}

show_logs() {
    log_info "Recent logs from API:"
    docker-compose -f docker-compose.production.yml logs --tail=20 api
}

show_status() {
    log_info "Service status:"
    docker-compose -f docker-compose.production.yml ps
}

cleanup() {
    log_warn "Cleaning up..."
    log_warn "To stop services: docker-compose -f docker-compose.production.yml down"
    log_warn "To view logs: docker-compose -f docker-compose.production.yml logs -f"
}

main() {
    log_info "Starting local production test..."
    echo ""

    check_prerequisites
    setup_environment
    build_images
    start_services

    if wait_for_services; then
        run_health_checks
        run_smoke_tests
        show_status
        echo ""
        log_info "🎉 Local production test passed!"
        cleanup
    else
        log_error "Services failed to start properly"
        show_logs
        exit 1
    fi
}

main "$@"
