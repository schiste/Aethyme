"""
Pytest wrapper for scorecard evaluation.
"""

import pytest
from tests.evals.scorecard_eval import (
    ScorecardEvaluator,
    generate_scorecard_test_set,
)


@pytest.fixture
def test_set_file(tmp_path):
    """Generate test set fixture."""
    test_file = tmp_path / "scorecard_test_set.json"
    generate_scorecard_test_set(str(test_file))
    return test_file


@pytest.fixture
def evaluator(test_set_file):
    """Create evaluator with test set."""
    evaluator = ScorecardEvaluator()
    evaluator.load_test_cases(str(test_set_file))
    return evaluator


def test_scorecard_evaluator_loads_test_cases(evaluator):
    """Test that evaluator loads test cases."""
    assert len(evaluator.test_cases) > 0


def test_scorecard_find_matching_violation(evaluator):
    """Test finding matching violations."""
    from tests.evals.scorecard_eval import ScorecardTestCase

    test_case = ScorecardTestCase(
        id="test",
        repository="repo",
        file_path="file.py",
        violation_type="missing_docs",
        expected_severity="warning",
        expected_line=10,
    )

    violations = [
        {
            "violation_type": "missing_docs",
            "file_path": "file.py",
            "line": 10,
            "severity": "warning",
        }
    ]

    match = evaluator.find_matching_violation(violations, test_case)
    assert match is not None


def test_scorecard_evaluation_runs(evaluator, tmp_path):
    """Test that evaluation runs without errors."""
    metrics = evaluator.evaluate_all()

    assert metrics.total_test_cases > 0

    # Export results
    output_file = tmp_path / "scorecard_metrics.json"
    evaluator.export_results(str(output_file))

    assert output_file.exists()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
