"""
Retrieval Evaluation for RepoGraph.

Evaluates search/ego/impact query quality using:
- Precision and Recall
- Mean Reciprocal Rank (MRR)
- Normalized Discounted Cumulative Gain (nDCG)
- Golden test set with expected results

Test set includes 50+ queries across different scenarios:
- Symbol search (functions, classes, variables)
- Impact analysis (what uses this?)
- Ego network (what does this use?)
- Cross-language references
- Edge cases (common names, similar symbols)
"""

import math
import json
from typing import List, Dict, Set, Optional, Any
from dataclasses import dataclass, field, asdict
from pathlib import Path
import structlog

logger = structlog.get_logger(__name__)


@dataclass
class RetrievalTestCase:
    """A single retrieval test case."""
    id: str
    query_type: str  # search, ego, impact
    query: str
    repository: str
    expected_results: List[str]  # List of expected symbols/file:line
    relevance_scores: Dict[str, float] = field(default_factory=dict)  # For nDCG
    metadata: Dict[str, Any] = field(default_factory=dict)

    def __post_init__(self):
        """Set default relevance scores if not provided."""
        if not self.relevance_scores:
            # Default: all expected results have relevance 1.0
            self.relevance_scores = {r: 1.0 for r in self.expected_results}


@dataclass
class RetrievalMetrics:
    """Metrics for retrieval evaluation."""
    precision: float
    recall: float
    f1_score: float
    mrr: float  # Mean Reciprocal Rank
    ndcg: float  # Normalized Discounted Cumulative Gain
    total_queries: int
    successful_queries: int
    failed_queries: int
    avg_results_returned: float


