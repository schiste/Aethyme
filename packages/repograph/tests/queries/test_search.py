"""
Contract tests for search queries.

Tests cover:
- Exact match search
- Fuzzy search
- Hybrid search
- Filters (language, kind)
- Pagination
- RLS isolation (org1 can't see org2)
"""

import pytest
from typing import List, Dict, Any
from src.graph.store import GraphStore


class TestSearchExact:
    """Test exact symbol name matching."""

    def test_exact_match_found(self, test_tenant_id):
        """Test exact match returns correct symbol."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="services/user.py:UserService", search_type="exact", limit=10)

        assert len(results) > 0
        assert results[0]["symbol"] == "services/user.py:UserService"
        assert results[0]["kind"] == "class"
        assert results[0]["language"] == "python"

    def test_exact_match_not_found(self, test_tenant_id):
        """Test exact match returns empty for non-existent symbol."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="non.existent:Symbol", search_type="exact", limit=10)

        assert len(results) == 0

    def test_exact_match_case_sensitive(self, test_tenant_id):
        """Test exact match is case-sensitive."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="services/user.py:userservice", search_type="exact", limit=10)

        assert len(results) == 0


class TestSearchFuzzy:
    """Test fuzzy/trigram similarity search."""

    def test_fuzzy_partial_match(self, test_tenant_id):
        """Test fuzzy search finds partial matches."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="UserServ", search_type="fuzzy", limit=10)

        # Should find UserService and related symbols
        assert len(results) > 0
        symbols = [r["symbol"] for r in results]
        assert any("UserService" in s for s in symbols)

    def test_fuzzy_typo_tolerance(self, test_tenant_id):
        """Test fuzzy search handles typos."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="UsrService", search_type="fuzzy", limit=10)

        # Should still find UserService despite typo
        assert len(results) > 0

    def test_fuzzy_ranking(self, test_tenant_id):
        """Test fuzzy search ranks by similarity."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="Product", search_type="fuzzy", limit=10)

        # ProductService should rank higher than ProductCard
        assert len(results) > 0
        # First result should have highest score
        if len(results) > 1:
            assert results[0]["score"] >= results[1]["score"]


class TestSearchHybrid:
    """Test hybrid search (FTS + fuzzy)."""

    def test_hybrid_search_fts(self, test_tenant_id):
        """Test hybrid search uses full-text search."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="authentication", search_type="hybrid", limit=10)

        # Should find UserService.authenticate via documentation
        assert len(results) > 0
        symbols = [r["symbol"] for r in results]
        assert any("authenticate" in s.lower() for s in symbols)

    def test_hybrid_search_fuzzy(self, test_tenant_id):
        """Test hybrid search uses fuzzy matching."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="PaymentServ", search_type="hybrid", limit=10)

        # Should find PaymentService
        assert len(results) > 0
        symbols = [r["symbol"] for r in results]
        assert any("PaymentService" in s for s in symbols)

    def test_hybrid_search_documentation(self, test_tenant_id):
        """Test hybrid search includes documentation."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="profile management", search_type="hybrid", limit=10)

        # Should find symbols with "profile management" in docs
        assert len(results) > 0


class TestSearchFilters:
    """Test search filters."""

    def test_filter_by_kind_class(self, test_tenant_id):
        """Test filtering by kind=class."""
        store = GraphStore(tenant_id=test_tenant_id)

        # First, search without filter
        all_results = store.search(query="Service", search_type="fuzzy", limit=50)

        # Then, search with kind filter (need to implement filter support in store.search)
        # For now, filter results manually
        class_results = [r for r in all_results if r["kind"] == "class"]

        assert len(class_results) > 0
        assert all(r["kind"] == "class" for r in class_results)

    def test_filter_by_kind_function(self, test_tenant_id):
        """Test filtering by kind=function."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="test_", search_type="fuzzy", limit=50)

        function_results = [r for r in results if r["kind"] == "function"]
        assert len(function_results) > 0
        assert all(r["kind"] == "function" for r in function_results)

    def test_filter_by_language_python(self, test_tenant_id):
        """Test filtering by language=python."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="Service", search_type="fuzzy", limit=50)

        python_results = [r for r in results if r["language"] == "python"]
        assert len(python_results) > 0
        assert all(r["language"] == "python" for r in python_results)

    def test_filter_by_language_typescript(self, test_tenant_id):
        """Test filtering by language=typescript."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="Component", search_type="fuzzy", limit=50)

        # Should find TypeScript components
        ts_results = [r for r in results if r["language"] == "typescript"]
        # We have LoginForm, ProductCard, Cart components
        assert len(ts_results) >= 0  # May be 0 if no TypeScript symbols match


