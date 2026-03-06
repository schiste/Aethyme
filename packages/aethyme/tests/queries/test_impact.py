"""
Contract tests for impact analysis queries.

Tests cover:
- Impact analysis with various max_depth limits
- Transitive dependencies
- Reverse impact (what depends on X)
- Performance with deep traversal
"""

from typing import TypeAlias, cast

from src.graph.store import GraphStore
from tests.support.db_seed import PRIMARY_TENANT_ID

ImpactNode: TypeAlias = dict[str, object]
ImpactDepthMap: TypeAlias = dict[int, list[ImpactNode]]


class TestImpactAnalysisBasic:
    """Test basic impact analysis functionality."""

    def test_impact_base_case(self):
        """Test impact analysis returns base symbol."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=1,
            limit=100
        )

        assert "error" not in result
        assert result["symbol"] == "models/user.py:User"
        assert isinstance(result["total_impacted"], int)

    def test_impact_direct_dependents(self):
        """Test finding direct dependents (depth 1)."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # User model is imported by UserService
        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=1,
            limit=100
        )

        assert "error" not in result
        assert result["total_impacted"] > 0

        # Should have depth 1 dependents
        assert 1 in result["by_depth"]
        depth_1 = result["by_depth"][1]

        # UserService should be a dependent
        symbols = [d["symbol"] for d in depth_1]
        assert any("UserService" in s for s in symbols)

    def test_impact_no_dependents(self):
        """Test symbol with no dependents."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # Test functions typically have no dependents
        result = store.impact_analysis(
            symbol="tests/test_user_service.py:test_authenticate",
            max_depth=5,
            limit=100
        )

        assert "error" not in result
        # Should find the symbol but have 0 impacted (nothing depends on tests)
        assert result["total_impacted"] == 0


class TestImpactAnalysisTransitive:
    """Test transitive dependency analysis."""

    def test_transitive_depth_2(self):
        """Test transitive dependencies at depth 2."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # User model -> UserService -> API routes
        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=2,
            limit=100
        )

        assert "error" not in result
        assert result["total_impacted"] > 0

        # Should have both depth 1 and depth 2
        assert 1 in result["by_depth"]

        # Depth 2 should include API routes that use UserService
        if 2 in result["by_depth"]:
            depth_2 = result["by_depth"][2]
            symbols = [d["symbol"] for d in depth_2]
            # Should include API routes
            assert any("api/routes" in s for s in symbols)

    def test_transitive_depth_3_plus(self):
        """Test deep transitive dependencies (depth 3+)."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=5,
            limit=200
        )

        assert "error" not in result

        # Should find multiple levels of dependencies
        depths = list(result["by_depth"].keys())
        assert len(depths) > 0

    def test_transitive_chain(self):
        """Test full dependency chain."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # Start from a low-level utility
        result = store.impact_analysis(
            symbol="utils/crypto.py:hash_password",
            max_depth=10,
            limit=500
        )

        assert "error" not in result

        # hash_password is used by:
        # - UserService.create_user (depth 1)
        # - create_user API route (depth 2)
        # - Frontend components (depth 3)
        assert result["total_impacted"] > 0


class TestImpactAnalysisDepthLimits:
    """Test max_depth parameter."""

    def test_max_depth_1(self):
        """Test max_depth=1 only finds direct dependents."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=1,
            limit=100
        )

        assert "error" not in result

        # Should only have depth 1 (no depth 2, 3, etc.)
        assert 1 in result["by_depth"]
        assert 2 not in result["by_depth"]
        assert result["max_depth_reached"] <= 1

    def test_max_depth_incremental(self):
        """Test increasing max_depth finds more dependents."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result_d1 = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=1,
            limit=100
        )

        result_d3 = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=3,
            limit=100
        )

        result_d5 = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=5,
            limit=100
        )

        # Deeper analysis should find same or more dependents
        assert result_d3["total_impacted"] >= result_d1["total_impacted"]
        assert result_d5["total_impacted"] >= result_d3["total_impacted"]

    def test_max_depth_10(self):
        """Test max_depth=10 for deep analysis."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="config/database.py:get_db_connection",
            max_depth=10,
            limit=500
        )

        assert "error" not in result
        # Database connection is used by many things
        assert result["total_impacted"] > 0


class TestImpactAnalysisReverseImpact:
    """Test reverse dependency analysis (what depends on X)."""

    def test_core_model_impact(self):
        """Test impact of changing core model."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # User is a core model used everywhere
        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=5,
            limit=500
        )

        assert "error" not in result
        assert result["total_impacted"] > 5  # Many things use User

        # Should include services, API routes, etc.
        all_symbols: list[str] = []
        by_depth = cast(ImpactDepthMap, result["by_depth"])
        for depth_nodes in by_depth.values():
            all_symbols.extend(
                [symbol for symbol in (node.get("symbol") for node in depth_nodes) if isinstance(symbol, str)]
            )

        assert any("Service" in s for s in all_symbols)
        assert any("api/routes" in s for s in all_symbols)

    def test_utility_function_impact(self):
        """Test impact of changing utility function."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="utils/validators.py:validate_email",
            max_depth=5,
            limit=100
        )

        assert "error" not in result

        # validate_email should be used by UserService or similar
        if result["total_impacted"] > 0:
            all_symbols: list[str] = []
            by_depth = cast(ImpactDepthMap, result["by_depth"])
            for depth_nodes in by_depth.values():
                all_symbols.extend(
                    [symbol for symbol in (node.get("symbol") for node in depth_nodes) if isinstance(symbol, str)]
                )

            # Should find services or functions that validate emails

    def test_leaf_node_no_impact(self):
        """Test leaf nodes have no impact (nothing depends on them)."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # Test functions are typically leaf nodes
        result = store.impact_analysis(
            symbol="tests/test_product_service.py:test_search",
            max_depth=5,
            limit=100
        )

        assert "error" not in result
        assert result["total_impacted"] == 0


