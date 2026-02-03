.PHONY: help dev dev-down dev-logs test test-watch test-integration lint format type-check security benchmark migrate migrate-create migrate-down seed clean build docker-build docker-push install-dev check-all ci

# Default target
.DEFAULT_GOAL := help

# Configuration
PYTHON := python3
PIP := $(PYTHON) -m pip
PYTEST := pytest
DOCKER_COMPOSE := docker-compose -f docker-compose.dev.yml
PROJECT_NAME := aethyme
COVERAGE_MIN := 80

## help: Show this help message
help:
	@echo 'Usage:'
	@sed -n 's/^##//p' ${MAKEFILE_LIST} | column -t -s ':' | sed -e 's/^/ /'

## dev: Start development environment with hot reload
dev:
	@echo "Starting development environment..."
	$(DOCKER_COMPOSE) up -d
	@echo "Services started:"
	@echo "  - API:               http://localhost:8000"
	@echo "  - API Docs:          http://localhost:8000/docs"
	@echo "  - Grafana:           http://localhost:3000 (admin/admin)"
	@echo "  - Prometheus:        http://localhost:9090"
	@echo "  - Jaeger UI:         http://localhost:16686"
	@echo "  - PgAdmin:           http://localhost:5050 (admin@aethyme.com/admin)"
	@echo "  - Redis Commander:   http://localhost:8081"
	@echo ""
	@echo "Run 'make dev-logs' to see logs"
	@echo "Run 'make dev-down' to stop all services"

## dev-down: Stop development environment
dev-down:
	@echo "Stopping development environment..."
	$(DOCKER_COMPOSE) down -v

## dev-logs: Follow logs from development environment
dev-logs:
	$(DOCKER_COMPOSE) logs -f

## test: Run test suite with coverage
test:
	@echo "Running tests with coverage..."
	$(PYTEST) -v --cov=src --cov-report=term --cov-report=html --cov-report=xml
	@echo ""
	@echo "Coverage report generated at htmlcov/index.html"

## test-watch: Run tests in watch mode
test-watch:
	@echo "Running tests in watch mode..."
	$(PYTEST) -v --cov=src --looponfail

## test-integration: Run integration tests only
test-integration:
	@echo "Running integration tests..."
	$(PYTEST) tests/integration/ -v --tb=short

## lint: Run all linters
lint:
	@echo "Running linters..."
	ruff check .
	@echo "Linting complete"

## format: Auto-format code
format:
	@echo "Formatting code..."
	black .
	isort .
	@echo "Formatting complete"

## type-check: Run type checking
type-check:
	@echo "Running type checker..."
	mypy src/

## security: Run security checks
security:
	@echo "Running security scans..."
	@echo "1. Bandit (code security)..."
	bandit -r src/ -ll
	@echo ""
	@echo "2. Safety (dependency vulnerabilities)..."
	safety check --json || true
	@echo ""
	@echo "3. pip-audit (package vulnerabilities)..."
	pip-audit --desc || true

## benchmark: Run performance benchmarks
benchmark:
	@echo "Running performance benchmarks..."
	@echo "1. Indexing benchmark..."
	$(PYTHON) benchmarks/indexing_benchmark.py
	@echo ""
	@echo "2. Query benchmark..."
	$(PYTHON) benchmarks/query_benchmark.py
	@echo ""
	@echo "3. Cache benchmark..."
	$(PYTHON) benchmarks/cache_benchmark.py

## migrate: Run database migrations
migrate:
	@echo "Running database migrations..."
	alembic upgrade head
	@echo "Migrations complete"

## migrate-create: Create a new migration
migrate-create:
	@read -p "Enter migration message: " msg; \
	alembic revision --autogenerate -m "$$msg"

## migrate-down: Rollback one migration
migrate-down:
	@echo "Rolling back one migration..."
	alembic downgrade -1

## seed: Seed database with test data
seed:
	@echo "Seeding database with test data..."
	$(PYTHON) scripts/seed_test_data.py
	@echo "Database seeded"

## clean: Remove generated files and caches
clean:
	@echo "Cleaning up..."
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name "*.egg-info" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name ".pytest_cache" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name ".ruff_cache" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name ".mypy_cache" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete
	rm -rf htmlcov/ .coverage coverage.xml
	@echo "Cleanup complete"

## build: Build Python package
build: clean
	@echo "Building package..."
	$(PYTHON) -m build
	@echo "Build complete"

## docker-build: Build Docker image
docker-build:
	@echo "Building Docker image..."
	docker build -t $(PROJECT_NAME):latest -t $(PROJECT_NAME):dev .
	@echo "Docker image built"

## docker-push: Push Docker image to registry
docker-push:
	@echo "Pushing Docker image..."
	docker push $(PROJECT_NAME):latest
	@echo "Docker image pushed"

## install-dev: Install development dependencies
install-dev:
	@echo "Installing development dependencies..."
	$(PIP) install --upgrade pip
	$(PIP) install -r requirements.txt
	$(PIP) install -r requirements-dev.txt
	$(PIP) install -e .
	@echo "Dependencies installed"

## check-all: Run all checks (lint, type, test, security)
check-all: lint type-check test security
	@echo ""
	@echo "All checks passed!"

## ci: Run CI pipeline locally
ci: clean install-dev check-all
	@echo ""
	@echo "CI pipeline completed successfully!"

## db-shell: Open PostgreSQL shell
db-shell:
	docker exec -it aethyme-postgres-dev psql -U aethyme -d aethyme_dev

## redis-shell: Open Redis CLI
redis-shell:
	docker exec -it aethyme-redis-dev redis-cli

## api-shell: Open API container shell
api-shell:
	docker exec -it aethyme-api-dev /bin/bash

## logs-api: Show API logs
logs-api:
	$(DOCKER_COMPOSE) logs -f api

## logs-worker: Show worker logs
logs-worker:
	$(DOCKER_COMPOSE) logs -f worker

## ps: Show running services
ps:
	$(DOCKER_COMPOSE) ps

## restart: Restart all services
restart:
	$(DOCKER_COMPOSE) restart

## db-backup: Backup database
db-backup:
	@echo "Creating database backup..."
	docker exec aethyme-postgres-dev pg_dump -U aethyme aethyme_dev > backup_$(shell date +%Y%m%d_%H%M%S).sql
	@echo "Backup created"

## db-restore: Restore database from backup
db-restore:
	@read -p "Enter backup file path: " file; \
	docker exec -i aethyme-postgres-dev psql -U aethyme aethyme_dev < $$file
	@echo "Database restored"

## health: Check health of all services
health:
	@echo "Checking service health..."
	@curl -f http://localhost:8000/health && echo "✓ API healthy" || echo "✗ API unhealthy"
	@curl -f http://localhost:9090/-/healthy && echo "✓ Prometheus healthy" || echo "✗ Prometheus unhealthy"
	@curl -f http://localhost:3000/api/health && echo "✓ Grafana healthy" || echo "✗ Grafana unhealthy"

## metrics: Show current metrics
metrics:
	@curl -s http://localhost:8000/metrics | head -50

## openapi: Generate OpenAPI spec
openapi:
	@echo "Generating OpenAPI specification..."
	$(PYTHON) scripts/generate_openapi.py > openapi.json
	@echo "OpenAPI spec written to openapi.json"
