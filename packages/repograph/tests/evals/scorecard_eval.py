"""
Scorecard Precision Evaluation for RepoGraph.

Evaluates scorecard detector quality using:
- False positive rate
- False negative rate
- Severity accuracy
- Known violations in test repositories

Test set includes repositories with known violations:
- Missing data-ui selectors
- Missing/incorrect documentation
- Absolute links
- i18n gaps
- Generated file edits
- Type/schema mismatches
"""

import json
from typing import List, Dict, Set, Optional, Any
from dataclasses import dataclass, asdict
from pathlib import Path
import structlog

logger = structlog.get_logger(__name__)


@dataclass
class ScorecardTestCase:
    """A single scorecard test case."""
    id: str
    repository: str
    file_path: str
    violation_type: str
    expected_severity: str  # blocker, warning, info
    expected_line: Optional[int] = None
    should_detect: bool = True  # False for false positive tests
    metadata: Dict[str, Any] = None

    def __post_init__(self):
        if self.metadata is None:
            self.metadata = {}


@dataclass
class ScorecardMetrics:
    """Metrics for scorecard evaluation."""
    precision: float  # TP / (TP + FP)
    recall: float  # TP / (TP + FN)
    f1_score: float
    false_positive_rate: float
    false_negative_rate: float
    severity_accuracy: float  # Percentage with correct severity
    total_test_cases: int
    true_positives: int
    false_positives: int
    false_negatives: int
    true_negatives: int
    severity_correct: int
    severity_incorrect: int


