"""
Load Testing Benchmarks for RepoGraph.

Tests system under concurrent load:
- 100+ concurrent queries
- Concurrent indexing operations
- Mixed workload scenarios
- Resource utilization tracking
- Throughput and latency under load
"""

import time
import json
import threading
import statistics
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import List, Dict, Any, Callable
from dataclasses import dataclass, asdict
from pathlib import Path
import structlog

logger = structlog.get_logger(__name__)


@dataclass
class LoadTestResult:
    """Result of a single operation under load."""
    operation_type: str
    duration: float
    success: bool
    thread_id: int
    timestamp: float
    error: str = None


@dataclass
class LoadTestMetrics:
    """Aggregated load test metrics."""
    total_operations: int
    successful_operations: int
    failed_operations: int
    duration: float
    throughput: float  # operations per second
    avg_latency: float
    median_latency: float
    p95_latency: float
    p99_latency: float
    max_latency: float
    error_rate: float


class LoadBenchmark:
    """
    Benchmark system under concurrent load.

    Test scenarios:
    - Concurrent queries (100+ simultaneous)
    - Concurrent indexing (multiple repos)
    - Mixed workload (queries + indexing)
    - Sustained load over time
    """

    def __init__(self, output_dir: str = "./benchmarks/results"):
        self.logger = logger.bind(component="LoadBenchmark")
        self.output_dir = Path(output_dir)
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.results: List[LoadTestResult] = []

    def execute_query(self, query_id: int) -> LoadTestResult:
        """
        Execute a single query.

        Args:
            query_id: Query identifier

        Returns:
            LoadTestResult
        """
        start_time = time.time()
        thread_id = threading.get_ident()

        try:
            # TODO: Integrate with actual query engine
            # Simulate query execution
            time.sleep(0.01 + (query_id % 10) * 0.001)  # Variable latency

            duration = time.time() - start_time
            return LoadTestResult(
                operation_type="query",
                duration=duration,
                success=True,
                thread_id=thread_id,
                timestamp=start_time,
            )

        except Exception as e:
            duration = time.time() - start_time
            return LoadTestResult(
                operation_type="query",
                duration=duration,
                success=False,
                thread_id=thread_id,
                timestamp=start_time,
                error=str(e),
            )

    def execute_index(self, repo_id: int) -> LoadTestResult:
        """
        Execute an indexing operation.

        Args:
            repo_id: Repository identifier

        Returns:
            LoadTestResult
        """
        start_time = time.time()
        thread_id = threading.get_ident()

        try:
            # TODO: Integrate with actual indexer
            # Simulate indexing
            time.sleep(0.1 + (repo_id % 5) * 0.02)  # Variable latency

            duration = time.time() - start_time
            return LoadTestResult(
                operation_type="index",
                duration=duration,
                success=True,
                thread_id=thread_id,
                timestamp=start_time,
            )

        except Exception as e:
            duration = time.time() - start_time
            return LoadTestResult(
                operation_type="index",
                duration=duration,
                success=False,
                thread_id=thread_id,
                timestamp=start_time,
                error=str(e),
            )

    def execute_scorecard(self, repo_id: int) -> LoadTestResult:
        """
        Execute a scorecard operation.

        Args:
            repo_id: Repository identifier

        Returns:
            LoadTestResult
        """
        start_time = time.time()
        thread_id = threading.get_ident()

        try:
            # TODO: Integrate with actual scorecard
            # Simulate scorecard
            time.sleep(0.05 + (repo_id % 3) * 0.01)

            duration = time.time() - start_time
            return LoadTestResult(
                operation_type="scorecard",
                duration=duration,
                success=True,
                thread_id=thread_id,
                timestamp=start_time,
            )

        except Exception as e:
            duration = time.time() - start_time
            return LoadTestResult(
                operation_type="scorecard",
                duration=duration,
                success=False,
                thread_id=thread_id,
                timestamp=start_time,
                error=str(e),
            )

    def run_concurrent_load(
        self,
        operation_func: Callable,
        num_operations: int,
        num_workers: int,
    ) -> LoadTestMetrics:
        """
        Run concurrent load test.

        Args:
            operation_func: Function to execute
            num_operations: Total number of operations
            num_workers: Number of concurrent workers

        Returns:
            LoadTestMetrics
        """
        self.logger.info(
            "Starting load test",
            num_operations=num_operations,
            num_workers=num_workers,
        )

        results = []
        start_time = time.time()

        with ThreadPoolExecutor(max_workers=num_workers) as executor:
            futures = [
                executor.submit(operation_func, i)
                for i in range(num_operations)
            ]

            for future in as_completed(futures):
                result = future.result()
                results.append(result)
                self.results.append(result)

        end_time = time.time()
        total_duration = end_time - start_time

        # Calculate metrics
        successful = [r for r in results if r.success]
        failed = [r for r in results if not r.success]
        latencies = [r.duration for r in successful]

        if not latencies:
            self.logger.error("No successful operations")
            return LoadTestMetrics(
                total_operations=len(results),
                successful_operations=0,
                failed_operations=len(failed),
                duration=total_duration,
                throughput=0.0,
                avg_latency=0.0,
                median_latency=0.0,
                p95_latency=0.0,
                p99_latency=0.0,
                max_latency=0.0,
                error_rate=100.0,
            )

        sorted_latencies = sorted(latencies)
        p95_idx = int(len(sorted_latencies) * 0.95)
        p99_idx = int(len(sorted_latencies) * 0.99)

        metrics = LoadTestMetrics(
            total_operations=len(results),
            successful_operations=len(successful),
            failed_operations=len(failed),
            duration=total_duration,
            throughput=len(successful) / total_duration,
            avg_latency=statistics.mean(latencies),
            median_latency=statistics.median(latencies),
            p95_latency=sorted_latencies[p95_idx] if p95_idx < len(sorted_latencies) else sorted_latencies[-1],
            p99_latency=sorted_latencies[p99_idx] if p99_idx < len(sorted_latencies) else sorted_latencies[-1],
            max_latency=max(latencies),
            error_rate=len(failed) / len(results) * 100,
        )

        self.logger.info("Load test complete", **asdict(metrics))

        return metrics

    def test_concurrent_queries(
        self,
        num_queries: int = 100,
        num_workers: int = 20,
    ) -> LoadTestMetrics:
        """
        Test concurrent query load.

        Args:
            num_queries: Number of queries
            num_workers: Number of concurrent workers

        Returns:
            LoadTestMetrics
        """
        self.logger.info(
            "Testing concurrent queries",
            num_queries=num_queries,
            num_workers=num_workers,
        )

        return self.run_concurrent_load(
            self.execute_query,
            num_queries,
            num_workers,
        )

    def test_concurrent_indexing(
        self,
        num_repos: int = 10,
        num_workers: int = 5,
    ) -> LoadTestMetrics:
        """
        Test concurrent indexing load.

        Args:
            num_repos: Number of repositories
            num_workers: Number of concurrent workers

        Returns:
            LoadTestMetrics
        """
        self.logger.info(
            "Testing concurrent indexing",
            num_repos=num_repos,
            num_workers=num_workers,
        )

        return self.run_concurrent_load(
            self.execute_index,
            num_repos,
            num_workers,
        )

    def test_mixed_workload(
        self,
        num_operations: int = 100,
        num_workers: int = 20,
        query_ratio: float = 0.7,
    ) -> LoadTestMetrics:
        """
        Test mixed workload (queries + indexing + scorecards).

        Args:
            num_operations: Total operations
            num_workers: Number of concurrent workers
            query_ratio: Ratio of queries vs other operations

        Returns:
            LoadTestMetrics
        """
        self.logger.info(
            "Testing mixed workload",
            num_operations=num_operations,
            num_workers=num_workers,
        )

        def mixed_operation(op_id: int) -> LoadTestResult:
            # Distribute operations based on ratio
            if op_id / num_operations < query_ratio:
                return self.execute_query(op_id)
            elif op_id % 2 == 0:
                return self.execute_index(op_id)
            else:
                return self.execute_scorecard(op_id)

        return self.run_concurrent_load(
            mixed_operation,
            num_operations,
            num_workers,
        )

    def test_sustained_load(
        self,
        duration_seconds: int = 60,
        operations_per_second: int = 10,
    ) -> LoadTestMetrics:
        """
        Test sustained load over time.

        Args:
            duration_seconds: Test duration
            operations_per_second: Target operations per second

        Returns:
            LoadTestMetrics
        """
        self.logger.info(
            "Testing sustained load",
            duration_seconds=duration_seconds,
            operations_per_second=operations_per_second,
        )

        results = []
        start_time = time.time()
        end_time = start_time + duration_seconds
        operation_count = 0

        while time.time() < end_time:
            batch_start = time.time()

            # Execute operations for this second
            with ThreadPoolExecutor(max_workers=operations_per_second) as executor:
                futures = [
                    executor.submit(self.execute_query, operation_count + i)
                    for i in range(operations_per_second)
                ]

                for future in as_completed(futures):
                    result = future.result()
                    results.append(result)
                    self.results.append(result)

            operation_count += operations_per_second

            # Wait for next second
            elapsed = time.time() - batch_start
            if elapsed < 1.0:
                time.sleep(1.0 - elapsed)

        total_duration = time.time() - start_time

        # Calculate metrics (same as run_concurrent_load)
        successful = [r for r in results if r.success]
        failed = [r for r in results if not r.success]
        latencies = [r.duration for r in successful]

        sorted_latencies = sorted(latencies)
        p95_idx = int(len(sorted_latencies) * 0.95)
        p99_idx = int(len(sorted_latencies) * 0.99)

        metrics = LoadTestMetrics(
            total_operations=len(results),
            successful_operations=len(successful),
            failed_operations=len(failed),
            duration=total_duration,
            throughput=len(successful) / total_duration,
            avg_latency=statistics.mean(latencies),
            median_latency=statistics.median(latencies),
            p95_latency=sorted_latencies[p95_idx],
            p99_latency=sorted_latencies[p99_idx],
            max_latency=max(latencies),
            error_rate=len(failed) / len(results) * 100,
        )

        self.logger.info("Sustained load test complete", **asdict(metrics))

        return metrics

    def export_results(self, output_file: str):
        """
        Export all results to JSON.

        Args:
            output_file: Output file path
        """
        data = {
            "results": [asdict(r) for r in self.results],
        }

        output_path = self.output_dir / output_file
        with open(output_path, 'w') as f:
            json.dump(data, f, indent=2)

        self.logger.info("Results exported", output_path=str(output_path))


