# Aethyme Evaluation Guide

**Version:** 1.0
**Feature Flag:** `TELEM_EVAL_V1`
**Last Updated:** 2025-01-22

## Overview

Aethyme includes comprehensive evaluation harnesses to measure and maintain quality across three key areas:
1. **Retrieval Quality** - Search/ego/impact query accuracy
2. **Autofix Correctness** - Fix quality and safety
3. **Scorecard Precision** - Violation detection accuracy

This guide covers how to run evaluations, interpret results, and integrate them into your workflow.

## Quick Start

### Run All Evaluations

```bash
# Generate test sets
python -c "from tests.evals.retrieval_eval import generate_golden_test_set; generate_golden_test_set('tests/evals/data/retrieval_test_set.json')"
python -c "from tests.evals.autofix_eval import generate_autofix_test_set; generate_autofix_test_set('tests/evals/data/autofix_test_set.json')"
python -c "from tests.evals.scorecard_eval import generate_scorecard_test_set; generate_scorecard_test_set('tests/evals/data/scorecard_test_set.json')"

# Run evaluations
pytest tests/evals/ -v
```

### Run Individual Evaluations

```bash
# Retrieval
pytest tests/evals/test_retrieval.py -v

# Autofix
pytest tests/evals/test_autofix.py -v

# Scorecard
pytest tests/evals/test_scorecard.py -v
```

## Retrieval Evaluation

### Overview

Evaluates the quality of search, ego, and impact queries using a golden test set of 50+ queries with known correct results.

### Metrics

- **Precision** - Percentage of returned results that are correct
- **Recall** - Percentage of correct results that were found
- **F1 Score** - Harmonic mean of precision and recall
- **MRR** (Mean Reciprocal Rank) - Position of first correct result
- **nDCG** (Normalized Discounted Cumulative Gain) - Ranking quality

### Running Retrieval Eval

```python
from tests.evals.retrieval_eval import run_retrieval_eval

# Run evaluation
metrics = run_retrieval_eval(
    test_file="tests/evals/data/retrieval_test_set.json",
    output_file="results/retrieval_metrics.json"
)

print(f"Precision: {metrics.precision:.2f}%")
print(f"Recall: {metrics.recall:.2f}%")
print(f"MRR: {metrics.mrr:.4f}")
print(f"nDCG: {metrics.ndcg:.4f}")
```

### Test Case Format

```json
{
  "id": "search_func_001",
  "query_type": "search",
  "query": "authenticate_user",
  "repository": "example-app",
  "expected_results": [
    "src/auth/authentication.py:authenticate_user:15",
    "tests/auth/test_authentication.py:test_authenticate_user:45"
  ],
  "relevance_scores": {
    "src/auth/authentication.py:authenticate_user:15": 1.0,
    "tests/auth/test_authentication.py:test_authenticate_user:45": 0.8
  },
  "metadata": {
    "category": "function_search",
    "language": "python"
  }
}
```

### Adding Custom Test Cases

```python
from tests.evals.retrieval_eval import RetrievalEvaluator, RetrievalTestCase

evaluator = RetrievalEvaluator()

test_case = RetrievalTestCase(
    id="custom_001",
    query_type="search",
    query="MyCustomClass",
    repository="my-repo",
    expected_results=["src/custom.py:MyCustomClass:10"],
    relevance_scores={"src/custom.py:MyCustomClass:10": 1.0},
)

evaluator.add_test_case(test_case)
metrics = evaluator.evaluate_all()
```

### Acceptance Criteria

- **Precision** ≥ 85%
- **Recall** ≥ 80%
- **MRR** ≥ 0.7
- **nDCG** ≥ 0.8

## Autofix Evaluation

### Overview

Evaluates autofix correctness using 30+ test cases with known broken files and expected fixes.

### Metrics

- **Correctness Rate** - Percentage of fixes that are correct
- **Safety Pass Rate** - Percentage that pass safety checks
- **No-Regression Rate** - Percentage that don't break tests
- **Average Diff Similarity** - How close fixes are to expected

### Running Autofix Eval