class ScorecardEvaluator:
    """
    Evaluates scorecard detector precision for RepoGraph.

    Runs scorecard on test repositories with known violations
    and validates detection accuracy.
    """

    def __init__(self):
        self.logger = logger.bind(component="ScorecardEvaluator")
        self.test_cases: List[ScorecardTestCase] = []

    def load_test_cases(self, test_file: str):
        """
        Load test cases from JSON file.

        Args:
            test_file: Path to JSON file with test cases
        """
        with open(test_file, 'r') as f:
            data = json.load(f)

        self.test_cases = [
            ScorecardTestCase(**case) for case in data.get("test_cases", [])
        ]

        self.logger.info(
            "Test cases loaded",
            test_file=test_file,
            count=len(self.test_cases),
        )

    def add_test_case(self, test_case: ScorecardTestCase):
        """
        Add a test case.

        Args:
            test_case: ScorecardTestCase to add
        """
        self.test_cases.append(test_case)

    def run_scorecard(
        self,
        repository: str,
        file_path: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """
        Run scorecard on repository/file.

        This is a stub that should be replaced with actual scorecard execution.

        Args:
            repository: Repository path
            file_path: Optional specific file to check

        Returns:
            List of violations found
        """
        # TODO: Integrate with actual scorecard engine
        self.logger.warning(
            "Using stub scorecard implementation",
            repository=repository,
            file_path=file_path,
        )
        return []

    def find_matching_violation(
        self,
        violations: List[Dict[str, Any]],
        test_case: ScorecardTestCase,
    ) -> Optional[Dict[str, Any]]:
        """
        Find a violation matching the test case.

        Args:
            violations: List of detected violations
            test_case: Test case to match

        Returns:
            Matching violation or None
        """
        for violation in violations:
            if violation.get("violation_type") != test_case.violation_type:
                continue

            if violation.get("file_path") != test_case.file_path:
                continue

            if test_case.expected_line is not None:
                if violation.get("line") != test_case.expected_line:
                    continue

            return violation

        return None

    def evaluate_single(
        self,
        test_case: ScorecardTestCase,
    ) -> Dict[str, Any]:
        """
        Evaluate a single test case.

        Args:
            test_case: Test case to evaluate

        Returns:
            Dictionary with evaluation results
        """
        try:
            # Run scorecard
            violations = self.run_scorecard(
                test_case.repository,
                test_case.file_path,
            )

            # Find matching violation
            matching_violation = self.find_matching_violation(violations, test_case)

            # Classify result
            if test_case.should_detect:
                # We expect to find this violation
                if matching_violation:
                    # True positive
                    severity_correct = (
                        matching_violation.get("severity") == test_case.expected_severity
                    )
                    return {
                        "test_case_id": test_case.id,
                        "result": "true_positive",
                        "severity_correct": severity_correct,
                        "detected_severity": matching_violation.get("severity"),
                        "expected_severity": test_case.expected_severity,
                    }
                else:
                    # False negative
                    return {
                        "test_case_id": test_case.id,
                        "result": "false_negative",
                        "severity_correct": False,
                    }
            else:
                # We expect NOT to find this violation
                if matching_violation:
                    # False positive
                    return {
                        "test_case_id": test_case.id,
                        "result": "false_positive",
                        "severity_correct": False,
                        "detected_severity": matching_violation.get("severity"),
                    }
                else:
                    # True negative
                    return {
                        "test_case_id": test_case.id,
                        "result": "true_negative",
                        "severity_correct": True,
                    }

        except Exception as e:
            self.logger.error(
                "Scorecard evaluation failed",
                test_case_id=test_case.id,
                error=str(e),
            )
            return {
                "test_case_id": test_case.id,
                "result": "error",
                "error": str(e),
            }

    def evaluate_all(self) -> ScorecardMetrics:
        """
        Evaluate all test cases.

        Returns:
            ScorecardMetrics with aggregated results
        """
        results = [self.evaluate_single(tc) for tc in self.test_cases]

        # Count results
        tp = sum(1 for r in results if r["result"] == "true_positive")
        fp = sum(1 for r in results if r["result"] == "false_positive")
        fn = sum(1 for r in results if r["result"] == "false_negative")
        tn = sum(1 for r in results if r["result"] == "true_negative")

        total = len(results)

        # Calculate rates
        precision = tp / (tp + fp) if (tp + fp) > 0 else 0.0
        recall = tp / (tp + fn) if (tp + fn) > 0 else 0.0
        f1 = (
            2 * precision * recall / (precision + recall)
            if (precision + recall) > 0 else 0.0
        )

        fpr = fp / (fp + tn) if (fp + tn) > 0 else 0.0
        fnr = fn / (fn + tp) if (fn + tp) > 0 else 0.0

        # Severity accuracy (only for true positives)
        severity_correct = sum(
            1 for r in results
            if r["result"] == "true_positive" and r.get("severity_correct", False)
        )
        severity_incorrect = tp - severity_correct

        severity_accuracy = (
            severity_correct / tp * 100 if tp > 0 else 0.0
        )

        metrics = ScorecardMetrics(
            precision=precision * 100,
            recall=recall * 100,
            f1_score=f1 * 100,
            false_positive_rate=fpr * 100,
            false_negative_rate=fnr * 100,
            severity_accuracy=severity_accuracy,
            total_test_cases=total,
            true_positives=tp,
            false_positives=fp,
            false_negatives=fn,
            true_negatives=tn,
            severity_correct=severity_correct,
            severity_incorrect=severity_incorrect,
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
            "metrics": asdict(metrics),
            "test_cases": [asdict(tc) for tc in self.test_cases],
        }

        with open(output_path, 'w') as f:
            json.dump(results, f, indent=2)

        self.logger.info(
            "Results exported",
            output_path=output_path,
        )


def run_scorecard_eval(
    test_file: str,
    output_file: Optional[str] = None,
) -> ScorecardMetrics:
    """
    Run scorecard evaluation.

    Args:
        test_file: Path to test cases JSON
        output_file: Optional path to export results

    Returns:
        ScorecardMetrics
    """
    evaluator = ScorecardEvaluator()
    evaluator.load_test_cases(test_file)
    metrics = evaluator.evaluate_all()

    if output_file:
        evaluator.export_results(output_file)

    return metrics


# Generate default test set
def generate_scorecard_test_set(output_path: str):
    """
    Generate test set with scorecard test cases.

    Args:
        output_path: Path to save test cases JSON
    """
    test_cases = [
        # Missing data-ui selectors
        {
            "id": "selector_001",
            "repository": "test-repo",
            "file_path": "src/components/Button.tsx",
            "violation_type": "missing_data_ui_selector",
            "expected_severity": "warning",
            "expected_line": 15,
            "should_detect": True,
            "metadata": {"category": "data_ui"},
        },
        # Missing documentation
        {
            "id": "docs_001",
            "repository": "test-repo",
            "file_path": "src/api/users.py",
            "violation_type": "missing_docstring",
            "expected_severity": "warning",
            "expected_line": 10,
            "should_detect": True,
            "metadata": {"category": "documentation"},
        },
        # Absolute links
        {
            "id": "links_001",
            "repository": "test-repo",
            "file_path": "docs/guide.md",
            "violation_type": "absolute_link",
            "expected_severity": "warning",
            "expected_line": 25,
            "should_detect": True,
            "metadata": {"category": "links"},
        },
        # i18n gaps
        {
            "id": "i18n_001",
            "repository": "test-repo",
            "file_path": "src/components/Welcome.tsx",
            "violation_type": "hardcoded_string",
            "expected_severity": "warning",
            "expected_line": 8,
            "should_detect": True,
            "metadata": {"category": "i18n"},
        },
        # Generated file edits
        {
            "id": "generated_001",
            "repository": "test-repo",
            "file_path": "src/generated/schema.py",
            "violation_type": "generated_file_edit",
            "expected_severity": "blocker",
            "expected_line": 5,
            "should_detect": True,
            "metadata": {"category": "generated_files"},
        },
        # False positive test - correct code
        {
            "id": "false_pos_001",
            "repository": "test-repo",
            "file_path": "src/components/GoodButton.tsx",
            "violation_type": "missing_data_ui_selector",
            "expected_severity": "warning",
            "should_detect": False,
            "metadata": {"category": "false_positive_test"},
        },
    ]

    # Add more test cases
    for i in range(6, 25):
        violation_type = [
            "missing_data_ui_selector",
            "missing_docstring",
            "absolute_link",
            "hardcoded_string",
        ][i % 4]

        severity = ["warning", "blocker", "info"][i % 3]

        test_cases.append({
            "id": f"generated_{i:03d}",
            "repository": "test-repo",
            "file_path": f"src/file_{i}.py",
            "violation_type": violation_type,
            "expected_severity": severity,
            "expected_line": i * 10,
            "should_detect": True,
            "metadata": {"category": "generated", "index": i},
        })

    data = {
        "version": "1.0",
        "description": "Test set for RepoGraph scorecard evaluation",
        "total_test_cases": len(test_cases),
        "test_cases": test_cases,
    }

    with open(output_path, 'w') as f:
        json.dump(data, f, indent=2)

    logger.info(
        "Scorecard test set generated",
        output_path=output_path,
        count=len(test_cases),
    )
