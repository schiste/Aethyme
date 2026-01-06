"""
End-to-End Workflow Benchmarks for RepoGraph.

Tests complete workflows from start to finish:
- Full index + query + scorecard + autofix pipeline
- Real-world usage patterns
- Multi-repo scenarios
- Performance under realistic conditions
"""

import time
import json
import statistics
from typing import List, Dict, Any, Optional
from dataclasses import dataclass, asdict
from pathlib import Path
import structlog

logger = structlog.get_logger(__name__)


@dataclass
class WorkflowResult:
    """Result of a workflow execution."""
    workflow_name: str
    total_duration: float
    phase_durations: Dict[str, float]
    success: bool
    error: Optional[str] = None
    metadata: Dict[str, Any] = None

    def __post_init__(self):
        if self.metadata is None:
            self.metadata = {}


@dataclass
class BenchmarkResults:
    """Aggregated benchmark results."""
    workflow_name: str
    total_runs: int
    successful_runs: int
    failed_runs: int
    avg_duration: float
    median_duration: float
    p95_duration: float
    p99_duration: float
    min_duration: float
    max_duration: float
    avg_phase_durations: Dict[str, float]


class EndToEndBenchmark:
    """
    Benchmark end-to-end workflows.

    Workflows tested:
    - New Repository Onboarding: Index -> Scorecard -> Autofix
    - Code Search Session: Multiple queries with caching
    - Impact Analysis Flow: Find symbol -> Impact -> Ego
    - Full AI Readiness Pipeline: Index -> Scorecard -> Fix -> Verify
    """

    def __init__(self, output_dir: str = "./benchmarks/results"):
        self.logger = logger.bind(component="EndToEndBenchmark")
        self.output_dir = Path(output_dir)
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.results: List[WorkflowResult] = []

    def run_index_phase(self, repository: str) -> float:
        """
        Run indexing phase.

        Args:
            repository: Repository path

        Returns:
            Duration in seconds
        """
        # TODO: Integrate with actual indexer
        self.logger.info("Running index phase", repository=repository)
        time.sleep(0.1)  # Simulate work
        return 0.1

    def run_scorecard_phase(self, repository: str) -> float:
        """
        Run scorecard phase.

        Args:
            repository: Repository path

        Returns:
            Duration in seconds
        """
        # TODO: Integrate with actual scorecard
        self.logger.info("Running scorecard phase", repository=repository)
        time.sleep(0.05)  # Simulate work
        return 0.05

    def run_autofix_phase(self, repository: str) -> float:
        """
        Run autofix phase.

        Args:
            repository: Repository path

        Returns:
            Duration in seconds
        """
        # TODO: Integrate with actual autofix
        self.logger.info("Running autofix phase", repository=repository)
        time.sleep(0.08)  # Simulate work
        return 0.08

    def run_query_phase(self, query_type: str, query: str) -> float:
        """
        Run query phase.

        Args:
            query_type: Type of query
            query: Query string

        Returns:
            Duration in seconds
        """
        # TODO: Integrate with actual query engine
        self.logger.info("Running query phase", query_type=query_type)
        time.sleep(0.02)  # Simulate work
        return 0.02

    def workflow_new_repository_onboarding(
        self,
        repository: str,
    ) -> WorkflowResult:
        """
        Benchmark: New Repository Onboarding.

        Steps:
        1. Index repository
        2. Run scorecard
        3. Apply safe autofixes
        4. Re-run scorecard to verify

        Args:
            repository: Repository path

        Returns:
            WorkflowResult
        """
        start_time = time.time()
        phase_durations = {}

        try:
            # Phase 1: Index
            phase_durations["index"] = self.run_index_phase(repository)

            # Phase 2: Initial scorecard
            phase_durations["scorecard_initial"] = self.run_scorecard_phase(repository)

            # Phase 3: Autofix
            phase_durations["autofix"] = self.run_autofix_phase(repository)

            # Phase 4: Verification scorecard
            phase_durations["scorecard_verify"] = self.run_scorecard_phase(repository)

            total_duration = time.time() - start_time

            return WorkflowResult(
                workflow_name="new_repository_onboarding",
                total_duration=total_duration,
                phase_durations=phase_durations,
                success=True,
                metadata={"repository": repository},
            )

        except Exception as e:
            total_duration = time.time() - start_time
            self.logger.error("Workflow failed", error=str(e))
            return WorkflowResult(
                workflow_name="new_repository_onboarding",
                total_duration=total_duration,
                phase_durations=phase_durations,
                success=False,
                error=str(e),
            )

    def workflow_code_search_session(
        self,
        queries: List[tuple[str, str]],
    ) -> WorkflowResult:
        """
        Benchmark: Code Search Session.

        Simulates a typical search session with multiple queries.

        Args:
            queries: List of (query_type, query) tuples

        Returns:
            WorkflowResult
        """
        start_time = time.time()
        phase_durations = {}

        try:
            for i, (query_type, query) in enumerate(queries):
                phase_key = f"query_{i+1}_{query_type}"
                phase_durations[phase_key] = self.run_query_phase(query_type, query)

            total_duration = time.time() - start_time

            return WorkflowResult(
                workflow_name="code_search_session",
                total_duration=total_duration,
                phase_durations=phase_durations,
                success=True,
                metadata={"query_count": len(queries)},
            )

        except Exception as e:
            total_duration = time.time() - start_time
            self.logger.error("Workflow failed", error=str(e))
            return WorkflowResult(
                workflow_name="code_search_session",
                total_duration=total_duration,
                phase_durations=phase_durations,
                success=False,
                error=str(e),
            )

    def workflow_impact_analysis(
        self,
        symbol: str,
    ) -> WorkflowResult:
        """
        Benchmark: Impact Analysis Flow.

        Steps:
        1. Search for symbol
        2. Get impact analysis
        3. Get ego network

        Args:
            symbol: Symbol to analyze

        Returns:
            WorkflowResult
        """
        start_time = time.time()
        phase_durations = {}

        try:
            phase_durations["search"] = self.run_query_phase("search", symbol)
            phase_durations["impact"] = self.run_query_phase("impact", symbol)
            phase_durations["ego"] = self.run_query_phase("ego", symbol)

            total_duration = time.time() - start_time

            return WorkflowResult(
                workflow_name="impact_analysis",
                total_duration=total_duration,
                phase_durations=phase_durations,
                success=True,
                metadata={"symbol": symbol},
            )

        except Exception as e:
            total_duration = time.time() - start_time
            self.logger.error("Workflow failed", error=str(e))
            return WorkflowResult(
                workflow_name="impact_analysis",
                total_duration=total_duration,
                phase_durations=phase_durations,
                success=False,
                error=str(e),
            )

    def run_benchmark(
        self,
        workflow_name: str,
        iterations: int = 10,
        **kwargs,
    ) -> BenchmarkResults:
        """
        Run a benchmark multiple times.

        Args:
            workflow_name: Name of workflow to benchmark
            iterations: Number of iterations
            **kwargs: Workflow-specific arguments

        Returns:
            BenchmarkResults
        """
        self.logger.info(
            "Starting benchmark",
            workflow_name=workflow_name,
            iterations=iterations,
        )

        results = []

        for i in range(iterations):
            if workflow_name == "new_repository_onboarding":
                result = self.workflow_new_repository_onboarding(**kwargs)
            elif workflow_name == "code_search_session":
                result = self.workflow_code_search_session(**kwargs)
            elif workflow_name == "impact_analysis":
                result = self.workflow_impact_analysis(**kwargs)
            else:
                raise ValueError(f"Unknown workflow: {workflow_name}")

            results.append(result)
            self.results.append(result)

        # Calculate statistics
        successful = [r for r in results if r.success]
        durations = [r.total_duration for r in successful]

        if not durations:
            self.logger.error("No successful runs")
            return BenchmarkResults(
                workflow_name=workflow_name,
                total_runs=len(results),
                successful_runs=0,
                failed_runs=len(results),
                avg_duration=0.0,
                median_duration=0.0,
                p95_duration=0.0,
                p99_duration=0.0,
                min_duration=0.0,
                max_duration=0.0,
                avg_phase_durations={},
            )

        # Calculate percentiles
        sorted_durations = sorted(durations)
        p95_idx = int(len(sorted_durations) * 0.95)
        p99_idx = int(len(sorted_durations) * 0.99)

        # Calculate average phase durations
        avg_phase_durations = {}
        if successful:
            all_phases = set()
            for r in successful:
                all_phases.update(r.phase_durations.keys())

            for phase in all_phases:
                phase_vals = [
                    r.phase_durations.get(phase, 0.0) for r in successful
                    if phase in r.phase_durations
                ]
                if phase_vals:
                    avg_phase_durations[phase] = sum(phase_vals) / len(phase_vals)

        benchmark_results = BenchmarkResults(
            workflow_name=workflow_name,
            total_runs=len(results),
            successful_runs=len(successful),
            failed_runs=len(results) - len(successful),
            avg_duration=statistics.mean(durations),
            median_duration=statistics.median(durations),
            p95_duration=sorted_durations[p95_idx] if p95_idx < len(sorted_durations) else sorted_durations[-1],
            p99_duration=sorted_durations[p99_idx] if p99_idx < len(sorted_durations) else sorted_durations[-1],
            min_duration=min(durations),
            max_duration=max(durations),
            avg_phase_durations=avg_phase_durations,
        )

        self.logger.info(
            "Benchmark complete",
            **asdict(benchmark_results),
        )

        return benchmark_results

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
    """Run end-to-end benchmarks."""
    benchmark = EndToEndBenchmark()

    # Benchmark 1: New Repository Onboarding
    results1 = benchmark.run_benchmark(
        "new_repository_onboarding",
        iterations=10,
        repository="example-repo",
    )
    print(f"\nNew Repository Onboarding:")
    print(f"  Avg: {results1.avg_duration:.3f}s")
    print(f"  p95: {results1.p95_duration:.3f}s")

    # Benchmark 2: Code Search Session
    results2 = benchmark.run_benchmark(
        "code_search_session",
        iterations=10,
        queries=[
            ("search", "authenticate"),
            ("search", "UserRepository"),
            ("search", "get_db"),
        ],
    )
    print(f"\nCode Search Session:")
    print(f"  Avg: {results2.avg_duration:.3f}s")
    print(f"  p95: {results2.p95_duration:.3f}s")

    # Benchmark 3: Impact Analysis
    results3 = benchmark.run_benchmark(
        "impact_analysis",
        iterations=10,
        symbol="authenticate_user",
    )
    print(f"\nImpact Analysis:")
    print(f"  Avg: {results3.avg_duration:.3f}s")
    print(f"  p95: {results3.p95_duration:.3f}s")

    # Export results
    benchmark.export_results("end_to_end_benchmark_results.json")


if __name__ == "__main__":
    main()
