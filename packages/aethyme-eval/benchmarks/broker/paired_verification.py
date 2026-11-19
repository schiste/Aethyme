#!/usr/bin/env python3
"""Pair independent verification commands on identical Playground trees."""

import argparse
import hashlib
import json
import os
import shutil
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
    for slot in range(2):
        if arm == "broker":
            info = json.loads(run(repo, dict(env, AETHYME_AGENT_PID="none"), binary, "broker",
                                  "start", "--task", f"Shared unit suite sample {index} slot {slot}",
                                  "--short-name", f"gate-{index}-{slot}", "--json").stdout)
            checkout, session = pathlib.Path(info["worktree_path"]), info["id"]
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
        argv = (binary, "broker", "advanced", "gates", "run", "--session", str(session), "--json") if session else (
            *GATE_ARGV,)
        barrier.wait(timeout=10)
        start = time.monotonic_ns()
        result = run(checkout, env, *argv, check=False)
        elapsed = (time.monotonic_ns() - start) / 1e6
        rows = json.loads(result.stdout) if session and result.returncode == 0 else None
        return {"session": session, "tree": tree, "wall_ms": elapsed, "returncode": result.returncode,
                "results": rows, "stderr": result.stderr}
    with ThreadPoolExecutor(max_workers=2) as pool:
        return list(pool.map(gate, gate_inputs))


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
    report = {"binary_sha256": digest, "binary_version": run(repo, env, binary, "--version").stdout.strip(),
              "samples": []}
    # Reserve this receipt before running any commands that could populate the cache.
    with output.open("x") as stream:
        json.dump(report, stream, indent=2)
    for index in range(1, args.samples + 1):
        arms = ("baseline", "broker") if index % 2 else ("broker", "baseline")
        trial = {arm: round_results(index, arm, root, repo, env, git, binary) for arm in arms}
        report["samples"].append(trial)
        output.write_text(json.dumps(report, indent=2) + "\n")
        trees = {row["tree"] for rows in trial.values() for row in rows}
        if len(trees) != 1:
            raise ValueError("verification arms did not operate on the same tree")
        if any(row["returncode"] for rows in trial.values() for row in rows):
            raise RuntimeError(f"verification command failed in sample {index}; inspect {output}")
        for row in trial["broker"]:
            results = row["results"]
            if len(results) != 1 or results[0]["tree_hash"] != row["tree"] or results[0]["status"] != "pass":
                raise ValueError("expected one passing verdict for the measured tree")
    print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
