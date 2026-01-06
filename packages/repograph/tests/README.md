# RepoGraph Test Suite

This directory contains the comprehensive test suite for RepoGraph, including unit tests, integration tests, and performance benchmarks.

## Table of Contents

- [Quick Start](#quick-start)
- [Test Organization](#test-organization)
- [Running Tests](#running-tests)
- [Writing Tests](#writing-tests)
- [Coverage Requirements](#coverage-requirements)
- [Performance Testing](#performance-testing)
- [Continuous Integration](#continuous-integration)

## Quick Start

```bash
# Install test dependencies
pip install -r requirements-dev.txt

# Run all tests
make test

# Run tests with coverage
pytest --cov=src --cov-report=html

# Run specific test file
pytest tests/test_auth.py -v

# Run tests matching a pattern
pytest -k "test_query" -v

# Run only fast tests (skip slow/integration)
pytest -m "not slow" -v
```

## Test Organization

```
tests/
├── conftest.py              # Shared fixtures and configuration
├── README.md                # This file
├── unit/                    # Unit tests (fast, isolated)
│   ├── test_auth.py
│   ├── test_models.py
│   ├── test_queries.py
│   └── test_utils.py
├── integration/             # Integration tests (slower, with DB/Redis)
│   ├── test_api_endpoints.py
│   ├── test_indexing_flow.py
│   └── test_rls_policies.py
├── performance/             # Performance and benchmark tests
│   ├── test_query_performance.py
│   └── test_indexing_performance.py
└── fixtures/                # Test data and fixtures
    ├── sample_repos/
    └── test_data.json
```

### Test Categories

We use pytest markers to categorize tests:

- **Unit tests** (default): Fast, isolated, no external dependencies
- **Integration tests** (`@pytest.mark.integration`): Test component interactions
- **Slow tests** (`@pytest.mark.slow`): Long-running tests
- **Auth tests** (`@pytest.mark.auth`): Authentication/authorization tests
- **RLS tests** (`@pytest.mark.rls`): Row-level security tests
- **Performance tests** (`@pytest.mark.performance`): Benchmarks and load tests

## Running Tests

### All Tests

```bash
# Using make
make test

# Using pytest directly
pytest -v
```

### By Category

```bash
# Unit tests only (fast)
pytest tests/unit/ -v

# Integration tests only
pytest tests/integration/ -v
pytest -m integration -v

# Skip slow tests
pytest -m "not slow" -v

# Auth-related tests only
pytest -m auth -v

# RLS tests only
pytest -m rls -v
```

### With Coverage

```bash
# Generate coverage report
pytest --cov=src --cov-report=html --cov-report=term

# View HTML report
open htmlcov/index.html

# Fail if coverage below 80%
pytest --cov=src --cov-fail-under=80
```

### Watch Mode

```bash
# Auto-run tests on file changes
make test-watch
pytest --looponfail
```

### Debugging

```bash
# Stop on first failure
pytest -x

# Show local variables on failure
pytest -l

# Enter debugger on failure
pytest --pdb

# Increase verbosity
pytest -vv

# Show print statements
pytest -s
```

## Writing Tests

### Basic Test Structure

```python
import pytest
from src.models import Repository

def test_repository_creation():
    """Test creating a repository instance."""
    repo = Repository(
        name="test-repo",
        org="test-org",
        language="python"
    )

    assert repo.name == "test-repo"
    assert repo.org == "test-org"
    assert repo.language == "python"
```

### Using Fixtures

```python
import pytest

@pytest.mark.asyncio
async def test_create_index(db_session, mock_repo_data):
    """Test index creation with database session."""
    # Use fixtures from conftest.py
    repo = await create_repository(db_session, mock_repo_data)
    assert repo.id is not None
```

### Async Tests

```python
import pytest

@pytest.mark.asyncio
async def test_async_query(db_session):
    """Test async database query."""
    result = await db_session.execute(
        "SELECT COUNT(*) FROM repositories"
    )
    count = result.scalar()
    assert count >= 0
```

### Parameterized Tests

```python
import pytest

@pytest.mark.parametrize("language,expected_parser", [
    ("python", "scip-python"),
    ("javascript", "scip-typescript"),
    ("go", "scip-go"),
])
def test_language_parser_mapping(language, expected_parser):
    """Test parser selection for different languages."""
    parser = get_parser_for_language(language)
    assert parser == expected_parser
```

### Testing Exceptions

```python
import pytest
from src.exceptions import UnauthorizedError

def test_unauthorized_access():
    """Test that unauthorized access raises error."""
    with pytest.raises(UnauthorizedError) as exc_info:
        access_protected_resource(token=None)

    assert "authentication required" in str(exc_info.value).lower()
```

### Testing API Endpoints

```python
import pytest

@pytest.mark.asyncio
async def test_get_repo(authenticated_client, create_test_repo):
    """Test GET /repos/{repo_id} endpoint."""
    # Create test data
    repo = await create_test_repo(name="test-repo")

    # Make request
    response = authenticated_client.get(f"/repos/{repo.id}")

    # Assert response
    assert response.status_code == 200
    data = response.json()
    assert data["name"] == "test-repo"
```

### Testing RLS (Row-Level Security)

```python
import pytest

@pytest.mark.rls
@pytest.mark.asyncio
async def test_rls_tenant_isolation(db_session, create_test_org):
    """Test that RLS prevents cross-tenant data access."""
    # Create two organizations
    org1 = await create_test_org(name="org1")
    org2 = await create_test_org(name="org2")

    # Set RLS context to org1
    await db_session.execute(
        "SET app.current_org_id = :org_id",
        {"org_id": org1.id}
    )

    # Query should only return org1 data
    repos = await get_repositories(db_session)
    assert all(r.org_id == org1.id for r in repos)
```

## Coverage Requirements

### Targets

- **Overall Coverage:** >80% (enforced in CI)
- **New Code:** 100% (goal)
- **Critical Paths:** 100% (auth, RLS, data integrity)

### Excluded from Coverage

- Migration scripts
- Development-only utilities
- Generated code
- Third-party integrations (tested via integration tests)

### Checking Coverage

```bash
# Generate coverage report
pytest --cov=src --cov-report=term-missing

# Fail if below 80%
pytest --cov=src --cov-fail-under=80

# HTML report with line-by-line coverage
pytest --cov=src --cov-report=html
open htmlcov/index.html
```

## Performance Testing

### Running Benchmarks

```bash
# All performance tests
pytest tests/performance/ -v

# Specific benchmark
pytest tests/performance/test_query_performance.py -v

# Using benchmark suite
make benchmark
```

### Writing Performance Tests

```python
import pytest

@pytest.mark.performance
def test_query_latency_under_2s(benchmark, db_session):
    """Test that queries complete in <2s (p95 target)."""
    def run_query():
        return execute_search_query(db_session, "test")

    # Benchmark automatically runs multiple iterations
    result = benchmark(run_query)

    # Assert p95 latency
    assert result.stats.percentiles.p95 < 2.0
```

### Performance Targets

| Component | Metric | Target | Test |
|-----------|--------|--------|------|
| Indexing | Duration (medium repo) | <2min | `test_indexing_performance` |
| Search Query | p95 Latency | <2s | `test_search_query_performance` |
| Ego Query | p95 Latency | <2s | `test_ego_query_performance` |
| Impact Query | p95 Latency | <2s | `test_impact_query_performance` |
| Cache | Hit Rate | >60% | `test_cache_hit_rate` |
| Auth | Success Rate | >99% | `test_auth_success_rate` |

## Continuous Integration

### CI Pipeline

Tests run automatically on:
- Pull request creation
- Push to `main` or `develop`
- Manual trigger

### CI Stages

1. **Unit Tests** (5min)
   - Fast tests without external dependencies
   - Coverage reporting

2. **Integration Tests** (10min)
   - Tests with database and Redis
   - RLS validation

3. **Performance Tests** (15min, on PR only)
   - Benchmarks vs baseline
   - Fail if regression >20%

4. **Security Tests** (2min)
   - Bandit code scanning
   - Dependency vulnerability checks

### Local CI Simulation

```bash
# Run full CI pipeline locally
make ci

# Or manually:
make clean
make install-dev
make lint
make type-check
make test
make security
```

## Test Data

### Fixtures

Test fixtures are defined in `conftest.py`:

- `db_session`: Database session with auto-rollback
- `redis_client`: Redis client with auto-cleanup
- `api_client`: FastAPI test client
- `authenticated_client`: Client with valid JWT
- `mock_repo_data`: Sample repository data
- `mock_index_data`: Sample index data

### Sample Repositories

Located in `tests/fixtures/sample_repos/`:

- `python-small/`: Small Python repo (~100 files)
- `python-medium/`: Medium Python repo (~1000 files)
- `javascript-spa/`: React SPA repository
- `monorepo/`: Multi-language monorepo

## Best Practices

### DO

- Write descriptive test names: `test_auth_fails_with_invalid_token`
- Use fixtures for common setup
- Test edge cases and error conditions
- Keep tests independent and isolated
- Use meaningful assertions
- Add docstrings to complex tests
- Mark slow/integration tests appropriately

### DON'T

- Use `sleep()` or arbitrary timeouts
- Depend on test execution order
- Leave commented-out test code
- Test implementation details
- Share mutable state between tests
- Commit failing tests

## Troubleshooting

### Tests Fail Locally but Pass in CI

```bash
# Ensure clean environment
make clean
make install-dev
make test
```

### Database Connection Errors

```bash
# Start dev environment
make dev

# Check services are running
make ps

# Check database is accessible
make db-shell
```

### Redis Connection Errors

```bash
# Check Redis is running
make redis-shell

# Flush test Redis database
docker exec repograph-redis-dev redis-cli -n 1 FLUSHDB
```

### Slow Tests

```bash
# Profile slow tests
pytest --durations=10

# Run only fast tests
pytest -m "not slow"
```

## Resources

- [pytest documentation](https://docs.pytest.org/)
- [pytest-asyncio](https://pytest-asyncio.readthedocs.io/)
- [pytest-cov](https://pytest-cov.readthedocs.io/)
- [FastAPI testing](https://fastapi.tiangolo.com/tutorial/testing/)

## Questions?

If you have questions about testing:
1. Check this README
2. Review existing tests for examples
3. Ask in #repograph-dev Slack channel