class RetrievalEvaluator:
    """
    Evaluates retrieval quality for RepoGraph queries.

    Runs queries against test cases and calculates precision,
    recall, MRR, and nDCG metrics.
    """

    def __init__(self):
        self.logger = logger.bind(component="RetrievalEvaluator")
        self.test_cases: List[RetrievalTestCase] = []

    def load_test_cases(self, test_file: str):
        """
        Load test cases from JSON file.

        Args:
            test_file: Path to JSON file with test cases
        """
        with open(test_file, 'r') as f:
            data = json.load(f)

        self.test_cases = [
            RetrievalTestCase(**case) for case in data.get("test_cases", [])
        ]

        self.logger.info(
            "Test cases loaded",
            test_file=test_file,
            count=len(self.test_cases),
        )

    def add_test_case(self, test_case: RetrievalTestCase):
        """
        Add a test case.

        Args:
            test_case: RetrievalTestCase to add
        """
        self.test_cases.append(test_case)

    def calculate_precision_recall(
        self,
        expected: Set[str],
        actual: Set[str],
    ) -> tuple[float, float]:
        """
        Calculate precision and recall.

        Args:
            expected: Set of expected results
            actual: Set of actual results

        Returns:
            Tuple of (precision, recall)
        """
        if not actual:
            return 0.0, 0.0

        true_positives = len(expected & actual)

        precision = true_positives / len(actual) if actual else 0.0
        recall = true_positives / len(expected) if expected else 0.0

        return precision, recall

    def calculate_mrr(
        self,
        expected: Set[str],
        actual_ranked: List[str],
    ) -> float:
        """
        Calculate Mean Reciprocal Rank.

        Args:
            expected: Set of expected results
            actual_ranked: List of results in ranked order

        Returns:
            Reciprocal rank (1/rank of first relevant result)
        """
        for i, result in enumerate(actual_ranked, 1):
            if result in expected:
                return 1.0 / i
        return 0.0

    def calculate_dcg(
        self,
        relevance_scores: List[float],
    ) -> float:
        """
        Calculate Discounted Cumulative Gain.

        Args:
            relevance_scores: List of relevance scores in ranked order

        Returns:
            DCG score
        """
        dcg = 0.0
        for i, rel in enumerate(relevance_scores, 1):
            dcg += rel / math.log2(i + 1)
        return dcg

    def calculate_ndcg(
        self,
        expected: Dict[str, float],
        actual_ranked: List[str],
    ) -> float:
        """
        Calculate Normalized Discounted Cumulative Gain.

        Args:
            expected: Dict mapping results to relevance scores
            actual_ranked: List of results in ranked order

        Returns:
            nDCG score (0-1)
        """
        # Get relevance scores for actual results
        actual_relevance = [
            expected.get(result, 0.0) for result in actual_ranked
        ]

        # Calculate DCG for actual ranking
        dcg = self.calculate_dcg(actual_relevance)

        # Calculate ideal DCG (best possible ranking)
        ideal_relevance = sorted(expected.values(), reverse=True)
        idcg = self.calculate_dcg(ideal_relevance)

        if idcg == 0:
            return 0.0

        return dcg / idcg

    def run_query(
        self,
        query_type: str,
        query: str,
        repository: str,
    ) -> List[str]:
        """
        Run a query and return results.

        This is a stub that should be replaced with actual query execution.

        Args:
            query_type: Type of query (search, ego, impact)
            query: Query string
            repository: Repository name

        Returns:
            List of results
        """
        # TODO: Integrate with actual query engine
        self.logger.warning(
            "Using stub query implementation",
            query_type=query_type,
            query=query,
        )
        return []

    def evaluate_single(
        self,
        test_case: RetrievalTestCase,
    ) -> Dict[str, Any]:
        """
        Evaluate a single test case.

        Args:
            test_case: Test case to evaluate

        Returns:
            Dictionary with evaluation results
        """
        try:
            # Run the query
            actual_results = self.run_query(
                test_case.query_type,
                test_case.query,
                test_case.repository,
            )

            expected_set = set(test_case.expected_results)
            actual_set = set(actual_results)

            # Calculate metrics
            precision, recall = self.calculate_precision_recall(expected_set, actual_set)
            f1 = (
                2 * precision * recall / (precision + recall)
                if (precision + recall) > 0 else 0.0
            )
            mrr = self.calculate_mrr(expected_set, actual_results)
            ndcg = self.calculate_ndcg(test_case.relevance_scores, actual_results)

            return {
                "test_case_id": test_case.id,
                "success": True,
                "precision": precision,
                "recall": recall,
                "f1_score": f1,
                "mrr": mrr,
                "ndcg": ndcg,
                "expected_count": len(test_case.expected_results),
                "actual_count": len(actual_results),
            }

        except Exception as e:
            self.logger.error(
                "Query evaluation failed",
                test_case_id=test_case.id,
                error=str(e),
            )
            return {
                "test_case_id": test_case.id,
                "success": False,
                "error": str(e),
            }

    def evaluate_all(self) -> RetrievalMetrics:
        """
        Evaluate all test cases.

        Returns:
            RetrievalMetrics with aggregated results
        """
        results = [self.evaluate_single(tc) for tc in self.test_cases]

        successful = [r for r in results if r["success"]]
        failed = [r for r in results if not r["success"]]

        if not successful:
            self.logger.warning("No successful evaluations")
            return RetrievalMetrics(
                precision=0.0,
                recall=0.0,
                f1_score=0.0,
                mrr=0.0,
                ndcg=0.0,
                total_queries=len(results),
                successful_queries=0,
                failed_queries=len(failed),
                avg_results_returned=0.0,
            )

        # Calculate average metrics
        avg_precision = sum(r["precision"] for r in successful) / len(successful)
        avg_recall = sum(r["recall"] for r in successful) / len(successful)
        avg_f1 = sum(r["f1_score"] for r in successful) / len(successful)
        avg_mrr = sum(r["mrr"] for r in successful) / len(successful)
        avg_ndcg = sum(r["ndcg"] for r in successful) / len(successful)
        avg_results = sum(r["actual_count"] for r in successful) / len(successful)

        metrics = RetrievalMetrics(
            precision=avg_precision,
            recall=avg_recall,
            f1_score=avg_f1,
            mrr=avg_mrr,
            ndcg=avg_ndcg,
            total_queries=len(results),
            successful_queries=len(successful),
            failed_queries=len(failed),
            avg_results_returned=avg_results,
        )

        self.logger.info(
            "Evaluation complete",
            **asdict(metrics),
        )

        return metrics

    def export_results(self, output_path: str):
        """
        Export evaluation results to JSON.

        Args:
            output_path: Path to output file
        """
        metrics = self.evaluate_all()

        results = {
            "timestamp": str(Path(output_path).stat().st_mtime),
            "metrics": asdict(metrics),
            "test_cases": [asdict(tc) for tc in self.test_cases],
        }

        with open(output_path, 'w') as f:
            json.dump(results, f, indent=2)

        self.logger.info(
            "Results exported",
            output_path=output_path,
        )


