#!/usr/bin/env python3
"""
Compare benchmark results between baseline and current run.

Used in CI to detect performance regressions.
"""

import argparse
import json
import sys


def load_results(file_path: str) -> dict:
    """Load benchmark results from JSON file."""
    try:
        with open(file_path, 'r') as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def compare_indexing(baseline: dict, current: dict, threshold: float) -> list:
    """Compare indexing benchmark results."""
    regressions = []

    for curr_result in current.get("results", []):
        size = curr_result["repo_size"]

        # Find matching baseline
        baseline_result = next(
            (r for r in baseline.get("results", []) if r["repo_size"] == size),
            None
        )

        if not baseline_result:
            continue

        # Compare median duration
        baseline_median = baseline_result["median"]
        current_median = curr_result["median"]
        percent_change = ((current_median - baseline_median) / baseline_median) * 100

        if percent_change > threshold:
            regressions.append({
                "type": "indexing",
                "size": size,
                "baseline": baseline_median,
                "current": current_median,
                "percent_change": percent_change,
            })

    return regressions


def compare_queries(baseline: dict, current: dict, threshold: float) -> list:
    """Compare query benchmark results."""
    regressions = []

    for query_type in current.get("results", {}).keys():
        curr_result = current["results"][query_type]
        baseline_result = baseline.get("results", {}).get(query_type)

        if not baseline_result:
            continue

        # Compare p95 latency
        baseline_p95 = baseline_result["p95"]
        current_p95 = curr_result["p95"]
        percent_change = ((current_p95 - baseline_p95) / baseline_p95) * 100

        if percent_change > threshold:
            regressions.append({
                "type": "query",
                "query_type": query_type,
                "baseline": baseline_p95,
                "current": current_p95,
                "percent_change": percent_change,
            })

    return regressions


def compare_cache(baseline: dict, current: dict, threshold: float) -> list:
    """Compare cache benchmark results."""
    regressions = []

    baseline_hit_rate = baseline.get("results", {}).get("hit_rate", 0)
    current_hit_rate = current.get("results", {}).get("hit_rate", 0)

    # For hit rate, lower is worse
    percent_change = ((baseline_hit_rate - current_hit_rate) / baseline_hit_rate) * 100

    if percent_change > threshold:
        regressions.append({
            "type": "cache",
            "metric": "hit_rate",
            "baseline": baseline_hit_rate,
            "current": current_hit_rate,
            "percent_change": -percent_change,  # Negative for hit rate drop
        })

    return regressions


def generate_markdown(regressions: list, threshold: float) -> str:
    """Generate markdown report of comparison."""
    md = ["# Performance Benchmark Comparison\n"]

    if not regressions:
        md.append("✅ **No performance regressions detected!**\n")
        md.append(f"All metrics within {threshold}% of baseline.\n")
    else:
        md.append(f"⚠️ **REGRESSION: {len(regressions)} performance regression(s) detected**\n")
        md.append(f"Threshold: {threshold}%\n\n")

        # Group by type
        indexing_regs = [r for r in regressions if r["type"] == "indexing"]
        query_regs = [r for r in regressions if r["type"] == "query"]
        cache_regs = [r for r in regressions if r["type"] == "cache"]

        if indexing_regs:
            md.append("## Indexing Regressions\n\n")
            md.append("| Size | Baseline | Current | Change |\n")
            md.append("|------|----------|---------|--------|\n")
            for reg in indexing_regs:
                md.append(
                    f"| {reg['size']} | {reg['baseline']:.2f}s | "
                    f"{reg['current']:.2f}s | +{reg['percent_change']:.1f}% |\n"
                )
            md.append("\n")

        if query_regs:
            md.append("## Query Regressions\n\n")
            md.append("| Query Type | Baseline p95 | Current p95 | Change |\n")
            md.append("|------------|--------------|-------------|--------|\n")
            for reg in query_regs:
                md.append(
                    f"| {reg['query_type']} | {reg['baseline']:.3f}s | "
                    f"{reg['current']:.3f}s | +{reg['percent_change']:.1f}% |\n"
                )
            md.append("\n")

        if cache_regs:
            md.append("## Cache Regressions\n\n")
            md.append("| Metric | Baseline | Current | Change |\n")
            md.append("|--------|----------|---------|--------|\n")
            for reg in cache_regs:
                md.append(
                    f"| Hit Rate | {reg['baseline']:.1%} | "
                    f"{reg['current']:.1%} | {reg['percent_change']:.1f}% |\n"
                )
            md.append("\n")

    return "".join(md)


def main():
    parser = argparse.ArgumentParser(description="Compare benchmark results")
    parser.add_argument("--current-indexing", help="Current indexing results")
    parser.add_argument("--current-query", help="Current query results")
    parser.add_argument("--current-cache", help="Current cache results")
    parser.add_argument("--baseline-indexing", help="Baseline indexing results")
    parser.add_argument("--baseline-query", help="Baseline query results")
    parser.add_argument("--baseline-cache", help="Baseline cache results")
    parser.add_argument("--threshold", type=float, default=20,
                        help="Regression threshold percentage (default: 20)")
    parser.add_argument("--output", default="comparison.md",
                        help="Output markdown file")
    args = parser.parse_args()

    all_regressions = []

    # Compare indexing
    if args.current_indexing and args.baseline_indexing:
        current = load_results(args.current_indexing)
        baseline = load_results(args.baseline_indexing)
        if current and baseline:
            all_regressions.extend(
                compare_indexing(baseline, current, args.threshold)
            )

    # Compare queries
    if args.current_query and args.baseline_query:
        current = load_results(args.current_query)
        baseline = load_results(args.baseline_query)
        if current and baseline:
            all_regressions.extend(
                compare_queries(baseline, current, args.threshold)
            )

    # Compare cache
    if args.current_cache and args.baseline_cache:
        current = load_results(args.current_cache)
        baseline = load_results(args.baseline_cache)
        if current and baseline:
            all_regressions.extend(
                compare_cache(baseline, current, args.threshold)
            )

    # Generate report
    markdown = generate_markdown(all_regressions, args.threshold)

    # Save to file
    with open(args.output, 'w') as f:
        f.write(markdown)

    print(markdown)

    # Exit with error if regressions found
    if all_regressions:
        sys.exit(1)


if __name__ == "__main__":
    main()
