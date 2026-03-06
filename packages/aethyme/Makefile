.PHONY: help dev dev-down dev-logs test test-unit test-integration test-full lint format type-check security migrate seed clean build install-dev check-all ci ps restart health metrics openapi

.DEFAULT_GOAL := help

PYTHON := python3
PIP := $(PYTHON) -m pip
PYTEST := .venv/bin/pytest
TEST_DATABASE_URL ?= postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test
DOCKER_COMPOSE := docker compose -f docker-compose.dev.yml

## help: Show this help message
help:
	@echo 'Usage:'
	@sed -n 's/^##//p' ${MAKEFILE_LIST} | column -t -s ':' | sed -e 's/^/ /'

## dev: Start PostgreSQL and Redis for local development
dev:
	$(DOCKER_COMPOSE) up -d postgres redis
	@echo "PostgreSQL: localhost:5432"
	@echo "Redis:      localhost:6379"
	@echo "API:        run 'bash scripts/start-api.sh' for http://localhost:8001"

## dev-down: Stop local development services
dev-down:
	$(DOCKER_COMPOSE) down

## dev-logs: Follow PostgreSQL and Redis logs
dev-logs:
	$(DOCKER_COMPOSE) logs -f postgres redis

## test: Run the fast unit-only suite
test:
	$(MAKE) test-unit

## test-unit: Run tests that do not require PostgreSQL
test-unit:
	$(PYTEST) -q -m "not integration"

## test-integration: Run PostgreSQL-backed integration tests
test-integration:
	TEST_DATABASE_URL="$(TEST_DATABASE_URL)" $(PYTEST) -q -m integration

## test-full: Run the full suite with unit and integration tests
test-full:
	TEST_DATABASE_URL="$(TEST_DATABASE_URL)" $(PYTEST) -q

## lint: Run Ruff
lint:
	ruff check .

## format: Format Python code
format:
	black .

## type-check: Run Pyright
type-check:
	.venv/bin/pyright --pythonpath .venv/bin/python src tests sdk/python/aethyme_sdk

## security: Run security scanners
security:
	bandit -r src/ -ll || true
	safety check --json || true
	pip-audit --desc || true

## migrate: Apply SQL migrations
migrate:
	bash scripts/migrate.sh

## seed: Seed development data
seed:
	$(PYTHON) scripts/seed_test_data.py

## clean: Remove generated files and caches
clean:
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name "*.egg-info" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name ".pytest_cache" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name ".ruff_cache" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name ".mypy_cache" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete
	rm -rf htmlcov/ .coverage coverage.xml

## build: Build the package
build: clean
	$(PYTHON) -m build

## install-dev: Install dev dependencies from pyproject and requirements-dev
install-dev:
	$(PIP) install --upgrade pip
	$(PIP) install -e '.[dev]' -r requirements-dev.txt

## check-all: Run lint, type check, and the fast unit suite
check-all: lint type-check test-unit

## ci: Run the local CI subset
ci: clean install-dev check-all

## ps: Show running development services
ps:
	$(DOCKER_COMPOSE) ps

## restart: Restart PostgreSQL and Redis
restart:
	$(DOCKER_COMPOSE) restart postgres redis

## health: Check API health if the API is running locally
health:
	@curl -f http://localhost:8001/health && echo "✓ API healthy" || echo "✗ API unhealthy"

## metrics: Show metrics if the API is running locally
metrics:
	@curl -s http://localhost:8001/metrics | head -50

## openapi: Generate the OpenAPI spec
openapi:
	$(PYTHON) scripts/generate_openapi.py > openapi.json
