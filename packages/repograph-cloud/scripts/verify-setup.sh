#!/bin/bash
# RepoGraph Cloud - Setup Verification Script
# Verifies that the environment is correctly configured for development

set -e

echo "🔍 RepoGraph Cloud - Setup Verification"
echo "========================================"
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check functions
check_command() {
    if command -v $1 &> /dev/null; then
        echo -e "${GREEN}✓${NC} $1 installed"
        if [ ! -z "$2" ]; then
            echo "  $($1 $2 2>&1 | head -1)"
        fi
        return 0
    else
        echo -e "${RED}✗${NC} $1 not found"
        return 1
    fi
}

check_docker_service() {
    if docker-compose ps | grep -q "$1.*Up"; then
        echo -e "${GREEN}✓${NC} $1 is running"
        return 0
    else
        echo -e "${RED}✗${NC} $1 is not running"
        return 1
    fi
}

# Prerequisites
echo "📋 Checking Prerequisites..."
echo ""

PREREQS_OK=true

check_command "node" "--version" || PREREQS_OK=false
check_command "python3" "--version" || PREREQS_OK=false
check_command "pnpm" "--version" || PREREQS_OK=false
check_command "docker" "--version" || PREREQS_OK=false
check_command "docker-compose" "--version" || PREREQS_OK=false

echo ""

# Check if Docker is running
echo "🐳 Checking Docker..."
echo ""

if docker info &> /dev/null; then
    echo -e "${GREEN}✓${NC} Docker daemon is running"
else
    echo -e "${RED}✗${NC} Docker daemon is not running"
    echo "  Please start Docker Desktop"
    PREREQS_OK=false
fi

echo ""

# Check Docker services
echo "🔧 Checking Services..."
echo ""

SERVICES_OK=true

if [ -f "docker-compose.yml" ]; then
    check_docker_service "postgres" || SERVICES_OK=false
    check_docker_service "redis" || SERVICES_OK=false
    check_docker_service "elasticsearch" || SERVICES_OK=false
else
    echo -e "${YELLOW}⚠${NC}  docker-compose.yml not found"
    echo "  Run: docker-compose up -d"
    SERVICES_OK=false
fi

echo ""

# Check environment file
echo "⚙️  Checking Configuration..."
echo ""

if [ -f ".env" ]; then
    echo -e "${GREEN}✓${NC} .env file exists"

    # Check for required variables
    if grep -q "JWT_SECRET_KEY=your-super-secret" .env; then
        echo -e "${YELLOW}⚠${NC}  JWT_SECRET_KEY not configured"
        echo "  Run: openssl rand -hex 32"
    else
        echo -e "${GREEN}✓${NC} JWT_SECRET_KEY configured"
    fi
else
    echo -e "${RED}✗${NC} .env file not found"
    echo "  Run: cp .env.example .env"
fi

echo ""

# Check dependencies
echo "📦 Checking Dependencies..."
echo ""

DEPS_OK=true

if [ -d "node_modules" ]; then
    echo -e "${GREEN}✓${NC} Node modules installed"
else
    echo -e "${YELLOW}⚠${NC}  Node modules not installed"
    echo "  Run: pnpm install"
    DEPS_OK=false
fi

if [ -d "apps/api/.venv" ]; then
    echo -e "${GREEN}✓${NC} Python virtual environment exists"
else
    echo -e "${YELLOW}⚠${NC}  Python virtual environment not found"
    echo "  Run: cd apps/api && python3 -m venv .venv"
    DEPS_OK=false
fi

echo ""

# Test connectivity
echo "🌐 Testing Connectivity..."
echo ""

test_endpoint() {
    if curl -s -f "$1" > /dev/null 2>&1; then
        echo -e "${GREEN}✓${NC} $2 accessible at $1"
        return 0
    else
        echo -e "${RED}✗${NC} $2 not accessible at $1"
        return 1
    fi
}

CONNECTIVITY_OK=true

if $SERVICES_OK; then
    test_endpoint "http://localhost:5433" "PostgreSQL" || CONNECTIVITY_OK=false
    test_endpoint "http://localhost:6380" "Redis" || CONNECTIVITY_OK=false
    test_endpoint "http://localhost:9201" "Elasticsearch" || CONNECTIVITY_OK=false
fi

echo ""

# Summary
echo "========================================"
echo "📊 Summary"
echo "========================================"
echo ""

if $PREREQS_OK && $SERVICES_OK && $DEPS_OK && $CONNECTIVITY_OK; then
    echo -e "${GREEN}✅ All checks passed!${NC}"
    echo ""
    echo "You're ready to start development:"
    echo "  pnpm dev"
    echo ""
    exit 0
else
    echo -e "${YELLOW}⚠️  Some checks failed${NC}"
    echo ""
    echo "Fix the issues above and run this script again:"
    echo "  ./scripts/verify-setup.sh"
    echo ""
    echo "Or see GETTING_STARTED.md for detailed setup instructions."
    echo ""
    exit 1
fi