def main():
    """Run load benchmarks."""
    benchmark = LoadBenchmark()

    # Test 1: Concurrent Queries
    print("\n=== Concurrent Queries (100 queries, 20 workers) ===")
    metrics1 = benchmark.test_concurrent_queries(num_queries=100, num_workers=20)
    print(f"Throughput: {metrics1.throughput:.1f} ops/sec")
    print(f"Avg Latency: {metrics1.avg_latency*1000:.1f}ms")
    print(f"p95 Latency: {metrics1.p95_latency*1000:.1f}ms")
    print(f"Error Rate: {metrics1.error_rate:.2f}%")

    # Test 2: Concurrent Indexing
    print("\n=== Concurrent Indexing (10 repos, 5 workers) ===")
    metrics2 = benchmark.test_concurrent_indexing(num_repos=10, num_workers=5)
    print(f"Throughput: {metrics2.throughput:.1f} ops/sec")
    print(f"Avg Latency: {metrics2.avg_latency:.3f}s")
    print(f"p95 Latency: {metrics2.p95_latency:.3f}s")

    # Test 3: Mixed Workload
    print("\n=== Mixed Workload (100 ops, 70% queries) ===")
    metrics3 = benchmark.test_mixed_workload(
        num_operations=100,
        num_workers=20,
        query_ratio=0.7,
    )
    print(f"Throughput: {metrics3.throughput:.1f} ops/sec")
    print(f"Avg Latency: {metrics3.avg_latency*1000:.1f}ms")
    print(f"p95 Latency: {metrics3.p95_latency*1000:.1f}ms")

    # Export results
    benchmark.export_results("load_benchmark_results.json")


if __name__ == "__main__":
    main()
