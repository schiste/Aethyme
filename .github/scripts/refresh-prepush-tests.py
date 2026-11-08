#!/usr/bin/env python3
"""Refresh the module times the pre-push selection budgets with.

Usage:
    gh run download <full Aethyme Gates run> --pattern 'rust-tests-junit-shard-*' --dir DIR
    python3 .github/scripts/refresh-prepush-tests.py --junit-dir DIR --run <run id>

Rewrites `modules`, `source.junit_run`, `source.suite_seconds` and
`budget_seconds` (15% of the suite) in
packages/aethyme/rust/.config/prepush-tests.json from nextest's JUnit reports.
The always-run lists are kept: they come from failure history, not from
timings, and change only by review. Use the shards of a full-suite run (one
that uploaded `full-suite`); a changed-scope run times only part of the suite.
"""

from __future__ import annotations

import argparse
import json
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict
from pathlib import Path

BUDGET_FRACTION = 0.15
DATA = Path(__file__).resolve().parents[2] / "packages/aethyme/rust/.config/prepush-tests.json"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--junit-dir", type=Path, required=True)
    parser.add_argument("--run", type=int, required=True, help="the Aethyme Gates run the reports came from")
    parser.add_argument("--data", type=Path, default=DATA)
    args = parser.parse_args()

    modules: dict[str, float] = defaultdict(float)
    tests = 0
    for report in sorted(args.junit_dir.rglob("*.xml")):
        for case in ET.parse(report).getroot().iter("testcase"):
            binary = case.get("classname") or ""
            name = case.get("name") or ""
            if not binary or not name:
                continue
            modules[f"{binary} {name.split('::')[0]}"] += float(case.get("time") or 0)
            tests += 1
    if tests == 0:
        print(f"no test cases under {args.junit_dir}", file=sys.stderr)
        return 1

    data = json.loads(args.data.read_text())
    total = sum(modules.values())
    data["modules"] = {key: round(seconds, 2) for key, seconds in sorted(modules.items())}
    data["source"]["junit_run"] = args.run
    data["source"]["suite_seconds"] = round(total, 1)
    data["budget_seconds"] = round(BUDGET_FRACTION * total)
    args.data.write_text(json.dumps(data, indent=1) + "\n")
    print(f"{tests} tests in {len(modules)} modules, {total:.0f} s; budget {data['budget_seconds']} s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
