#!/usr/bin/env python3
"""Pair independent verification commands on identical Playground trees."""

import argparse
import hashlib
import json
import math
import os
import shutil
import statistics
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from paired import prepare, rep_environment, run

GATE_ARGV = ("python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q")
GATE_POLICY = (
    '[[gate]]\nname = "unit"\ncommand = "python3 -m unittest discover -s tests -t . -q"\n'
    'cache = true\ntimeout_seconds = 20\n'
)


def stage(root, env, git, binary):
    root.mkdir(mode=0o700)
    repo = prepare(root, env, git)
    ignore = repo / ".gitignore"
    ignore.write_text(ignore.read_text() + "!.aethyme/gates.toml\n")
    (repo / ".aethyme/gates.toml").write_text(GATE_POLICY)
    run(repo, env, git, "add", "--", ".gitignore", ".aethyme/gates.toml")
    run(repo, env, git, "commit", "-qm", "Declare the Playground unit test gate")
    run(repo, env, git, "push", "-q", "origin", "main")
    print(run(repo, env, binary, "broker", "advanced", "trust", "status", "--json").stdout)


def round_results(index, arm, root, repo, env, git, binary):
    gate_inputs = []
    worktree_root = (root / "worktrees").resolve()
    for slot in range(2):
        if arm == "broker":
            info = json.loads(run(repo, dict(env, AETHYME_AGENT_PID="none"), binary, "broker",
                                  "start", "--task", f"Shared unit suite sample {index} slot {slot}",
                                  "--short-name", f"gate-{index}-{slot}", "--json").stdout)
            checkout = Path(info["worktree_path"]).resolve()
            if not checkout.is_relative_to(worktree_root):
                raise RuntimeError("Broker worktree escaped experiment root")
            session = info["id"]
        else:
            checkout = root / "worktrees" / f"git-{index}-{slot}"
            run(repo, env, git, "worktree", "add", "--detach", str(checkout), "origin/main")
            session = None
        with (checkout / "README.md").open("a") as stream:
            stream.write(f"\nVerification sample {index}.\n")
        run(checkout, env, git, "add", "--", "README.md")
        run(checkout, env, git, "commit", "-qm", "Verification sample")
        tree = run(checkout, env, git, "rev-parse", "HEAD^{tree}").stdout.strip()
        gate_inputs.append((checkout, session, tree))
    barrier = threading.Barrier(2)

    def gate(spec):
        checkout, session, tree = spec
        argv = GATE_ARGV
        if session is not None:
            argv = (binary, "broker", "advanced", "gates", "run", "--session", str(session), "--json")
        barrier.wait(timeout=10)
        start = time.monotonic_ns()
        result = run(checkout, env, *argv, check=False)
        elapsed = (time.monotonic_ns() - start) / 1e6
        rows = json.loads(result.stdout) if session and result.returncode == 0 else None
        return {"session": session, "tree": tree, "wall_ms": elapsed, "returncode": result.returncode,
                "results": rows, "stderr": result.stderr}
    with ThreadPoolExecutor(max_workers=2) as pool:
        return list(pool.map(gate, gate_inputs))



def verify_sample(trial, previous_trees, previous_runs):
    trees = {row["tree"] for rows in trial.values() for row in rows}
    if len(trees) != 1 or trees & previous_trees:
        raise ValueError("expected a new tree shared by both verification arms")
    if any(row["returncode"] for rows in trial.values() for row in rows):
        raise RuntimeError("verification command failed")
    verdicts = []
    for row in trial["broker"]:
        results = row["results"]
        if len(results) != 1 or results[0]["tree_hash"] != row["tree"] or results[0]["status"] != "pass":
            raise ValueError("expected one passing verdict for the measured tree")
        verdict = results[0]
        if not verdict.get("run_id") or verdict["run_id"] in previous_runs:
            raise ValueError("expected a gate run produced in this sample")
        verdicts.append(verdict)
    fresh = {row["run_id"]: row for row in verdicts if not row["cached"]}
    for row in verdicts:
        producer = fresh.get(row["run_id"])
        if producer is None or (row["cached"] and (
            not row.get("cache_provenance") or row["cache_provenance"] != producer.get("cache_provenance")
        )):
            raise ValueError("reused verdict lacks a matching producer in this sample")
    if len(fresh) != sum(not row["cached"] for row in verdicts):
        raise ValueError("independent executions have the same run ID")
    previous_trees.update(trees)
    previous_runs.update(fresh)
    return verdicts


