#!/usr/bin/env bash
# Validate the exact non-Rust performance-reporting inputs without building Aethyme.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
benchmark_dir="$repo_root/packages/aethyme-eval/benchmarks/performance"
workflow="$repo_root/.github/workflows/product-performance.yml"

for path in \
  "$benchmark_dir/broker_latency.py" \
  "$benchmark_dir/test_broker_latency.py" \
  "$benchmark_dir/broker-budgets.json" \
  "$repo_root/packages/aethyme/docs/guides/broker-performance.md" \
  "$workflow"; do
  if [[ ! -f $path || -L $path ]]; then
    printf 'missing or non-regular performance input: %s\n' "$path" >&2
    exit 1
  fi
done

# Parse without writing __pycache__, and enforce that the harness remains
# standard-library-only apart from its local test import.
PYTHONDONTWRITEBYTECODE=1 python3 - "$benchmark_dir" <<'PY'
import ast
import json
from pathlib import Path
import sys
import unittest

benchmark_dir = Path(sys.argv[1])
allowed_stdlib = sys.stdlib_module_names
sources = {
    "broker_latency.py": set(),
    "test_broker_latency.py": {"broker_latency"},
}
required_tests = {
    "test_approved_limits_and_warning_policy",
    "test_exact_limit_is_within_budget",
    "test_invalid_limits_are_rejected",
    "test_invalid_samples_are_not_reported_as_fast",
    "test_median_is_used",
    "test_missing_or_unknown_action_is_invalid",
    "test_overruns_are_warnings_and_keep_raw_samples",
    "test_unknown_schema_or_hard_enforcement_is_rejected",
}
for filename, allowed_local in sources.items():
    path = benchmark_dir / filename
    source = path.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=str(path))
    compile(tree, str(path), "exec")
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            modules = [alias.name.split(".", 1)[0] for alias in node.names]
        elif isinstance(node, ast.ImportFrom) and node.module:
            modules = [node.module.split(".", 1)[0]]
        else:
            continue
        for module in modules:
            if module not in allowed_stdlib and module not in allowed_local:
                raise SystemExit(f"{path}: non-stdlib import is not allowed: {module}")

sys.path.insert(0, str(benchmark_dir))
suite = unittest.defaultTestLoader.discover(
    str(benchmark_dir), pattern="test_broker_latency.py", top_level_dir=str(benchmark_dir)
)

def iter_test_cases(test_suite):
    for item in test_suite:
        if isinstance(item, unittest.TestSuite):
            yield from iter_test_cases(item)
        else:
            yield item

discovered_tests = {case.id().rsplit(".", 1)[-1] for case in iter_test_cases(suite)}
missing_tests = sorted(required_tests - discovered_tests)
if missing_tests:
    raise SystemExit(f"performance policy tests were not discovered: {', '.join(missing_tests)}")

budget_path = benchmark_dir / "broker-budgets.json"
json.loads(budget_path.read_text(encoding="utf-8"))

# Mirror docs_hygiene::documentation_has_last_updated for the one curated guide
# in this fast class. Keep a negative control so removing the metadata cannot
# silently make this lightweight lane weaker than the full suite.
repo_root = benchmark_dir.parents[3]
guide = repo_root / "packages" / "aethyme" / "docs" / "guides" / "broker-performance.md"
guide_text = guide.read_text(encoding="utf-8")

def has_freshness_marker(text):
    return "Last Updated:" in text or "Last Reviewed:" in text

if not has_freshness_marker(guide_text):
    raise SystemExit(f"{guide}: missing Last Updated: or Last Reviewed:")

without_markers = "\n".join(
    line for line in guide_text.splitlines()
    if "Last Updated:" not in line and "Last Reviewed:" not in line
)
if has_freshness_marker(without_markers):
    raise SystemExit("documentation freshness negative control unexpectedly passed")
PY

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s "$benchmark_dir" -p 'test_broker_latency.py' -v

# Use a versioned upstream tool through Go's module verifier; do not install a
# mutable action tag or run the benchmark workflow's Cargo commands here.
if ! command -v go >/dev/null 2>&1; then
  printf 'Go is required to run pinned actionlint v1.7.11\n' >&2
  exit 1
fi
go version
GOWORK=off GOTOOLCHAIN=local go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.11 "$workflow"
