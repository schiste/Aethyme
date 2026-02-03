"""
Pytest wrapper for retrieval evaluation.
"""

import pytest
import json
from pathlib import Path
from tests.evals.retrieval_eval import (
    RetrievalEvaluator,
    generate_golden_test_set,
)


@pytest.fixture
def test_set_file(tmp_path):
    """Generate test set fixture."""
    test_file = tmp_path / "retrieval_test_set.json"
    generate_golden_test_set(str(test_file))
    return test_file


@pytest.fixture
def evaluator(test_set_file):
    """Create evaluator with test set."""
    evaluator = RetrievalEvaluator()
    evaluator.load_test_cases(str(test_set_file))
    return evaluator


def test_retrieval_evaluator_loads_test_cases(evaluator):
    """Test that evaluator loads test cases."""
    assert len(evaluator.test_cases) > 50


def test_retrieval_precision_recall(evaluator):
    """Test precision and recall calculation."""
    expected = {"a", "b", "c"}
    actual = {"b", "c", "d"}

    precision, recall = evaluator.calculate_precision_recall(expected, actual)

    assert precision == pytest.approx(2/3)  # 2 correct out of 3 returned
    assert recall == pytest.approx(2/3)     # 2 correct out of 3 expected


def test_retrieval_mrr(evaluator):
    """Test MRR calculation."""
    expected = {"a", "b"}
    actual_ranked = ["x", "y", "a", "z"]  # First relevant at position 3

    mrr = evaluator.calculate_mrr(expected, actual_ranked)

    assert mrr == pytest.approx(1/3)


def test_retrieval_ndcg(evaluator):
    """Test nDCG calculation."""
    expected = {"a": 1.0, "b": 0.8, "c": 0.6}
    actual_ranked = ["a", "c", "b"]  # Not ideal order

    ndcg = evaluator.calculate_ndcg(expected, actual_ranked)

    assert 0 <= ndcg <= 1


def test_retrieval_evaluation_runs(evaluator, tmp_path):
    """Test that evaluation runs without errors."""
    metrics = evaluator.evaluate_all()

    assert metrics.total_queries > 0

    # Export results
    output_file = tmp_path / "retrieval_metrics.json"
    evaluator.export_results(str(output_file))

    assert output_file.exists()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