```python
from tests.evals.autofix_eval import run_autofix_eval

metrics = run_autofix_eval(
    test_file="tests/evals/data/autofix_test_set.json",
    output_file="results/autofix_metrics.json"
)

print(f"Correctness: {metrics.correctness_rate:.2f}%")
print(f"Safety: {metrics.safety_pass_rate:.2f}%")
print(f"No Regression: {metrics.no_regression_rate:.2f}%")
```

### Test Case Format

```json
{
  "id": "docs_001",
  "fix_type": "docs",
  "broken_file_path": "src/utils.py",
  "broken_content": "def calculate_total(items):\n    return sum(items)\n",
  "expected_fixed_content": "\"\"\"Calculate sum of items.\"\"\"\ndef calculate_total(items):\n    return sum(items)\n",
  "safety_checks": ["preserves_functionality", "no_breaking_changes"],
  "metadata": {"category": "missing_docstring"}
}
```

### Safety Checks

Available safety checks:
- `no_generated_file_edits` - Prevents editing generated files
- `preserves_functionality` - Ensures functions aren't removed
- `no_breaking_changes` - Ensures imports aren't removed

### Acceptance Criteria

- **Correctness Rate** ≥ 90%
- **Safety Pass Rate** ≥ 95%
- **No-Regression Rate** ≥ 98%

## Scorecard Evaluation

### Overview

Evaluates scorecard detector precision using repositories with known violations.

### Metrics

- **Precision** - TP / (TP + FP) - Accuracy of detections
- **Recall** - TP / (TP + FN) - Coverage of detections
- **F1 Score** - Harmonic mean of precision and recall
- **False Positive Rate** - FP / (FP + TN)
- **False Negative Rate** - FN / (FN + TP)
- **Severity Accuracy** - Percentage with correct severity

### Running Scorecard Eval

```python
from tests.evals.scorecard_eval import run_scorecard_eval

metrics = run_scorecard_eval(
    test_file="tests/evals/data/scorecard_test_set.json",
    output_file="results/scorecard_metrics.json"
)

print(f"Precision: {metrics.precision:.2f}%")
print(f"False Positive Rate: {metrics.false_positive_rate:.2f}%")
print(f"Severity Accuracy: {metrics.severity_accuracy:.2f}%")
```

### Test Case Format

```json
{
  "id": "selector_001",
  "repository": "test-repo",
  "file_path": "src/components/Button.tsx",
  "violation_type": "missing_data_ui_selector",
  "expected_severity": "warning",
  "expected_line": 15,
  "should_detect": true,
  "metadata": {"category": "data_ui"}
}
```

### False Positive Tests

Set `should_detect: false` to test that correct code doesn't trigger violations:

```json
{
  "id": "false_pos_001",
  "repository": "test-repo",
  "file_path": "src/components/GoodButton.tsx",
  "violation_type": "missing_data_ui_selector",
  "expected_severity": "warning",
  "should_detect": false,
  "metadata": {"category": "false_positive_test"}
}
```

### Acceptance Criteria

- **Precision** ≥ 85%
- **Recall** ≥ 80%
- **False Positive Rate** ≤ 10%
- **Severity Accuracy** ≥ 90%

## CI Integration

### GitHub Actions Workflow

Evaluations run automatically on every PR via `.github/workflows/evals.yml`:

```yaml
name: Evaluation Tests

on:
  pull_request:
    branches: [main, develop]
  push:
    branches: [main]

jobs:
  retrieval-eval:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run retrieval evaluation
        run: pytest tests/evals/test_retrieval.py -v
```

### Regression Detection

PRs are automatically checked for regressions:

```python
# Fail PR if precision drops > 5%
if current_precision < baseline_precision - 5.0:
    print(f"REGRESSION: Precision degraded by {degradation:.2f}%")
    sys.exit(1)
```

### PR Comments

Results are automatically posted to PRs:

```markdown
## Evaluation Results

### Retrieval Evaluation
- Precision: 92.3%
- Recall: 88.7%
- MRR: 0.85

### Autofix Evaluation
- Correctness: 95.2%
- Safety Pass Rate: 98.1%

### Scorecard Evaluation
- Precision: 89.4%
- False Positive Rate: 7.2%
```

