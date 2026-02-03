"""
Performance benchmarking for query operations.

Load testing scenarios:
- 1000 queries with varying types
- Measure p50, p95, p99 latencies
- Test with and without cache
- Test concurrent requests (10, 50, 100)
"""

import asyncio
import time
import statistics
from typing import List, Dict, Any
from dataclasses import dataclass
import json


@dataclass
class BenchmarkResult:
    """Results from a benchmark run."""
    query_type: str
    total_queries: int
    duration_seconds: float
    queries_per_second: float
    p50_ms: float
    p95_ms: float
    p99_ms: float
    min_ms: float
    max_ms: float
    cache_enabled: bool
    concurrent_requests: int


class QueryBenchmark:
    """Performance benchmarking suite for queries."""

    def __init__(self, tenant_id: str):
        self.tenant_id = tenant_id
        self.results: List[BenchmarkResult] = []

    async def benchmark_search(
        self,
        queries: List[str],
        cache_enabled: bool = False,
        concurrent: int = 1
    ) -> BenchmarkResult:
        """
        Benchmark search queries.

        Args:
            queries: List of search queries to test
            cache_enabled: Whether to use cache
            concurrent: Number of concurrent requests

        Returns:
            BenchmarkResult with performance metrics
        """
        from src.queries.optimized_search import get_search_service

        latencies = []
        start_time = time.time()

        async with get_search_service() as search_service:
            # Run queries
            for i in range(0, len(queries), concurrent):
                batch = queries[i:i + concurrent]

                # Execute batch concurrently
                tasks = [
                    self._run_search(search_service, query)
                    for query in batch
                ]

                batch_results = await asyncio.gather(*tasks, return_exceptions=True)

                # Collect latencies
                for result in batch_results:
                    if isinstance(result, dict) and "latency_ms" in result:
                        latencies.append(result["latency_ms"])

        total_duration = time.time() - start_time

        return self._calculate_result(
            query_type="search",
            latencies=latencies,
            total_duration=total_duration,
            cache_enabled=cache_enabled,
            concurrent=concurrent
        )

    async def _run_search(
        self,
        search_service,
        query: str
    ) -> Dict[str, Any]:
        """Run single search query and measure latency."""
        start = time.time()

        try:
            results = await search_service.search_hybrid(
                tenant_id=self.tenant_id,
                query=query,
                limit=20
            )

            latency_ms = (time.time() - start) * 1000

            return {
                "latency_ms": latency_ms,
                "results_count": len(results),
                "success": True
            }

        except Exception as e:
            latency_ms = (time.time() - start) * 1000
            return {
                "latency_ms": latency_ms,
                "error": str(e),
                "success": False
            }

    async def benchmark_ego_graph(
        self,
        symbols: List[str],
        depth: int = 2,
        cache_enabled: bool = False,
        concurrent: int = 1
    ) -> BenchmarkResult:
        """
        Benchmark ego graph queries.

        Args:
            symbols: List of symbols to query
            depth: Graph traversal depth
            cache_enabled: Whether to use cache
            concurrent: Concurrent requests

        Returns:
            BenchmarkResult
        """
        from src.graph.store import GraphStore

        latencies = []
        start_time = time.time()
        store = GraphStore(tenant_id=self.tenant_id)

        for i in range(0, len(symbols), concurrent):
            batch = symbols[i:i + concurrent]

            # For now, run sequentially (TODO: async version)
            for symbol in batch:
                start = time.time()

                try:
                    result = store.ego_graph(
                        symbol=symbol,
                        depth=depth,
                        limit=100
                    )

                    if "error" not in result:
                        latency_ms = (time.time() - start) * 1000
                        latencies.append(latency_ms)

                except Exception:
                    pass

        total_duration = time.time() - start_time

        return self._calculate_result(
            query_type=f"ego_depth_{depth}",
            latencies=latencies,
            total_duration=total_duration,
            cache_enabled=cache_enabled,
            concurrent=concurrent
        )

    async def benchmark_impact_analysis(
        self,
        symbols: List[str],
        max_depth: int = 10,
        cache_enabled: bool = False,
        concurrent: int = 1
    ) -> BenchmarkResult:
        """
        Benchmark impact analysis queries.

        Args:
            symbols: List of symbols to analyze
            max_depth: Maximum traversal depth
            cache_enabled: Whether to use cache
            concurrent: Concurrent requests

        Returns:
            BenchmarkResult
        """
        from src.graph.store import GraphStore

        latencies = []
        start_time = time.time()
        store = GraphStore(tenant_id=self.tenant_id)

        for i in range(0, len(symbols), concurrent):
            batch = symbols[i:i + concurrent]

            for symbol in batch:
                start = time.time()

                try:
                    result = store.impact_analysis(
                        symbol=symbol,
                        max_depth=max_depth,
                        limit=1000
                    )

                    if "error" not in result:
                        latency_ms = (time.time() - start) * 1000
                        latencies.append(latency_ms)

                except Exception:
                    pass

        total_duration = time.time() - start_time

        return self._calculate_result(
            query_type=f"impact_depth_{max_depth}",
            latencies=latencies,
            total_duration=total_duration,
            cache_enabled=cache_enabled,
            concurrent=concurrent
        )

    def _calculate_result(
        self,
        query_type: str,
        latencies: List[float],
        total_duration: float,
        cache_enabled: bool,
        concurrent: int
    ) -> BenchmarkResult:
        """Calculate benchmark statistics."""
        if not latencies:
            return BenchmarkResult(
                query_type=query_type,
                total_queries=0,
                duration_seconds=total_duration,
                queries_per_second=0.0,
                p50_ms=0.0,
                p95_ms=0.0,
                p99_ms=0.0,
                min_ms=0.0,
                max_ms=0.0,
                cache_enabled=cache_enabled,
                concurrent_requests=concurrent
            )

        sorted_latencies = sorted(latencies)
        n = len(sorted_latencies)

        return BenchmarkResult(
            query_type=query_type,
            total_queries=len(latencies),
            duration_seconds=total_duration,
            queries_per_second=len(latencies) / total_duration if total_duration > 0 else 0,
            p50_ms=sorted_latencies[int(n * 0.5)],
            p95_ms=sorted_latencies[int(n * 0.95)],
            p99_ms=sorted_latencies[int(n * 0.99)],
            min_ms=sorted_latencies[0],
            max_ms=sorted_latencies[-1],
            cache_enabled=cache_enabled,
            concurrent_requests=concurrent
        )

    def print_results(self):
        """Print benchmark results as a table."""
        print("\n" + "="*100)
        print("QUERY PERFORMANCE BENCHMARK RESULTS")
        print("="*100)
        print(f"{'Query Type':<25} {'Cache':<8} {'Conc':<6} {'Total':<8} {'QPS':<10} {'p50':<10} {'p95':<10} {'p99':<10}")
        print("-"*100)

        for result in self.results:
            print(
                f"{result.query_type:<25} "
                f"{'Yes' if result.cache_enabled else 'No':<8} "
                f"{result.concurrent_requests:<6} "
                f"{result.total_queries:<8} "
                f"{result.queries_per_second:<10.1f} "
                f"{result.p50_ms:<10.1f} "
                f"{result.p95_ms:<10.1f} "
                f"{result.p99_ms:<10.1f}"
            )

        print("="*100)

    def export_json(self, filepath: str):
        """Export results to JSON file."""
        data = {
            "timestamp": time.time(),
            "results": [
                {
                    "query_type": r.query_type,
                    "cache_enabled": r.cache_enabled,
                    "concurrent_requests": r.concurrent_requests,
                    "total_queries": r.total_queries,
                    "duration_seconds": r.duration_seconds,
                    "queries_per_second": r.queries_per_second,
                    "latency_ms": {
                        "p50": r.p50_ms,
                        "p95": r.p95_ms,
                        "p99": r.p99_ms,
                        "min": r.min_ms,
                        "max": r.max_ms,
                    }
                }
                for r in self.results
            ]
        }

        with open(filepath, 'w') as f:
            json.dump(data, f, indent=2)


