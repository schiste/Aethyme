#!/usr/bin/env python3
"""Summarize one Aethyme Gates run, with the detail of its cargo-test gate.

Inputs, all optional because a gate that did not run leaves nothing behind:

* ``--gate-log``: the output of the "Run all gates" step;
* ``--phases``: tab-separated ``phase, seconds, exit status`` lines from the
  cargo-test gate (``AETHYME_GATE_PHASES`` in ``.aethyme/gates.toml``);
* ``--junit``: the gate's nextest JUnit report.

Outputs a Markdown summary for ``$GITHUB_STEP_SUMMARY`` and a JSON document
(schema ``aethyme.gate-telemetry/v1``). A missing input is reported as not
recorded, never as zero, and never fails the job: the gates decide the verdict.
"""

from __future__ import annotations

import argparse
import collections
import importlib.util
import json
import re
import sys
from pathlib import Path

SCHEMA = "aethyme.gate-telemetry/v1"
SLOWEST = 15
BINARIES = 10
GATE_RESULT = re.compile(r"^gate (\S+) (pass|fail|error|cached)\w* in (\d+)s\b")
GATE_COVERED = re.compile(r"^gate (\S+): covered by pull-request run (\d+)")
GATE_REFUSED = re.compile(r"^gate (\S+) could not start: (.*)")


def load_read_shard():
    """The JUnit reader the workspace test telemetry already defines."""
    path = Path(__file__).with_name("rust-test-telemetry.py")
    spec = importlib.util.spec_from_file_location("rust_test_telemetry", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module.read_shard


def read_gates(path: Path | None) -> list[dict] | None:
    if path is None or not path.is_file():
        return None
    gates = {}
    for line in path.read_text(errors="replace").splitlines():
        if match := GATE_RESULT.match(line):
            gates[match[1]] = {
                "gate": match[1],
                "result": match[2],
                "seconds": int(match[3]),
            }
        elif match := GATE_COVERED.match(line):
            gates[match[1]] = {
                "gate": match[1],
                "result": "covered",
                "seconds": None,
                "detail": f"pull-request run {match[2]}",
            }
        elif match := GATE_REFUSED.match(line):
            gates[match[1]] = {
                "gate": match[1],
                "result": "refused",
                "seconds": None,
                "detail": match[2][:200],
            }
    return list(gates.values())


def read_phases(path: Path | None) -> list[dict] | None:
    if path is None or not path.is_file():
        return None
    phases = []
    for line in path.read_text().splitlines():
        fields = line.split("\t")
        if len(fields) == 3 and fields[1].isdigit() and fields[2].isdigit():
            phases.append(
                {"phase": fields[0], "seconds": int(fields[1]), "status": int(fields[2])}
            )
    return phases


def read_suite(path: Path | None) -> dict | None:
    if path is None or not path.is_file():
        return None
    report = load_read_shard()(path)
    tests = report["tests"]
    test_seconds = round(sum(t["seconds"] for t in tests), 1)
    wall = report["wall_seconds"]
    binaries = collections.defaultdict(lambda: [0, 0.0])
    for test in tests:
        binaries[test["binary"]][0] += 1
        binaries[test["binary"]][1] += test["seconds"]
    return {
        "tests": len(tests),
        "passed": sum(t["status"] == "passed" for t in tests),
        "flaky": [t for t in tests if t["status"] == "flaky"],
        "failed": [t for t in tests if t["status"] == "failed"],
        "wall_seconds": wall,
        "test_seconds": test_seconds,
        "average_parallelism": round(test_seconds / wall, 2) if wall else None,
        "slowest": sorted(tests, key=lambda t: -t["seconds"])[:SLOWEST],
        "binaries": [
            {"binary": name, "tests": count, "test_seconds": round(seconds, 1)}
            for name, (count, seconds) in sorted(
                binaries.items(), key=lambda item: -item[1][1]
            )[:BINARIES]
        ],
    }


def fmt(seconds: float | None) -> str:
    if seconds is None:
        return "-"
    minutes, rest = divmod(float(seconds), 60)
    return f"{int(minutes)}m{rest:04.1f}s" if minutes else f"{rest:.1f}s"


def markdown(summary: dict) -> str:
    lines = ["## Aethyme Gates", ""]
    gates = summary["gates"]
    if gates is None:
        lines.append("Gate results were not recorded.")
    else:
        total = sum(g["seconds"] or 0 for g in gates)
        lines += ["| Gate | Result | Time | Share |", "|---|---|---|---|"]
        for gate in sorted(gates, key=lambda g: -(g["seconds"] or 0)):
            share = f"{gate['seconds'] * 100 / total:.1f}%" if gate["seconds"] and total else "-"
            lines.append(f"| {gate['gate']} | {gate['result']} | {fmt(gate['seconds'])} | {share} |")

    lines += ["", "### cargo-test phases", ""]
    phases = summary["phases"]
    if phases is None:
        lines.append("Not recorded: the cargo-test gate did not run here.")
    else:
        lines += ["| Phase | Time | Exit status |", "|---|---|---|"]
        lines += [f"| {p['phase']} | {fmt(p['seconds'])} | {p['status']} |" for p in phases]

    lines += ["", "### cargo-test suite", ""]
    suite = summary["suite"]
    if suite is None:
        lines.append("No JUnit report was recorded.")
    else:
        lines.append(
            f"**{suite['tests']} tests** ({suite['passed']} passed, {len(suite['flaky'])} flaky, "
            f"{len(suite['failed'])} failed): {fmt(suite['wall_seconds'])} of wall time, "
            f"{fmt(suite['test_seconds'])} of test time, average parallelism "
            f"{suite['average_parallelism']}."
        )
        lines += ["", "| Binary | Tests | Test time |", "|---|---|---|"]
        lines += [
            f"| `{b['binary']}` | {b['tests']} | {fmt(b['test_seconds'])} |"
            for b in suite["binaries"]
        ]
        lines += ["", "| Slowest tests | Time |", "|---|---|"]
        lines += [
            f"| `{t['binary']} {t['name']}` | {fmt(t['seconds'])} |" for t in suite["slowest"]
        ]
        for title, key in (("Flaky tests", "flaky"), ("Failed tests", "failed")):
            if suite[key]:
                lines += ["", f"### {title}", ""]
                lines += [f"- `{t['binary']} {t['name']}`" for t in suite[key]]
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--gate-log", type=Path)
    parser.add_argument("--phases", type=Path)
    parser.add_argument("--junit", type=Path)
    parser.add_argument("--markdown", type=Path, required=True)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args(argv)

    summary = {
        "schema": SCHEMA,
        "gates": read_gates(args.gate_log),
        "phases": read_phases(args.phases),
        "suite": read_suite(args.junit),
    }
    args.json.write_text(json.dumps(summary, indent=2) + "\n")
    args.markdown.write_text(markdown(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