## Performance Benchmarks

### End-to-End Benchmarks

Test complete workflows:

```bash
python benchmarks/comprehensive/end_to_end_benchmark.py
```

Workflows tested:
- **New Repository Onboarding** - Index → Scorecard → Autofix
- **Code Search Session** - Multiple queries with caching
- **Impact Analysis Flow** - Search → Impact → Ego

### Load Benchmarks

Test system under concurrent load:

```bash
python benchmarks/comprehensive/load_benchmark.py
```

Tests:
- **Concurrent Queries** - 100+ simultaneous queries
- **Concurrent Indexing** - Multiple repos in parallel
- **Mixed Workload** - Queries + indexing + scorecards
- **Sustained Load** - Continuous load over time

### Regression Detection

Automatically detect performance regressions:

```bash
python benchmarks/comprehensive/regression_detection.py
```

Configuration:
- **Warning Threshold** - 10% degradation
- **Critical Threshold** - 25% degradation

Metrics checked:
- Index latency (p50, p95, p99)
- Query latency (p50, p95, p99)
- Throughput (ops/sec)

## Best Practices

### 1. Run Evals Before Merging

Always run evaluations locally before submitting PRs:

```bash
make test-evals  # or pytest tests/evals/ -v
```

### 2. Update Test Sets Regularly

Add new test cases as you discover edge cases:

```python
# Add to retrieval test set
{
  "id": "edge_case_001",
  "query_type": "search",
  "query": "edge_case_function",
  ...
}
```

### 3. Monitor Eval Trends

Track evaluation metrics over time using Grafana dashboards.

### 4. Investigate Failures

When evals fail, investigate:
1. Check specific test cases that failed
2. Compare expected vs actual results
3. Verify recent code changes
4. Update test cases if expectations changed

### 5. Use Evals for Features

Add eval test cases when implementing new features:

```python
# When adding new violation detector
test_case = ScorecardTestCase(
    id="new_detector_001",
    violation_type="new_violation_type",
    should_detect=True,
    ...
)
```

## Troubleshooting

### Eval Failures

**Precision Drop**
- Check recent code changes to search/query logic
- Verify test cases are still valid
- Review false positives in results

**Recall Drop**
- Check if indexing is complete
- Verify all expected symbols are indexed
- Review false negatives in results

### Performance Issues

**Slow Evals**
- Run with smaller test sets for development
- Use caching where possible
- Parallelize test execution

### CI Failures

**Flaky Tests**
- Check for non-deterministic behavior
- Verify test isolation
- Add retries for network-dependent tests

## Viewing Results

### Console Output

```bash
pytest tests/evals/ -v --tb=short
```

### JSON Reports

```python
import json

with open('results/retrieval_metrics.json') as f:
    metrics = json.load(f)

print(json.dumps(metrics, indent=2))
```

### Grafana Dashboards

View historical trends:
1. Open Grafana (http://localhost:3000)
2. Navigate to "Evaluation Results" dashboard
3. Filter by time range and eval type

## Advanced Topics

### Custom Metrics

Add custom evaluation metrics:

```python
class CustomEvaluator:
    def evaluate_custom_metric(self, expected, actual):
        # Custom logic
        return score
```

### Parallel Execution

Run evaluations in parallel:

```python
from concurrent.futures import ThreadPoolExecutor

with ThreadPoolExecutor(max_workers=4) as executor:
    futures = [
        executor.submit(evaluator.evaluate_single, tc)
        for tc in test_cases
    ]
```

### Statistical Significance

Test for statistical significance:

```python
from scipy import stats

# Compare two evaluation runs
t_stat, p_value = stats.ttest_ind(baseline_scores, current_scores)

if p_value < 0.05:
    print("Statistically significant difference")
```

## See Also

- [Telemetry Guide](./telemetry-guide.md)
- [Performance Budgets](./architecture/performance-budgets.md)
- [CI/CD Documentation](./architecture/deployment.md)

## Support

For issues or questions:
1. Review eval results in CI logs
2. Check Grafana "Evaluation Results" dashboard
3. Consult #aethyme-quality channel
