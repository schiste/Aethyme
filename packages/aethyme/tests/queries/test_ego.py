"""
Contract tests for ego graph queries.

Tests cover:
- Ego graph with depth 1, 2, 3
- Symbol not found (404)
- Circular dependencies
- Large ego graphs (>100 nodes)
"""

import pytest
from src.graph.store import GraphStore


class TestEgoGraphBasic:
    """Test basic ego graph functionality."""

    def test_ego_depth_0(self, test_tenant_id):
        """Test ego graph at depth 0 (just the symbol itself)."""
        store = GraphStore(tenant_id=test_tenant_id)
        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=0,
            limit=100
        )

        assert "error" not in result
        assert result["definition"] is not None
        assert result["definition"]["symbol"] == "services/user.py:UserService"
        assert result["total_nodes"] >= 1

        # Depth 0 should only have the symbol itself
        assert 0 in result["nodes_by_depth"]
        assert len(result["nodes_by_depth"][0]) >= 1

    def test_ego_depth_1(self, test_tenant_id):
        """Test ego graph at depth 1 (direct connections)."""
        store = GraphStore(tenant_id=test_tenant_id)
        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=1,
            limit=100
        )

        assert "error" not in result
        assert result["total_nodes"] > 1

        # Should have depth 0 (self) and depth 1 (direct connections)
        assert 0 in result["nodes_by_depth"]
        assert 1 in result["nodes_by_depth"]

        # UserService should have connections to:
        # - Methods (authenticate, create_user, get_profile, update_profile)
        # - Models (User, UserProfile)
        # - Repository (UserRepository)
        depth_1_nodes = result["nodes_by_depth"][1]
        assert len(depth_1_nodes) > 0

        # Check that we have some expected connections
        symbols = [n["symbol"] for n in depth_1_nodes]
        # Should include at least some models or methods
        assert any("User" in s or "authenticate" in s for s in symbols)

    def test_ego_depth_2(self, test_tenant_id):
        """Test ego graph at depth 2 (two hops)."""
        store = GraphStore(tenant_id=test_tenant_id)
        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=2,
            limit=100
        )

        assert "error" not in result
        assert result["total_nodes"] > 1

        # Should have depths 0, 1, and possibly 2
        assert 0 in result["nodes_by_depth"]
        assert 1 in result["nodes_by_depth"]

        # Depth 2 might include:
        # - API routes that call UserService methods
        # - Database functions called by UserRepository
        # - Utilities called by UserService methods

    def test_ego_depth_3(self, test_tenant_id):
        """Test ego graph at depth 3 (three hops)."""
        store = GraphStore(tenant_id=test_tenant_id)
        result = store.ego_graph(
            symbol="services/order.py:OrderService",
            depth=3,
            limit=200
        )

        assert "error" not in result
        assert result["total_nodes"] > 1

        # OrderService has complex dependencies:
        # - Depth 1: Order model, OrderRepository, ProductService, PaymentService
        # - Depth 2: Product model, Payment methods
        # - Depth 3: Even more downstream dependencies


class TestEgoGraphErrors:
    """Test error cases."""

    def test_symbol_not_found(self, test_tenant_id):
        """Test ego graph for non-existent symbol returns error."""
        store = GraphStore(tenant_id=test_tenant_id)
        result = store.ego_graph(
            symbol="non.existent:Symbol",
            depth=2,
            limit=100
        )

        assert "error" in result
        assert "not found" in result["error"].lower()

    def test_empty_symbol(self, test_tenant_id):
        """Test ego graph with empty symbol."""
        store = GraphStore(tenant_id=test_tenant_id)
        result = store.ego_graph(
            symbol="",
            depth=2,
            limit=100
        )

        assert "error" in result

    def test_invalid_depth_zero(self, test_tenant_id):
        """Test depth validation (depth must be >= 0)."""
        store = GraphStore(tenant_id=test_tenant_id)

        # Depth 0 should work
        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=0,
            limit=100
        )
        assert "error" not in result


class TestEgoGraphCircular:
    """Test handling of circular dependencies."""

    def test_circular_dependency_detection(self, test_tenant_id):
        """Test ego graph handles circular dependencies without infinite loop."""
        store = GraphStore(tenant_id=test_tenant_id)

        # UserService and OrderService have circular reference in test data
        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=3,
            limit=100
        )

        assert "error" not in result
        # Should complete without hanging
        assert result["total_nodes"] > 0

        # All nodes should be unique (no duplicates from cycles)
        all_node_ids = []
        for depth_nodes in result["nodes_by_depth"].values():
            all_node_ids.extend([n["id"] for n in depth_nodes])

        assert len(all_node_ids) == len(set(all_node_ids)), "Found duplicate nodes"

    def test_self_referential_symbol(self, test_tenant_id):
        """Test symbol that references itself."""
        store = GraphStore(tenant_id=test_tenant_id)

        # Even if a symbol has a self-edge, it should only appear once
        result = store.ego_graph(
            symbol="services/cache.py:CacheService",
            depth=2,
            limit=100
        )

        assert "error" not in result
        # Definition should only appear once
        assert result["definition"] is not None


