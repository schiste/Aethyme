#!/usr/bin/env python3
"""
Indexing Performance Benchmark

Benchmarks indexing performance on repositories of different sizes.
Reports median, p95, p99 times and memory usage.
"""

import sys
import time
import json
import psutil
import statistics
from pathlib import Path
from typing import Dict, List, Optional
from dataclasses import dataclass, asdict
import structlog

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))

from src.indexing.validator import IndexerValidator, ValidationMetrics
from src.indexing.logging import setup_indexing_logging, create_indexing_logger
from src.indexing.metrics import metrics_collector

logger = structlog.get_logger(__name__)


@dataclass
class BenchmarkResult:
    """Results from a benchmark run."""
    repo_name: str
    repo_size: str
    language: str
    indexer_type: str
    duration_seconds: float
    symbol_count: int
    node_count: int
    edge_count: int
    file_count: int
    memory_usage_mb: float
    success: bool


class IndexingBenchmark:
    """
    Performance benchmark for indexing operations.

    Tests indexing on repos of different sizes and tracks performance metrics.
    """

    def __init__(self, test_repos_path: Optional[Path] = None):
        """
        Initialize benchmark.

        Args:
            test_repos_path: Path to test_repos.json fixture
        """
        if test_repos_path is None:
            test_repos_path = Path(__file__).parent.parent / "tests" / "fixtures" / "test_repos.json"

        self.test_repos_path = test_repos_path
        self.results: List[BenchmarkResult] = []
        self.logger = logger.bind(component="IndexingBenchmark")

        # Setup logging
        setup_indexing_logging(log_level="INFO", json_format=False)

    def load_test_repos(self) -> Dict:
        """Load test repository configuration."""
        with open(self.test_repos_path) as f:
            return json.load(f)

    def get_process_memory_mb(self) -> float:
        """Get current process memory usage in MB."""
        process = psutil.Process()
        return process.memory_info().rss / 1024 / 1024

    def benchmark_repository(
        self,
        repo_config: Dict,
        repo_path: Path,
    ) -> BenchmarkResult:
        """
        Benchmark indexing for a single repository.

        Args:
            repo_config: Repository configuration from test_repos.json
            repo_path: Local path to cloned repository

        Returns:
            BenchmarkResult
        """
        repo_name = repo_config["name"]
        language = repo_config["language"]
        repo_size = repo_config["size"]

        self.logger.info(
            "Starting benchmark",
            repo_name=repo_name,
            language=language,
            size=repo_size,
        )

        # Create logger for this operation
        index_logger = create_indexing_logger(
            repository_name=repo_name,
        )

        # Measure initial memory
        initial_memory = self.get_process_memory_mb()

        # Validate repository (which performs indexing)
        validator = IndexerValidator()

        start_time = time.time()

        try:
            metrics = validator.validate_repository(
                repo_path=repo_path,
                repo_name=repo_name,
                language=language,
                try_scip=True,
                force_fallback=False,
            )

            duration = time.time() - start_time
            final_memory = self.get_process_memory_mb()
            memory_usage = final_memory - initial_memory

            result = BenchmarkResult(
                repo_name=repo_name,
                repo_size=repo_size,
                language=language,
                indexer_type=metrics.indexer_type.value,
                duration_seconds=duration,
                symbol_count=metrics.symbol_count,
                node_count=metrics.node_count,
                edge_count=metrics.edge_count,
                file_count=metrics.file_count,
                memory_usage_mb=memory_usage,
                success=True,
            )

            self.logger.info(
                "Benchmark completed",
                repo_name=repo_name,
                duration=duration,
                memory_mb=memory_usage,
            )

            # Emit metrics
            metrics_collector.emit_full_metrics(
                repository=repo_name,
                language=language,
                indexer_type=metrics.indexer_type.value,
                duration_seconds=duration,
                symbol_count=metrics.symbol_count,
                node_count=metrics.node_count,
                edge_count=metrics.edge_count,
                file_count=metrics.file_count,
                status="success",
            )

        except Exception as e:
            duration = time.time() - start_time
            self.logger.error(
                "Benchmark failed",
                repo_name=repo_name,
                error=str(e),
            )

            result = BenchmarkResult(
                repo_name=repo_name,
                repo_size=repo_size,
                language=language,
                indexer_type="unknown",
                duration_seconds=duration,
                symbol_count=0,
                node_count=0,
                edge_count=0,
                file_count=0,
                memory_usage_mb=0,
                success=False,
            )

        self.results.append(result)
        return result

    def run_benchmarks(
        self,
        repo_paths: Dict[str, Path],
        iterations: int = 1,
    ) -> List[BenchmarkResult]:
        """
        Run benchmarks on multiple repositories.

        Args:
            repo_paths: Dict mapping repo names to local paths
            iterations: Number of times to run each benchmark

        Returns:
            List of BenchmarkResults
        """
        config = self.load_test_repos()
        test_repos = config["test_repositories"]

        self.logger.info(
            "Starting benchmark suite",
            total_repos=len(repo_paths),
            iterations=iterations,
        )

        for repo_config in test_repos:
            repo_name = repo_config["name"]

            if repo_name not in repo_paths:
                self.logger.warning(
                    "Repository not found, skipping",
                    repo_name=repo_name,
                )
                continue

            repo_path = repo_paths[repo_name]

            # Run multiple iterations
            for iteration in range(iterations):
                self.logger.info(
                    "Running iteration",
                    repo_name=repo_name,
                    iteration=iteration + 1,
                    total=iterations,
                )

                self.benchmark_repository(repo_config, repo_path)

        return self.results

    def calculate_statistics(self) -> Dict:
        """
        Calculate statistics from benchmark results.

        Returns:
            Dict with statistics by repo size
        """
        # Group by repo size
        by_size = {"small": [], "medium": [], "large": []}

        for result in self.results:
            if result.success:
                by_size[result.repo_size].append(result.duration_seconds)

        stats = {}
        for size, durations in by_size.items():
            if not durations:
                stats[size] = {
                    "count": 0,
                    "median": 0,
                    "p95": 0,
                    "p99": 0,
                    "mean": 0,
                    "min": 0,
                    "max": 0,
                }
                continue

            sorted_durations = sorted(durations)
            stats[size] = {
                "count": len(durations),
                "median": statistics.median(sorted_durations),
                "p95": sorted_durations[int(len(sorted_durations) * 0.95)] if len(sorted_durations) > 1 else sorted_durations[0],
                "p99": sorted_durations[int(len(sorted_durations) * 0.99)] if len(sorted_durations) > 1 else sorted_durations[0],
                "mean": statistics.mean(sorted_durations),
                "min": min(sorted_durations),
                "max": max(sorted_durations),
            }

        return stats

    def generate_report(self) -> str:
        """
        Generate benchmark report.

        Returns:
            Markdown report
        """
        stats = self.calculate_statistics()
        config = self.load_test_repos()
        targets = config["validation_thresholds"]["median_index_time_target_seconds"]

        report = ["# Indexing Performance Benchmark Report\n"]
        report.append(f"Total runs: {len(self.results)}\n")
        report.append(f"Successful: {sum(1 for r in self.results if r.success)}\n")
        report.append(f"Failed: {sum(1 for r in self.results if not r.success)}\n")

        report.append("\n## Performance by Repository Size\n")
        report.append("| Size | Count | Median | P95 | P99 | Mean | Target | Status |")
        report.append("| --- | --- | --- | --- | --- | --- | --- | --- |")

        for size in ["small", "medium", "large"]:
            s = stats[size]
            target = targets[size]
            meets_target = "✅ Pass" if s["median"] <= target else "❌ Fail"

            report.append(
                f"| {size.capitalize()} | {s['count']} | "
                f"{s['median']:.2f}s | {s['p95']:.2f}s | {s['p99']:.2f}s | "
                f"{s['mean']:.2f}s | {target}s | {meets_target} |"
            )

        report.append("\n## Detailed Results\n")
        report.append("| Repository | Size | Language | Indexer | Duration (s) | Symbols | Memory (MB) | Status |")
        report.append("| --- | --- | --- | --- | --- | --- | --- | --- |")

        for result in self.results:
            status = "✅" if result.success else "❌"
            report.append(
                f"| {result.repo_name} | {result.repo_size} | {result.language} | "
                f"{result.indexer_type} | {result.duration_seconds:.2f} | "
                f"{result.symbol_count} | {result.memory_usage_mb:.1f} | {status} |"
            )

        report.append("\n## Indexer Comparison\n")

        # Compare SCIP vs fallback
        scip_results = [r for r in self.results if r.indexer_type == "scip" and r.success]
        fallback_results = [r for r in self.results if r.indexer_type == "fallback" and r.success]

        if scip_results:
            scip_median = statistics.median([r.duration_seconds for r in scip_results])
            report.append(f"- **SCIP**: {len(scip_results)} runs, median {scip_median:.2f}s")

        if fallback_results:
            fallback_median = statistics.median([r.duration_seconds for r in fallback_results])
            report.append(f"- **Fallback**: {len(fallback_results)} runs, median {fallback_median:.2f}s")

        if scip_results and fallback_results:
            speedup = fallback_median / scip_median
            report.append(f"- **SCIP Speedup**: {speedup:.2f}x faster than fallback")

        report.append("\n## Recommendations\n")

        # Add recommendations based on results
        for size in ["small", "medium", "large"]:
            s = stats[size]
            target = targets[size]
            if s["median"] > target:
                report.append(
                    f"- ⚠️  {size.capitalize()} repos exceed target "
                    f"({s['median']:.2f}s vs {target}s target)"
                )

        # Memory recommendations
        max_memory = max((r.memory_usage_mb for r in self.results if r.success), default=0)
        if max_memory > 1000:
            report.append(f"- ⚠️  High memory usage detected: {max_memory:.1f} MB")

        return "\n".join(report)

    def save_results(self, output_dir: Path):
        """
        Save benchmark results to files.

        Args:
            output_dir: Directory to save results
        """
        output_dir.mkdir(parents=True, exist_ok=True)

        # Save raw results as JSON
        results_json = output_dir / "benchmark_results.json"
        with open(results_json, "w") as f:
            json.dump(
                [asdict(r) for r in self.results],
                f,
                indent=2,
            )

        self.logger.info("Saved results", path=str(results_json))

        # Save report as Markdown
        report_md = output_dir / "index_perf_report.md"
        with open(report_md, "w") as f:
            f.write(self.generate_report())

        self.logger.info("Saved report", path=str(report_md))

        # Save statistics as JSON
        stats_json = output_dir / "benchmark_stats.json"
        with open(stats_json, "w") as f:
            json.dump(self.calculate_statistics(), f, indent=2)

        self.logger.info("Saved statistics", path=str(stats_json))


def main():
    """Main entry point for benchmark script."""
    import argparse

    parser = argparse.ArgumentParser(description="Run indexing performance benchmarks")
    parser.add_argument(
        "--repos-dir",
        type=Path,
        required=True,
        help="Directory containing cloned test repositories",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("benchmarks/results"),
        help="Output directory for results",
    )
    parser.add_argument(
        "--iterations",
        type=int,
        default=1,
        help="Number of iterations per repository",
    )

    args = parser.parse_args()

    # Map repository names to paths
    repo_paths = {}
    for repo_dir in args.repos_dir.iterdir():
        if repo_dir.is_dir() and not repo_dir.name.startswith("."):
            repo_paths[repo_dir.name] = repo_dir

    print(f"Found {len(repo_paths)} repositories in {args.repos_dir}")

    # Run benchmarks
    benchmark = IndexingBenchmark()
    benchmark.run_benchmarks(repo_paths, iterations=args.iterations)

    # Save results
    benchmark.save_results(args.output_dir)

    # Print report
    print("\n" + "=" * 80)
    print(benchmark.generate_report())
    print("=" * 80)


if __name__ == "__main__":
    main()
