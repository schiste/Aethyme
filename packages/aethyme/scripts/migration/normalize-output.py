#!/usr/bin/env python
"""Canonicalize CLI output for golden diffing (retirement plan Phase 0).

Reads stdout of one command invocation on stdin. If it parses as JSON,
volatile fields are scrubbed and the result is re-emitted with sorted
keys; otherwise text passes through with only the repo path replaced.
Every transformation is a *normalization*, never a semantic rewrite —
byte parity after normalization is the migration bar (decision #2).

Usage: normalize-output.py --repo <path-that-becomes-{REPO}>
"""

from __future__ import annotations

import argparse
import json
import re
import sys

# Keys whose VALUES are volatile across runs/machines but not meaningful
# to parity. Grow this list deliberately — every addition weakens the
# diff, so each entry needs a reason.
VOLATILE_KEYS = {
    "generated_at",       # wall-clock stamps
    "finished_at",
    "started_at",
    "timestamp",
    "scan_id",            # scorecard run identity
    "duration_ms",        # timing
    "total_duration_ms",
    "elapsed_ms",
    "execution_time_ms",
    "total_scan_time_ms",
    "binary_path",        # machine-local paths outside the repo
    "cache_path",
    "engine_version",     # varies with rebuilds during migration
    "run_id",             # per-run randomness
    "snapshot_key",       # content-addressed temp snapshot identity
}


def scrub(node: object) -> object:
    if isinstance(node, dict):
        return {
            key: "<volatile>" if key in VOLATILE_KEYS else scrub(value)
            for key, value in node.items()
        }
    if isinstance(node, list):
        return [scrub(item) for item in node]
    return node


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    args = parser.parse_args()

    raw = sys.stdin.read()
    # Repo paths appear both verbatim and canonicalized (/private on macOS).
    for prefix in (f"/private{args.repo}", args.repo):
        raw = raw.replace(prefix, "{REPO}")
    raw = re.sub(r'"scan_id":\s*"[^"]+"', '"scan_id": "<volatile>"', raw)
    raw = re.sub(r'"timestamp":\s*"[^"]+"', '"timestamp": "<volatile>"', raw)
    raw = re.sub(
        r'"execution_time_ms":\s*[-0-9.eE]+',
        '"execution_time_ms": "<volatile>"',
        raw,
    )
    raw = re.sub(
        r'"total_scan_time_ms":\s*[-0-9.eE]+',
        '"total_scan_time_ms": "<volatile>"',
        raw,
    )
    raw = re.sub(r"\*\*Scan ID:\*\* `[^`]+`", "**Scan ID:** `<volatile>`", raw)
    raw = re.sub(r"\*\*Scan Time:\*\* \d+ms", "**Scan Time:** <volatile>", raw)
    raw = re.sub(r"\*\*Timestamp:\*\* .+", "**Timestamp:** <volatile>", raw)
    raw = re.sub(
        r"(\| [a-z0-9-]+ \| \d+ \| )\d+(?:\.\d+)?( \|)",
        r"\1<volatile>\2",
        raw,
    )

    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        # Text output: strip trailing whitespace per line for stability.
        lines = [line.rstrip() for line in raw.splitlines()]
        sys.stdout.write("\n".join(lines) + "\n")
        return

    sys.stdout.write(json.dumps(scrub(payload), sort_keys=True, indent=2))
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