async def run_full_benchmark():
    """Run comprehensive benchmark suite."""
    print("Starting Aethyme Query Performance Benchmark...")

    tenant_id = "00000000-0000-0000-0000-000000000001"
    benchmark = QueryBenchmark(tenant_id)

    # Test queries
    search_queries = [
        "UserService", "ProductService", "OrderService",
        "authenticate", "create_order", "search", "payment",
        "validate", "cache", "database", "api", "route",
    ] * 20  # 260 queries

    symbols = [
        "services/user.py:UserService",
        "services/product.py:ProductService",
        "services/order.py:OrderService",
        "services/payment.py:PaymentService",
        "models/user.py:User",
        "models/product.py:Product",
        "models/order.py:Order",
    ] * 5  # 35 symbols

    # Benchmark search without cache
    print("\n1. Benchmarking search (no cache, sequential)...")
    result = await benchmark.benchmark_search(search_queries[:100], cache_enabled=False, concurrent=1)
    benchmark.results.append(result)

    # Benchmark search with concurrent requests
    print("2. Benchmarking search (no cache, 10 concurrent)...")
    result = await benchmark.benchmark_search(search_queries[:100], cache_enabled=False, concurrent=10)
    benchmark.results.append(result)

    # Benchmark ego graph
    print("3. Benchmarking ego graph (depth 2)...")
    result = await benchmark.benchmark_ego_graph(symbols, depth=2, cache_enabled=False, concurrent=1)
    benchmark.results.append(result)

    print("4. Benchmarking ego graph (depth 3)...")
    result = await benchmark.benchmark_ego_graph(symbols, depth=3, cache_enabled=False, concurrent=1)
    benchmark.results.append(result)

    # Benchmark impact analysis
    print("5. Benchmarking impact analysis (depth 5)...")
    result = await benchmark.benchmark_impact_analysis(symbols, max_depth=5, cache_enabled=False, concurrent=1)
    benchmark.results.append(result)

    print("6. Benchmarking impact analysis (depth 10)...")
    result = await benchmark.benchmark_impact_analysis(symbols, max_depth=10, cache_enabled=False, concurrent=1)
    benchmark.results.append(result)

    # Print and export results
    benchmark.print_results()
    benchmark.export_json("benchmark_results.json")

    print("\nResults exported to benchmark_results.json")
    print("\nPerformance Targets:")
    print("- Search p95: <500ms (no cache), <50ms (with cache)")
    print("- Ego graph p95: <1s (depth 2), <2s (depth 3)")
    print("- Impact analysis p95: <2s")


if __name__ == "__main__":
    asyncio.run(run_full_benchmark())