def summarize(report, verdicts):
    summary = {}
    for arm in ("baseline", "broker"):
        pair_wall = sorted(max(row["wall_ms"] for row in trial[arm]) for trial in report["samples"])
        summary[arm] = {
            "pair_median_ms": statistics.median(pair_wall),
            "pair_p90_ms": pair_wall[math.ceil(0.9 * len(pair_wall)) - 1],
        }
    cached = [row for row in verdicts if row["cached"]]
    summary["broker"].update(
        executions=len(verdicts) - len(cached),
        executions_avoided=len(cached),
        waited_for_shared_verdict=sum((row["wait_duration_ms"] or 0) > 0 for row in cached),
        reused_execution_ms=sum(row["duration_ms"] for row in cached),
    )
    summary["baseline"]["executions"] = len(verdicts)
    return summary


def write_summary(path, report):
    summary = report["summary"]
    shared = summary["broker"]
    with Path(path).open("a") as stream:
        stream.write("\n### Paired Playground shared verification\n\n")
        stream.write(f'`{report["binary_version"]}`, {len(report["samples"])} matched pairs. '
                     "Each pair's wall time is the slower of its two requests.\n\n")
        stream.write("| Workflow | Median pair | p90 pair | Gate executions |\n"
                     "| --- | ---: | ---: | ---: |\n")
        for arm, label in (("baseline", "Direct test command"), ("broker", "Aethyme broker")):
            row = summary[arm]
            stream.write(f'| {label} | {row["pair_median_ms"]:.1f} ms | '
                         f'{row["pair_p90_ms"]:.1f} ms | {row["executions"]} |\n')
        stream.write(f'\nShared results avoided {shared["executions_avoided"]} executions. '
                     f'{shared["waited_for_shared_verdict"]} requests waited for the shared verdict. '
                     f'The reused executions recorded {shared["reused_execution_ms"]} ms '
                     "of child execution time. Raw operation timings are in the artifact.\n")

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("stage", "run"))
    parser.add_argument("--binary", type=lambda value: Path(value).resolve(strict=True), required=True)
    parser.add_argument("--root", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--samples", type=int, default=6, choices=range(1, 13), metavar="1..12")
    args = parser.parse_args()
    root, binary = args.root, args.binary
    git = shutil.which("git")
    env = dict(rep_environment(root), AETHYME_UPDATE_CHECK="off", AETHYME_MEASURE_OUTPUT="1")
    if args.action == "stage":
        stage(root, env, git, binary)
        return 0

    repo = root / "repository"
    env["PATH"] = str(root / "bin") + os.pathsep + env["PATH"]
    if run(repo, env, git, "remote", "get-url", "origin").stdout.strip() != str(root / "origin.git"):
        raise ValueError("expected the staged Playground's local bare origin")
    policy = run(repo, env, git, "show", "HEAD:.aethyme/gates.toml").stdout
    if policy != GATE_POLICY:
        raise ValueError("expected the staged Playground gate policy")
    trust = json.loads(run(repo, env, binary, "broker", "advanced", "trust", "status", "--json").stdout)
    if not trust["trusted"]:
        raise ValueError("operator must approve the exact Playground gate before measuring")
    output = root / "results.json"
    with binary.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    report = {"schema_version": 1, "binary_sha256": digest, "binary_version": run(repo, env, binary, "--version").stdout.strip(),
              "samples": []}
    # Reserve this receipt before running any commands that could populate the cache.
    with output.open("x") as stream:
        json.dump(report, stream, indent=2)
    previous_trees, previous_runs, verdicts = set(), set(), []
    for index in range(1, args.samples + 1):
        arms = ("baseline", "broker") if index % 2 else ("broker", "baseline")
        trial = {arm: round_results(index, arm, root, repo, env, git, binary) for arm in arms}
        report["samples"].append(trial)
        output.write_text(json.dumps(report, indent=2) + "\n")
        try:
            verdicts.extend(verify_sample(trial, previous_trees, previous_runs))
        except (RuntimeError, ValueError) as error:
            raise RuntimeError(f"invalid sample {index}: {error}; inspect {output}") from error
    report["summary"] = summarize(report, verdicts)
    output.write_text(json.dumps(report, indent=2) + "\n")
    if github_summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        write_summary(github_summary, report)
    print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
