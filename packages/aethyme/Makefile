.PHONY: help test lint format type-check security clean build install-dev check-all ci rust-test rust-fmt rust-check

.DEFAULT_GOAL := help

PYTHON := python3
PIP := $(PYTHON) -m pip
PYTEST := $(PYTHON) -m pytest

## help: Show this help message
help:
	@echo 'Usage:'
	@sed -n 's/^##//p' ${MAKEFILE_LIST} | column -t -s ':' | sed -e 's/^/ /'

## test: Run the test suite (local-first, no services required)
test:
	$(PYTEST) -q

## lint: Run Ruff
lint:
	ruff check .

## format: Format Python code
format:
	black .

## type-check: Run Pyright
type-check:
	.venv/bin/pyright --pythonpath .venv/bin/python src tests

## security: Run security scanners
security:
	bandit -r src/ -ll || true
	safety check --json || true
	pip-audit --desc || true

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

## check-all: Run lint, type check, and the test suite
check-all: lint type-check test

## ci: Run the local CI subset
ci: clean install-dev check-all

## rust-test: Run Rust engine tests
rust-test:
	cargo test --manifest-path rust/Cargo.toml

## rust-fmt: Format Rust engine code
rust-fmt:
	cargo fmt --manifest-path rust/Cargo.toml --all

## rust-check: Check Rust engine code
rust-check:
	cargo check --manifest-path rust/Cargo.toml
