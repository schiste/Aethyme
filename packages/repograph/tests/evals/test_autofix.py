"""
Pytest wrapper for autofix evaluation.
"""

import pytest
from tests.evals.autofix_eval import (
    AutofixEvaluator,
    generate_autofix_test_set,
)


@pytest.fixture
def test_set_file(tmp_path):
    """Generate test set fixture."""
    test_file = tmp_path / "autofix_test_set.json"
    generate_autofix_test_set(str(test_file))
    return test_file


@pytest.fixture
def evaluator(test_set_file, tmp_path):
    """Create evaluator with test set."""
    evaluator = AutofixEvaluator(workspace_dir=str(tmp_path / "workspace"))
    evaluator.load_test_cases(str(test_set_file))
    return evaluator


def test_autofix_evaluator_loads_test_cases(evaluator):
    """Test that evaluator loads test cases."""
    assert len(evaluator.test_cases) >= 30


def test_autofix_diff_similarity(evaluator):
    """Test diff similarity calculation."""
    similarity = evaluator.calculate_diff_similarity(
        "hello world",
        "hello world!"
    )

    assert 0 <= similarity <= 1
    assert similarity > 0.9  # Very similar


def test_autofix_safety_checks(evaluator):
    """Test safety checks."""
    from tests.evals.autofix_eval import AutofixTestCase

    test_case = AutofixTestCase(
        id="test",
        fix_type="docs",
        broken_file_path="test.py",
        broken_content="def foo(): pass",
        expected_fixed_content='"""doc"""\ndef foo(): pass',
        safety_checks=["preserves_functionality"],
    )

    # Test with safe content
    violations = evaluator.check_safety(test_case, test_case.expected_fixed_content)
    assert len(violations) == 0


def test_autofix_evaluation_runs(evaluator, tmp_path):
    """Test that evaluation runs without errors."""
    metrics = evaluator.evaluate_all()

    assert metrics.total_test_cases > 0

    # Export results
    output_file = tmp_path / "autofix_metrics.json"
    evaluator.export_results(str(output_file))

    assert output_file.exists()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
