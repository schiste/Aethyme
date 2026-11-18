#!/usr/bin/env python3
"""Measure writable broker opens in disposable copies of the broker Playground."""

import argparse
import collections
import json
import math
import os
import random
import shutil
import statistics
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from paired import prepare, rep_environment, run


def metrics(repo, command):
    path = repo / ".aethyme/logs/command-metrics.jsonl"
    return [row for line in path.read_text().splitlines() if (row := json.loads(line))["command"] == command]


def distribution(values):
    ordered = sorted(values)
    return {"median": statistics.median(ordered), "p90": ordered[math.ceil(0.9 * len(ordered)) - 1]}


def run_trial(binary, git, root, concurrent, recent_sweep):
    # The broker recognizes repositories under its temp directory as
    # ephemeral, so detached workers cannot outlive the disposable trial.
    env = dict(rep_environment(root), TMPDIR=str(root), AETHYME_UPDATE_CHECK="off",
               AETHYME_MEASURE_OUTPUT="1", AETHYME_AGENT_PID=str(os.getpid()))
    repo = prepare(root, env, git)
    if recent_sweep:
        (repo / ".aethyme/broker.toml").write_text("[retention]\nartifact_reclaim_days = 0\n")
        git_info = repo / ".git/info"
        git_info.mkdir(exist_ok=True)
        with (git_info / "exclude").open("a") as ignores:
            ignores.write("\n/target/\n")
    run(repo, env, git, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main")

    def invoke(cwd, command, *args):
        began = time.monotonic()
        proc = run(cwd, env, binary, "broker", command, *args, "--json")
        return json.loads(proc.stdout), (time.monotonic() - began) * 1000

    def start(name, path=None):
        scope = ("--path", path) if path else ()
        info, elapsed = invoke(repo, "start", "--task", name, "--short-name", name, *scope)
        checkout = Path(info["worktree_path"]).resolve()
        if not checkout.is_relative_to(root / "worktrees"):
            raise RuntimeError(f"Broker worktree escaped Playground: {checkout}")
        return info, checkout, elapsed

    def commit(checkout, path):
        with (checkout / path).open("a") as source:
            source.write("\n# Writable open profile fixture.\n")
        run(checkout, env, git, "add", "--", path)
        run(checkout, env, git, "commit", "-qm", "Change fixture")

    def publish(info, checkout):
        result, elapsed = invoke(checkout, "advance", "--session", str(info["id"]))
        head = run(checkout, env, git, "rev-parse", "HEAD").stdout.strip()
        remote_head = run(checkout, env, git, "ls-remote", "--refs", "origin",
                          f"refs/heads/{info['branch']}").stdout.split()
        if result["state"] != "published" or not remote_head or result["pushed_head"] != head or remote_head[0] != head:
            raise RuntimeError(f"Publication did not preserve session {info['id']}'s exact commit")
        return elapsed

    def finish(info, checkout, *options):
        result, _ = invoke(checkout, "finish", "--session", str(info["id"]), *options)
        if not result["closed"]:
            raise RuntimeError(f"Session {info['id']} did not close")

    def integrate_and_finish(info, checkout):
        run(repo, env, git, "merge", "--no-ff", "-qm", "Integrate profile fixture", info["branch"])
        run(repo, env, git, "push", "-q", "origin", "main")
        finish(info, checkout)

    seed, checkout, elapsed = start("seed")
    rows = {"cold_start": [(elapsed, metrics(repo, "start")[-1])]}
    if recent_sweep:
        finish(seed, checkout, "--keep-worktree")
        cache = checkout / "target"
        cache.mkdir()
        (cache / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172\n")
        (cache / "fixture.txt").write_text("Regenerable Playground output\n")
        invoke(repo, "gc", "sweep")
        if cache.exists():
            raise RuntimeError("The seed sweep did not reclaim its fixture cache")
    else:
        finish(seed, checkout)
    first, checkout, elapsed = start("work", "shoplib/money.py")
    rows["steady_start"] = [(elapsed, metrics(repo, "start")[-1])]
    commit(checkout, "shoplib/money.py")
    if concurrent:
        second, second_checkout, _ = start("peer", "shoplib/inventory.py")
        commit(second_checkout, "shoplib/inventory.py")
        with ThreadPoolExecutor(max_workers=2) as pool:
            one = pool.submit(publish, first, checkout)
            two = pool.submit(publish, second, second_checkout)
            walls = {first["id"]: one.result(), second["id"]: two.result()}
        records = {row["session_id"]: row for row in metrics(repo, "advance")}
        if records.keys() != walls.keys():
            raise RuntimeError("Missing concurrent advance measurements")
        rows["concurrent_advance"] = [(wall, records[session]) for session, wall in walls.items()]
        integrate_and_finish(second, second_checkout)
    else:
        elapsed = publish(first, checkout)
        rows["advance"] = [(elapsed, metrics(repo, "advance")[-1])]
    integrate_and_finish(first, checkout)
    return {step: {"wall_ms": [wall for wall, _ in samples],
                   "phases": [row.get("phase_timings_ms", {}) for _, row in samples]}
            for step, samples in rows.items()}


def summarize(trials):
    steps = collections.defaultdict(list)
    for trial in trials:
        for name, row in trial.items():
            steps[name].append(row)
    result = {}
    for name, rows in steps.items():
        walls = [wall for row in rows for wall in row["wall_ms"]]
        phases = [phase for row in rows for phase in row["phases"]]
        names = sorted({key for phase in phases for key in phase})
        result[name] = {
            "wall": distribution(walls),
            "samples": len(walls),
            "phases": {key: {**distribution([row.get(key, 0) for row in phases]),
                             "occurrences": sum(key in row for row in phases)} for key in names},
        }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=12)
    parser.add_argument("--arm", action="append", required=True, metavar="NAME=BINARY")
    parser.add_argument("--concurrent", action="store_true")
    parser.add_argument("--recent-sweep", action="store_true",
                        help="Generate a real completed cleanup record before warm measurements")
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    binaries = {name: str(Path(binary).resolve(strict=True))
                for name, binary in (entry.split("=", 1) for entry in args.arm)}
    git = shutil.which("git")
    if not git:
        parser.error("git must be installed")
    raw = collections.defaultdict(list)
    rng = random.Random(104)
    for index in range(args.repeats):
        order = list(binaries)
        rng.shuffle(order)
        for arm in order:
            with tempfile.TemporaryDirectory(prefix="open-", dir=args.scratch) as temporary:
                trial = run_trial(binaries[arm], git, Path(temporary).resolve(), args.concurrent, args.recent_sweep)
            raw[arm].append(trial)
            print(arm, index + 1, {step: [round(x) for x in row["wall_ms"]]
                                  for step, row in trial.items()}, flush=True)
    report = {"trials": args.repeats, "seed": 104, "concurrent": args.concurrent, "recent_sweep": args.recent_sweep,
              "summary": {arm: summarize(trials) for arm, trials in raw.items()}, "raw": raw}
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
