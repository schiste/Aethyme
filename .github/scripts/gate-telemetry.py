#!/usr/bin/env python3
"""Summarize the gates one Aethyme Gates run ran in its gates job.

``--gate-log`` is the output of the "Run all gates" step. The cargo-test gate
runs in shard jobs, and rust-test-telemetry.py reports its detail.

Outputs a Markdown summary for ``$GITHUB_STEP_SUMMARY`` and a JSON document
(schema ``aethyme.gate-telemetry/v2``). A missing log is reported as not
recorded, never as zero, and never fails the job: the gates decide the verdict.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

SCHEMA = "aethyme.gate-telemetry/v2"
GATE_RESULT = re.compile(r"^gate (\S+) (pass|fail|error|cached)\w* in (\d+)s\b")
GATE_COVERED = re.compile(r"^gate (\S+): covered by pull-request run (\d+)")
GATE_SHARDED = re.compile(r"^gate (\S+): runs in this workflow's shard jobs")
GATE_REFUSED = re.compile(r"^gate (\S+) could not start: (.*)")


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
        elif match := GATE_SHARDED.match(line):
            gates[match[1]] = {"gate": match[1], "result": "shard jobs", "seconds": None}
        elif match := GATE_REFUSED.match(line):
            gates[match[1]] = {
                "gate": match[1],
                "result": "refused",
                "seconds": None,
                "detail": match[2][:200],
            }
    return list(gates.values())


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
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--gate-log", type=Path)
    parser.add_argument("--markdown", type=Path, required=True)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args(argv)

    summary = {"schema": SCHEMA, "gates": read_gates(args.gate_log)}
    args.json.write_text(json.dumps(summary, indent=2) + "\n")
    args.markdown.write_text(markdown(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