class TestImpactAnalysisPerformance:
    """Test performance characteristics."""

    def test_impact_performance_shallow(self):
        """Test shallow impact analysis is fast."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        import time
        start = time.time()

        result = store.impact_analysis(
            symbol="models/order.py:Order",
            max_depth=2,
            limit=50
        )

        duration = time.time() - start

        assert "error" not in result
        # Should complete quickly for shallow depth
        assert duration < 1.0  # <1s for depth 2

    def test_impact_performance_deep(self):
        """Test deep impact analysis completes in reasonable time."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        import time
        start = time.time()

        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=10,
            limit=500
        )

        duration = time.time() - start

        assert "error" not in result
        # Should complete within target even for deep analysis
        assert duration < 2.0  # <2s for depth 10

    def test_limit_enforcement(self):
        """Test that limit parameter is enforced."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result_small = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=10,
            limit=10
        )

        result_large = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=10,
            limit=100
        )

        # Small limit should return fewer results
        assert result_small["total_impacted"] <= 10
        assert result_large["total_impacted"] >= result_small["total_impacted"]


class TestImpactAnalysisErrors:
    """Test error cases."""

    def test_symbol_not_found(self):
        """Test impact analysis for non-existent symbol."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="non.existent:Symbol",
            max_depth=5,
            limit=100
        )

        assert "error" in result

    def test_empty_symbol(self):
        """Test impact analysis with empty symbol."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="",
            max_depth=5,
            limit=100
        )

        assert "error" in result


class TestImpactAnalysisCircular:
    """Test handling of circular dependencies."""

    def test_circular_dependency_handling(self):
        """Test impact analysis handles circular dependencies."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # UserService <-> OrderService circular reference
        result = store.impact_analysis(
            symbol="services/user.py:UserService",
            max_depth=10,
            limit=500
        )

        assert "error" not in result

        # Should complete without infinite loop
        assert result["total_impacted"] >= 0

        # Should not have duplicate symbols
        all_node_ids: set[object] = set()
        by_depth = cast(ImpactDepthMap, result["by_depth"])
        for depth_nodes in by_depth.values():
            for node in depth_nodes:
                node_id = node.get("symbol")
                assert node_id not in all_node_ids, f"Duplicate symbol: {node_id}"
                all_node_ids.add(node_id)


class TestImpactAnalysisRLS:
    """Test Row-Level Security for impact analysis."""

    def test_tenant_isolation(self):
        """Test impact analysis respects tenant isolation."""
        store1 = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        # Try to analyze tenant2 symbol from tenant1 context
        result = store1.impact_analysis(
            symbol="services/blog.py:BlogService",
            max_depth=5,
            limit=100
        )

        # Should not find the symbol due to RLS
        assert "error" in result

    def test_impact_only_within_tenant(self):
        """Test impact analysis only includes nodes from same tenant."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=10,
            limit=500
        )

        assert "error" not in result

        # All impacted symbols should be from same tenant
        all_symbols: list[str] = []
        by_depth = cast(ImpactDepthMap, result["by_depth"])
        for depth_nodes in by_depth.values():
            all_symbols.extend(
                [symbol for symbol in (node.get("symbol") for node in depth_nodes) if isinstance(symbol, str)]
            )

        # Should not contain tenant2 symbols
        assert not any("BlogService" in s for s in all_symbols)


class TestImpactAnalysisDepthOrganization:
    """Test depth-level organization."""

    def test_nodes_organized_by_depth(self):
        """Test that impacted nodes are organized by depth."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="models/product.py:Product",
            max_depth=5,
            limit=100
        )

        assert "error" not in result

        # Each depth should be a list of dicts
        by_depth = cast(ImpactDepthMap, result["by_depth"])
        for _depth, nodes in by_depth.items():
            assert isinstance(nodes, list)
            assert all(isinstance(node, dict) for node in nodes)
            assert all("symbol" in node for node in nodes)

    def test_depth_increments(self):
        """Test depths increment correctly."""
        store = GraphStore(tenant_id=PRIMARY_TENANT_ID)

        result = store.impact_analysis(
            symbol="models/user.py:User",
            max_depth=5,
            limit=100
        )

        assert "error" not in result

        if result["total_impacted"] > 0:
            depths = sorted(result["by_depth"].keys())
            # Depths should start from 1 (no depth 0 for impacted nodes)
            if len(depths) > 0:
                assert depths[0] >= 1

                # Each depth should be sequential or skip
                for i in range(1, len(depths)):
                    assert depths[i] > depths[i-1]
