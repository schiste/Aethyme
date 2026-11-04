#!/usr/bin/env python3
"""Summarize one sharded Rust workspace test run.

Inputs are the artifacts the OSS CI test jobs leave behind:

* ``--junit-dir``: one nextest JUnit report per shard, at
  ``<dir>/<shard name>/junit.xml``;
* ``--build-json``: the build job's facts (archive size, binary count);
* ``--jobs-json`` (optional): ``GET /repos/{repo}/actions/runs/{id}/jobs``.

Outputs a Markdown summary for ``$GITHUB_STEP_SUMMARY`` and a JSON document
(schema ``aethyme.rust-test-telemetry/v1``) uploaded as an artifact, so a
regression in compile time, shard balance or a single slow test is visible
on every run instead of being reconstructed from raw logs.

Missing job timings are reported as unavailable, never as zero. A missing or
unreadable shard report is an error: the verdict job has already required
every shard to pass, so an absent report means the telemetry is wrong.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SCHEMA = "aethyme.rust-test-telemetry/v1"
SLOWEST = 15


def parse_time(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def seconds(start: str | None, end: str | None) -> float | None:
    a, b = parse_time(start), parse_time(end)
    if a is None or b is None:
        return None
    return round((b - a).total_seconds(), 1)


def read_shard(path: Path) -> dict:
    root = ET.parse(path).getroot()
    tests = []
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                status = "failed"
            elif case.find("skipped") is not None:
                status = "skipped"
            elif case.find("flakyFailure") is not None or case.find("flakyError") is not None:
                status = "flaky"
            else:
                status = "passed"
            tests.append(
                {
                    "binary": case.get("classname") or suite.get("name") or "",
                    "name": case.get("name") or "",
                    "seconds": float(case.get("time") or 0.0),
                    "status": status,
                }
            )
    return {
        "shard": path.parent.name,
        "wall_seconds": round(float(root.get("time") or 0.0), 1),
        "tests": tests,
    }


def job_phases(jobs: list[dict]) -> list[dict]:
    """Per-job queue, total and step durations for the Rust test jobs."""
    phases = []
    for job in jobs:
        name = job.get("name", "")
        if not name.startswith("Rust workspace tests") and name != "Broker performance controls":
            continue
        steps = [
            {"name": step.get("name"), "seconds": seconds(step.get("started_at"), step.get("completed_at"))}
            for step in job.get("steps", [])
            if step.get("conclusion") not in (None, "skipped")
        ]
        phases.append(
            {
                "job": name,
                "conclusion": job.get("conclusion"),
                "queued_seconds": seconds(job.get("created_at"), job.get("started_at")),
                "seconds": seconds(job.get("started_at"), job.get("completed_at")),
                "started_at": job.get("started_at"),
                "completed_at": job.get("completed_at"),
                "steps": steps,
            }
        )
    return phases


def summarize(shards: list[dict], build: dict, jobs: list[dict] | None) -> dict:
    tests = [dict(t, shard=s["shard"]) for s in shards for t in s["tests"]]
    walls = [s["wall_seconds"] for s in shards]
    phases = job_phases(jobs) if jobs is not None else None
    critical = None
    if phases:
        starts = [parse_time(p["started_at"]) for p in phases if p["started_at"]]
        ends = [parse_time(p["completed_at"]) for p in phases if p["completed_at"]]
        if starts and ends:
            critical = round((max(ends) - min(starts)).total_seconds(), 1)
    return {
        "schema": SCHEMA,
        "build": build,
        "tests": {
            "total": len(tests),
            "passed": sum(t["status"] == "passed" for t in tests),
            "flaky": sum(t["status"] == "flaky" for t in tests),
            "failed": sum(t["status"] == "failed" for t in tests),
            "skipped": sum(t["status"] == "skipped" for t in tests),
            "test_seconds": round(sum(t["seconds"] for t in tests), 1),
        },
        "shards": [
            {
                "shard": s["shard"],
                "tests": len(s["tests"]),
                "wall_seconds": s["wall_seconds"],
                "test_seconds": round(sum(t["seconds"] for t in s["tests"]), 1),
            }
            for s in shards
        ],
        "shard_balance": round(max(walls) / min(walls), 2) if walls and min(walls) > 0 else None,
        "slowest": sorted(tests, key=lambda t: -t["seconds"])[:SLOWEST],
        "flaky": [t for t in tests if t["status"] == "flaky"],
        "failed": [t for t in tests if t["status"] == "failed"],
        "jobs": phases,
        "jobs_first_start_to_last_end_seconds": critical,
    }


def fmt(value: float | int | None, unit: str = "s") -> str:
    if value is None:
        return "unavailable"
    if unit == "s":
        minutes, secs = divmod(float(value), 60)
        return f"{int(minutes)}m{secs:04.1f}s" if minutes else f"{secs:.1f}s"
    return f"{value}{unit}"


def markdown(summary: dict) -> str:
    t = summary["tests"]
    b = summary["build"]
    lines = ["## Rust workspace tests", ""]
    lines.append(
        f"**{t['total']} tests** ({t['passed']} passed, {t['flaky']} flaky, "
        f"{t['failed']} failed, {t['skipped']} skipped) across **{len(summary['shards'])} shards**; "
        f"{fmt(t['test_seconds'])} of test time; shard balance (slowest/fastest) "
        f"{summary['shard_balance'] if summary['shard_balance'] is not None else 'unavailable'}."
    )
    lines.append(
        f"Build: {b.get('test_binaries', 'unavailable')} test binaries, archive "
        f"{fmt(round(b['archive_bytes'] / 1048576, 1), ' MiB') if b.get('archive_bytes') else 'unavailable'}. "
        f"Rust test jobs, first start to last end: {fmt(summary['jobs_first_start_to_last_end_seconds'])}."
    )
    lines += ["", "### Jobs", ""]
    if summary["jobs"] is None:
        lines.append("Job timings unavailable (the Actions API could not be read).")
    else:
        lines += ["| Job | Queued | Total | Longest steps |", "|---|---|---|---|"]
        for job in summary["jobs"]:
            longest = sorted(
                (s for s in job["steps"] if s["seconds"] is not None), key=lambda s: -s["seconds"]
            )[:3]
            steps = "; ".join(f"{s['name']} {fmt(s['seconds'])}" for s in longest)
            lines.append(f"| {job['job']} | {fmt(job['queued_seconds'])} | {fmt(job['seconds'])} | {steps} |")
    lines += ["", "### Shards", "", "| Shard | Tests | Wall | Test time |", "|---|---|---|---|"]
    for s in summary["shards"]:
        lines.append(f"| {s['shard']} | {s['tests']} | {fmt(s['wall_seconds'])} | {fmt(s['test_seconds'])} |")
    lines += ["", f"### Slowest {SLOWEST} tests", "", "| Time | Shard | Test |", "|---|---|---|"]
    for test in summary["slowest"]:
        lines.append(f"| {fmt(test['seconds'])} | {test['shard']} | `{test['binary']} {test['name']}` |")
    for title, key in (("Flaky tests (passed on retry)", "flaky"), ("Failed tests", "failed")):
        if summary[key]:
            lines += ["", f"### {title}", ""]
            lines += [f"- `{t['binary']} {t['name']}` ({t['shard']})" for t in summary[key]]
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--junit-dir", type=Path, required=True)
    parser.add_argument("--build-json", type=Path, required=True)
    parser.add_argument("--jobs-json", type=Path)
    parser.add_argument("--markdown", type=Path, required=True)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args(argv)

    reports = sorted(args.junit_dir.glob("*/junit.xml"))
    if not reports:
        print(f"error: no shard reports under {args.junit_dir}", file=sys.stderr)
        return 1
    shards = [read_shard(path) for path in reports]
    build = json.loads(args.build_json.read_text())
    jobs = None
    if args.jobs_json is not None and args.jobs_json.is_file():
        jobs = json.loads(args.jobs_json.read_text()).get("jobs")

    summary = summarize(shards, build, jobs)
    args.json.write_text(json.dumps(summary, indent=2) + "\n")
    args.markdown.write_text(markdown(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