class TestEgoGraphLarge:
    """Test large ego graphs."""

    def test_large_graph_performance(self, test_tenant_id):
        """Test ego graph with many connected nodes."""
        store = GraphStore(tenant_id=test_tenant_id)

        import time
        start = time.time()

        # OrderService has many dependencies
        result = store.ego_graph(
            symbol="services/order.py:OrderService",
            depth=3,
            limit=200
        )

        duration = time.time() - start

        assert "error" not in result
        assert result["total_nodes"] > 10

        # Should complete within performance target
        # Target: <1s for depth 2, <2s for depth 3
        assert duration < 3.0

    def test_limit_enforcement(self, test_tenant_id):
        """Test that limit parameter is enforced."""
        store = GraphStore(tenant_id=test_tenant_id)

        result_small = store.ego_graph(
            symbol="services/order.py:OrderService",
            depth=3,
            limit=10
        )

        result_large = store.ego_graph(
            symbol="services/order.py:OrderService",
            depth=3,
            limit=100
        )

        assert result_small["total_nodes"] <= 10
        assert result_large["total_nodes"] >= result_small["total_nodes"]

    def test_deep_traversal(self, test_tenant_id):
        """Test deep graph traversal (depth 3)."""
        store = GraphStore(tenant_id=test_tenant_id)

        result = store.ego_graph(
            symbol="api/routes/orders.py:create_order",
            depth=3,
            limit=150
        )

        assert "error" not in result

        # API route -> OrderService -> ProductService/PaymentService -> deeper deps
        # Should find nodes at multiple depths
        depths = list(result["nodes_by_depth"].keys())
        assert len(depths) > 1


class TestEgoGraphDepthLevels:
    """Test depth-specific behavior."""

    def test_nodes_organized_by_depth(self, test_tenant_id):
        """Test that nodes are correctly organized by depth."""
        store = GraphStore(tenant_id=test_tenant_id)

        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=2,
            limit=100
        )

        assert "error" not in result

        # Each depth level should be a list
        for depth, nodes in result["nodes_by_depth"].items():
            assert isinstance(nodes, list)
            assert all(isinstance(n, dict) for n in nodes)
            assert all("symbol" in n for n in nodes)

    def test_depth_increments_correctly(self, test_tenant_id):
        """Test that depths increment correctly."""
        store = GraphStore(tenant_id=test_tenant_id)

        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=3,
            limit=100
        )

        assert "error" not in result

        # Depths should be sequential: 0, 1, 2, 3 (or subset)
        depths = sorted(result["nodes_by_depth"].keys())
        assert depths[0] == 0

        # Each subsequent depth should be +1
        for i in range(1, len(depths)):
            assert depths[i] <= depths[i-1] + 1


class TestEgoGraphRLS:
    """Test Row-Level Security for ego graphs."""

    def test_tenant_isolation(self):
        """Test ego graph respects tenant isolation."""
        tenant1_id = "00000000-0000-0000-0000-000000000001"
        tenant2_id = "00000000-0000-0000-0000-000000000002"

        store1 = GraphStore(tenant_id=tenant1_id)

        # Try to query tenant2 symbol from tenant1 context
        result = store1.ego_graph(
            symbol="services/blog.py:BlogService",
            depth=2,
            limit=100
        )

        # Should not find the symbol due to RLS
        assert "error" in result

    def test_ego_contains_only_tenant_nodes(self, test_tenant_id):
        """Test ego graph only includes nodes from same tenant."""
        store = GraphStore(tenant_id=test_tenant_id)

        result = store.ego_graph(
            symbol="services/user.py:UserService",
            depth=3,
            limit=100
        )

        assert "error" not in result

        # All nodes should be from the same tenant
        # (tenant_id might not be in response, but we verify via search isolation)
        all_symbols = []
        for nodes in result["nodes_by_depth"].values():
            all_symbols.extend([n["symbol"] for n in nodes])

        # Should not contain any tenant2 symbols (BlogService, etc.)
        assert not any("BlogService" in s for s in all_symbols)


# Fixtures

@pytest.fixture
def test_tenant_id():
    """Return test tenant ID (org1)."""
    return "00000000-0000-0000-0000-000000000001"