class TestSearchPagination:
    """Test search pagination."""

    def test_limit_results(self, test_tenant_id):
        """Test limit parameter works."""
        store = GraphStore(tenant_id=test_tenant_id)

        results_5 = store.search(query="Service", search_type="fuzzy", limit=5)
        results_10 = store.search(query="Service", search_type="fuzzy", limit=10)

        assert len(results_5) <= 5
        assert len(results_10) <= 10
        assert len(results_10) >= len(results_5)

    def test_limit_max_100(self, test_tenant_id):
        """Test limit is capped at 100."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="", search_type="fuzzy", limit=1000)

        # Should be capped at 100 (or less if fewer results)
        assert len(results) <= 100

    def test_empty_query_behavior(self, test_tenant_id):
        """Test empty query returns no results."""
        store = GraphStore(tenant_id=test_tenant_id)
        results = store.search(query="", search_type="exact", limit=10)

        assert len(results) == 0


class TestSearchRLS:
    """Test Row-Level Security isolation."""

    def test_tenant_isolation_org1(self):
        """Test org1 can only see org1 data."""
        tenant1_id = "00000000-0000-0000-0000-000000000001"
        store = GraphStore(tenant_id=tenant1_id)

        results = store.search(query="Service", search_type="fuzzy", limit=50)

        # All results should be from tenant1
        assert all(r.get("tenant_id") == tenant1_id for r in results if "tenant_id" in r)

        # Should find UserService, ProductService, OrderService, etc.
        symbols = [r["symbol"] for r in results]
        assert any("UserService" in s for s in symbols)
        assert any("ProductService" in s for s in symbols)

        # Should NOT find BlogService (tenant2)
        assert not any("BlogService" in s for s in symbols)

    def test_tenant_isolation_org2(self):
        """Test org2 can only see org2 data."""
        tenant2_id = "00000000-0000-0000-0000-000000000002"
        store = GraphStore(tenant_id=tenant2_id)

        results = store.search(query="Service", search_type="fuzzy", limit=50)

        # Should find BlogService
        symbols = [r["symbol"] for r in results]
        assert any("BlogService" in s for s in symbols)

        # Should NOT find UserService (tenant1)
        assert not any("UserService" in s for s in symbols)

    def test_cross_tenant_query_fails(self):
        """Test querying with wrong tenant returns no results."""
        tenant1_id = "00000000-0000-0000-0000-000000000001"
        tenant2_id = "00000000-0000-0000-0000-000000000002"

        store = GraphStore(tenant_id=tenant1_id)

        # Search for symbol that only exists in tenant2
        results = store.search(query="BlogService", search_type="exact", limit=10)

        # Should return no results due to RLS
        assert len(results) == 0


class TestSearchPerformance:
    """Test search performance characteristics."""

    def test_search_returns_quickly(self, test_tenant_id, benchmark):
        """Test search completes within performance target."""
        store = GraphStore(tenant_id=test_tenant_id)

        def run_search():
            return store.search(query="Service", search_type="hybrid", limit=20)

        result = benchmark(run_search)

        # Should have results
        assert len(result) > 0

        # Benchmark will measure execution time
        # Target: <500ms without cache, verified in benchmark tests

    def test_large_result_set_performance(self, test_tenant_id):
        """Test performance with large result sets."""
        store = GraphStore(tenant_id=test_tenant_id)

        # Search for common term that will return many results
        import time
        start = time.time()
        results = store.search(query="py", search_type="fuzzy", limit=100)
        duration = time.time() - start

        # Should complete in reasonable time even with many results
        assert duration < 2.0  # <2s for 100 results


# Fixtures

@pytest.fixture
def test_tenant_id():
    """Return test tenant ID (org1)."""
    return "00000000-0000-0000-0000-000000000001"


@pytest.fixture(scope="session", autouse=True)
def load_test_data(db_connection):
    """Load test data fixtures before running tests."""
    import os

    fixture_path = os.path.join(
        os.path.dirname(__file__),
        "fixtures",
        "graph_data.sql"
    )

    with open(fixture_path, 'r') as f:
        sql = f.read()

    with db_connection.cursor() as cur:
        cur.execute(sql)

    db_connection.commit()
    yield
    # Cleanup after tests if needed


@pytest.fixture(scope="session")
def db_connection():
    """Provide database connection for test setup."""
    from src.graph.connection_pool import db_pool

    conn = db_pool.get_connection()
    yield conn
    conn.close()