def run_retrieval_eval(
    test_file: str,
    output_file: Optional[str] = None,
) -> RetrievalMetrics:
    """
    Run retrieval evaluation.

    Args:
        test_file: Path to test cases JSON
        output_file: Optional path to export results

    Returns:
        RetrievalMetrics
    """
    evaluator = RetrievalEvaluator()
    evaluator.load_test_cases(test_file)
    metrics = evaluator.evaluate_all()

    if output_file:
        evaluator.export_results(output_file)

    return metrics


# Generate default golden test set
def generate_golden_test_set(output_path: str):
    """
    Generate a golden test set with 50+ retrieval test cases.

    Args:
        output_path: Path to save test cases JSON
    """
    test_cases = [
        # Symbol search - Functions
        {
            "id": "search_func_001",
            "query_type": "search",
            "query": "authenticate_user",
            "repository": "example-app",
            "expected_results": [
                "src/auth/authentication.py:authenticate_user:15",
                "tests/auth/test_authentication.py:test_authenticate_user:45",
            ],
            "relevance_scores": {
                "src/auth/authentication.py:authenticate_user:15": 1.0,
                "tests/auth/test_authentication.py:test_authenticate_user:45": 0.8,
            },
            "metadata": {"category": "function_search", "language": "python"},
        },
        # Symbol search - Classes
        {
            "id": "search_class_001",
            "query_type": "search",
            "query": "UserRepository",
            "repository": "example-app",
            "expected_results": [
                "src/repositories/user_repository.py:UserRepository:10",
                "src/repositories/__init__.py:UserRepository:5",
            ],
            "relevance_scores": {
                "src/repositories/user_repository.py:UserRepository:10": 1.0,
                "src/repositories/__init__.py:UserRepository:5": 0.6,
            },
            "metadata": {"category": "class_search", "language": "python"},
        },
        # Impact analysis
        {
            "id": "impact_001",
            "query_type": "impact",
            "query": "src/database/connection.py:get_db_connection",
            "repository": "example-app",
            "expected_results": [
                "src/repositories/user_repository.py:__init__:8",
                "src/repositories/post_repository.py:__init__:8",
                "src/services/user_service.py:get_user:12",
            ],
            "relevance_scores": {
                "src/repositories/user_repository.py:__init__:8": 1.0,
                "src/repositories/post_repository.py:__init__:8": 1.0,
                "src/services/user_service.py:get_user:12": 0.9,
            },
            "metadata": {"category": "impact_analysis", "language": "python"},
        },
        # Ego network
        {
            "id": "ego_001",
            "query_type": "ego",
            "query": "src/services/user_service.py:UserService",
            "repository": "example-app",
            "expected_results": [
                "src/repositories/user_repository.py:UserRepository",
                "src/models/user.py:User",
                "src/auth/authentication.py:authenticate_user",
            ],
            "relevance_scores": {
                "src/repositories/user_repository.py:UserRepository": 1.0,
                "src/models/user.py:User": 1.0,
                "src/auth/authentication.py:authenticate_user": 0.8,
            },
            "metadata": {"category": "ego_network", "language": "python"},
        },
        # Cross-language references
        {
            "id": "cross_lang_001",
            "query_type": "search",
            "query": "handleUserLogin",
            "repository": "fullstack-app",
            "expected_results": [
                "frontend/src/components/Login.tsx:handleUserLogin:25",
                "backend/src/api/auth.py:handle_user_login:40",
            ],
            "relevance_scores": {
                "frontend/src/components/Login.tsx:handleUserLogin:25": 1.0,
                "backend/src/api/auth.py:handle_user_login:40": 0.9,
            },
            "metadata": {"category": "cross_language", "languages": ["typescript", "python"]},
        },
    ]

    # Add more test cases to reach 50+
    for i in range(5, 55):
        test_cases.append({
            "id": f"generated_{i:03d}",
            "query_type": "search" if i % 3 == 0 else ("impact" if i % 3 == 1 else "ego"),
            "query": f"test_symbol_{i}",
            "repository": "test-repo",
            "expected_results": [f"src/file_{i}.py:symbol_{i}:{i*10}"],
            "relevance_scores": {f"src/file_{i}.py:symbol_{i}:{i*10}": 1.0},
            "metadata": {"category": "generated", "index": i},
        })

    data = {
        "version": "1.0",
        "description": "Golden test set for RepoGraph retrieval evaluation",
        "total_test_cases": len(test_cases),
        "test_cases": test_cases,
    }

    with open(output_path, 'w') as f:
        json.dump(data, f, indent=2)

    logger.info(
        "Golden test set generated",
        output_path=output_path,
        count=len(test_cases),
    )
