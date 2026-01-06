# Testing Quick Start Guide

**Status**: Week 13 Complete - 43/44 tests passing (97.7%)

## Quick Commands

### Run All Tests
```bash
cd packages/repograph-cloud/apps/api
source venv/bin/activate
export PYTHONPATH=$PWD
pytest tests/api/ -v
```

### Run Specific Test Suite
```bash
# Dashboard tests (10/10 passing)
pytest tests/api/test_dashboard.py -v

# Repository tests (18/19 passing)
pytest tests/api/test_repositories.py -v

# Search tests (15/15 passing)
pytest tests/api/test_search.py -v
```

### Run with Coverage
```bash
pytest tests/api/ --cov=app --cov-report=html
open coverage_html/index.html  # View coverage report
```

### Run Single Test
```bash
pytest tests/api/test_dashboard.py::TestDashboardStats::test_get_dashboard_stats_success -v
```

### Run Fast (No Coverage)
```bash
pytest tests/api/ -v --no-cov
```

## Test Database Setup

### One-Time Setup
```bash
# Create test database
docker exec repograph-postgres psql -U repograph -c "CREATE DATABASE repograph_test;"
```

### Reset Database (if needed)
```bash
docker exec repograph-postgres psql -U repograph -d repograph_test -c "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;"
```

## Test Results Summary

| Suite | Tests | Passed | Status |
|-------|-------|--------|--------|
| Dashboard | 10 | 10 | ✅ 100% |
| Repositories | 19 | 18 | ✅ 94.7% |
| Search | 15 | 15 | ✅ 100% |
| **Total** | **44** | **43** | **✅ 97.7%** |

## Test Structure

```
tests/
├── conftest.py           # Shared fixtures (220+ lines)
├── api/
│   ├── __init__.py
│   ├── test_dashboard.py      # 10 tests (280+ lines)
│   ├── test_repositories.py   # 19 tests (250+ lines)
│   └── test_search.py         # 15 tests (240+ lines)
```

## Key Fixtures

```python
# Use in your tests
async def test_example(
    authenticated_async_client: AsyncClient,  # HTTP client with auth
    test_repository: Repository,  # Single test repository
    db_session: AsyncSession,  # Database session
):
    response = await authenticated_async_client.get("/api/dashboard/stats")
    assert response.status_code == 200
```

### Available Fixtures

- `db_session` - Clean database session per test
- `test_organization` - Test organization
- `test_user` - Test user with auth
- `test_user_token` - JWT token
- `authenticated_async_client` - Authenticated HTTP client
- `test_repository` - Single test repository
- `multiple_test_repositories` - 5 test repositories

## Troubleshooting

### Tests Failing with "relation does not exist"
```bash
# Reset test database
docker exec repograph-postgres psql -U repograph -d repograph_test -c "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;"
```

### Tests Failing with "duplicate key"
The cleanup fixture should handle this, but if it persists:
```bash
# Manually clean tables
docker exec repograph-postgres psql -U repograph -d repograph_test -c "
DELETE FROM github_accounts;
DELETE FROM api_keys;
DELETE FROM repositories;
DELETE FROM users;
DELETE FROM organizations;
"
```

### Import Errors
```bash
# Ensure PYTHONPATH is set
export PYTHONPATH=$PWD

# Or use pytest from the api directory
cd packages/repograph-cloud/apps/api
pytest tests/
```

## CI/CD Integration

### GitHub Actions Example
```yaml
name: Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest

    services:
      postgres:
        image: postgres:15
        env:
          POSTGRES_USER: repograph
          POSTGRES_PASSWORD: repograph_dev_password
          POSTGRES_DB: repograph_test
        ports:
          - 5433:5432

    steps:
      - uses: actions/checkout@v3

      - name: Set up Python
        uses: actions/setup-python@v4
        with:
          python-version: '3.12'

      - name: Install dependencies
        run: |
          cd packages/repograph-cloud/apps/api
          pip install -r requirements.txt

      - name: Run tests
        run: |
          cd packages/repograph-cloud/apps/api
          export PYTHONPATH=$PWD
          pytest tests/api/ --cov=app --cov-report=xml

      - name: Upload coverage
        uses: codecov/codecov-action@v3
```

## Writing New Tests

### Basic Test Template
```python
import pytest
from httpx import AsyncClient
from fastapi import status

class TestMyFeature:
    """Test my new feature."""

    @pytest.mark.asyncio
    async def test_my_feature_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test successful case."""
        response = await authenticated_async_client.get("/api/my-endpoint")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()
        assert "expected_field" in data

    @pytest.mark.asyncio
    async def test_my_feature_unauthorized(
        self,
        async_client: AsyncClient,  # No auth
    ):
        """Test auth required."""
        response = await async_client.get("/api/my-endpoint")
        assert response.status_code == status.HTTP_401_UNAUTHORIZED
```

### Testing with Database
```python
@pytest.mark.asyncio
async def test_create_item(
    authenticated_async_client: AsyncClient,
    db_session: AsyncSession,
):
    """Test creating an item."""
    # Create via API
    response = await authenticated_async_client.post(
        "/api/items/",
        json={"name": "Test Item"}
    )

    assert response.status_code == status.HTTP_201_CREATED
    item_id = response.json()["id"]

    # Verify in database
    from app.models import Item
    from sqlalchemy import select

    result = await db_session.execute(
        select(Item).where(Item.id == item_id)
    )
    item = result.scalar_one()
    assert item.name == "Test Item"
```

## Performance Testing

```python
@pytest.mark.asyncio
async def test_endpoint_performance(
    authenticated_async_client: AsyncClient,
):
    """Test endpoint responds quickly."""
    import time

    start = time.time()
    response = await authenticated_async_client.get("/api/fast-endpoint")
    elapsed = time.time() - start

    assert response.status_code == 200
    assert elapsed < 0.5  # Should respond in under 500ms
```

## Mocking External Services

```python
from unittest.mock import patch, MagicMock

@pytest.mark.asyncio
async def test_with_mock_service(
    authenticated_async_client: AsyncClient,
):
    """Test with mocked external service."""
    with patch("app.services.external.ExternalService") as MockService:
        mock_instance = MockService.return_value
        mock_instance.call_api.return_value = {"status": "success"}

        response = await authenticated_async_client.post("/api/use-service")

        assert response.status_code == 200
        mock_instance.call_api.assert_called_once()
```

## Next Steps

1. **Expand Coverage**: Add tests for background jobs, integrations
2. **Frontend Testing**: Set up Jest for React components
3. **E2E Testing**: Implement Playwright tests
4. **CI/CD**: Integrate tests into GitHub Actions

## Resources

- [Full Report](WEEK_13_TESTING_FINAL_REPORT.md)
- [pytest Documentation](https://docs.pytest.org/)
- [pytest-asyncio Documentation](https://pytest-asyncio.readthedocs.io/)
- [FastAPI Testing](https://fastapi.tiangolo.com/tutorial/testing/)

---

**Questions?** Check the full report or test files for more examples.
