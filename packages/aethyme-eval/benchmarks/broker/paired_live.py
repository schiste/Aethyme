#!/usr/bin/env python3
"""Compare direct Git and Aethyme with live Codex workers on the broker Playground."""

import argparse
import hashlib
import json
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from paired import HERE, MEASURES, codex_attempt, load_tasks, prepare, rep_environment, run, validate


def dump(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def verify_suite(repo, env):
    return run(repo, env, sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q",
               check=False).returncode == 0


def check_hidden(repo, env, folder, test_name):
    runner = folder / "hidden.py"
    shutil.copyfile(HERE / "hidden_checks" / test_name, runner)
    return run(folder, dict(env, PYTHONPATH=str(repo)), sys.executable,
               "-m", "unittest", "-q", "hidden", check=False).returncode == 0


def assigned_paths(repo, env, git, task):
    scope = run(repo, env, git, "apply", "--numstat",
                str(HERE / "reference" / f"{task['id']}.patch")).stdout
    return {line.split("\t")[2] for line in scope.splitlines()}


def worker(args, repo, root, env, git, task, arm, folder, peer_titles):
    started = time.monotonic()
    owned = assigned_paths(repo, env, git, task)
    session = None
    checkout = root / "worktrees" / task["id"]
    branch = f"agent/{task['id']}"
    if arm == "broker":
        # The coordinator is not the worker. Avoid binding an enclosing
        # Codex process before the sandboxed worker has started.
        paths = [argument for path in sorted(owned) for argument in ("--path", path)]
        result = run(repo, dict(env, AETHYME_AGENT_PID="none"), args.binary,
                     "broker", "start", "--task", task["title"], "--short-name", task["id"],
                     "--agent", "Worker <worker@example.invalid>", *paths, "--json")
        info = json.loads(result.stdout)
        checkout = Path(info["worktree_path"]).resolve()
        if not checkout.is_relative_to(root / "worktrees"):
            raise RuntimeError("Broker worktree escaped experiment root")
        session, branch = info["id"], info["branch"]
        dump(folder / "start.json", info)
        lifecycle = f"{shlex.quote(args.binary)} broker"
        inspect = f"{lifecycle} status --session {session} --json"
        publish = f"{lifecycle} advance --session {session} --json"
    else:
        run(repo, env, git, "worktree", "add", "-qb", branch, str(checkout), "origin/main")
        inspect = "git status --short --branch"
        if peer_titles:
            inspect += " and git worktree list"
        publish = f"git push origin HEAD:refs/heads/{branch}"
    peer_context = f"Concurrent tasks: {', '.join(peer_titles)}. " if peer_titles else ""
    prompt = (
        f"Implement {task['title']}.\n{task['spec']}\nAcceptance: {task['acceptance']}\n"
        f"{peer_context}Work only in the assigned isolated Git worktree on its existing branch. "
        f"Only edit these owned paths: {', '.join(sorted(owned))}. "
        f"Inspect coordination state using {inspect}. Use its relevant signals to avoid rework. "
        "Do not change another worktree or branch, force-push or write to main. "
        "Run python3 -m unittest discover -s tests -t . -q. Stage only explicit owned paths and commit. "
        f"Publish the committed branch with {publish}. Stop after a clean successful publication. "
        "The origin is a disposable local experiment. Do not search outside your worktree."
    )
    return {"task": task, "session": session, "branch": branch, "checkout": checkout, "owned": owned,
            "base": run(checkout, env, git, "rev-parse", "HEAD").stdout.strip(), "prompt": prompt,
            "started": started, "folder": folder}


def verify_branch(repo, env, git, work, base):
    paths = set(run(work["checkout"], env, git, "diff", "--name-only", base, "HEAD").stdout.splitlines())
    result = validate(repo, work["checkout"], env, git, work["task"], work["branch"])
    result["owned_paths_only"] = bool(paths) and paths <= work["owned"]
    result["valid"] = all(result[k] for k in ("hidden", "suite", "published_exact_head", "clean",
                                              "owned_paths_only"))
    return result


def integrate(args, repo, root, env, git, work, arm, first_attempt):
    task, folder, branch, checkout = (work[k] for k in ("task", "folder", "branch", "checkout"))
    attempts = [first_attempt]
    initial = verify_branch(repo, env, git, work, work["base"])
    conflict = None
    completed = False
    repair_used = False
    finish = None
    if initial["valid"]:
        upstream = run(repo, env, git, "rev-parse", "HEAD").stdout.strip()
        run(repo, env, git, "fetch", "-q", "origin", branch)
        merge = run(repo, env, git, "merge", "--no-ff", "--no-commit", "FETCH_HEAD", check=False)
        if merge.returncode:
            dirty = run(repo, env, git, "diff", "--name-only", "--diff-filter=U").stdout
            if not dirty:
                raise RuntimeError(merge.stderr.strip() or "merge failed without conflict evidence")
            conflict = "textual"
        elif not verify_suite(repo, env) or not check_hidden(repo, env, folder, task["hidden_check"]):
            conflict = "validation"
        if conflict:
            run(repo, env, git, "merge", "--abort")
            opened = run(checkout, env, git, "merge", "--no-ff", "--no-commit", upstream, check=False)
            if conflict == "textual" and opened.returncode == 0:
                raise RuntimeError("textual conflict did not reproduce in worker worktree")
            if conflict == "validation" and opened.returncode:
                raise RuntimeError("validation conflict could not be replayed in worker worktree")
            repair_used = True
            attempts.append(codex_attempt(
                args, folder / "repair.jsonl", root, checkout, env,
                work["prompt"] + "\nYour change is committed. The coordinator tried to integrate it "
                "and reproduced a conflict. The peer merge is open in your worktree. Retain both "
                "features and fix the conflict within your owned paths, then run the test suite, "
                "commit the merge and publish. Do not start another merge."
            ))
            repaired = verify_branch(repo, env, git, work, upstream)
            dump(folder / "repair-validation.json", repaired)
            if repaired["valid"]:
                run(repo, env, git, "fetch", "-q", "origin", branch)
                completed = run(repo, env, git, "merge", "--ff-only", "FETCH_HEAD", check=False).returncode == 0
        else:
            run(repo, env, git, "commit", "-qm", f"Integrate {task['id']}")
            completed = True
        if completed:
            run(repo, env, git, "push", "-q", "origin", "main")
            if arm == "broker":
                result = run(checkout, env, args.binary, "broker", "finish",
                             "--session", str(work["session"]), "--json", check=False)
                finish = result.returncode == 0 and json.loads(result.stdout)["closed"] is True
                (folder / "finish.json").write_text(result.stdout)
                (folder / "finish.stderr").write_text(result.stderr)
    elapsed = time.monotonic() - work["started"]
    usage = {key: [a["usage"][key] for a in attempts] for key in MEASURES[1:]}
    return {"task": task["id"], "arm": arm, "wall_seconds": elapsed, "initial": initial,
            **{key: sum(values) if None not in values else None for key, values in usage.items()},
            "attempts": attempts, "merged": completed, "session_finished": finish, "conflict": conflict,
            "rework_required": repair_used or not completed, "human_interventions": 0}


def run_group(args, folder, tasks, overlap, arm):
    git = shutil.which("git")
    with tempfile.TemporaryDirectory(prefix=f"paired-{arm}-", dir=args.scratch) as temp:
        root = Path(temp).resolve()
        env = dict(rep_environment(root), AETHYME_UPDATE_CHECK="off", AETHYME_MEASURE_OUTPUT="1")
        repo = prepare(root, env, git)
        workers = []
        for task in tasks:
            task_folder = folder / task["id"]
            task_folder.mkdir()
            peers = [peer["title"] for peer in tasks if peer is not task]
            workers.append(worker(args, repo, root, env, git, task, arm, task_folder, peers))
        group_started = time.monotonic()
        with ThreadPoolExecutor(max_workers=len(workers)) as pool:
            attempts = [pool.submit(codex_attempt, args, work["folder"] / "attempt.jsonl",
                                    root, work["checkout"], env, work["prompt"]) for work in workers]
            rows = [integrate(args, repo, root, env, git, work, arm, attempt.result())
                    for work, attempt in zip(workers, attempts, strict=True)]
        group_seconds = time.monotonic() - group_started
        final_suite = verify_suite(repo, env)
        for row, work in zip(rows, workers, strict=True):
            row["final_hidden"] = check_hidden(repo, env, work["folder"], work["task"]["hidden_check"])
            row["final_suite"] = final_suite
            row["completed"] = (row["merged"] and row["final_hidden"] and final_suite
                                and row["session_finished"] is not False)
            row["overlap"] = overlap
            row["group_seconds"] = group_seconds
        if overlap and all(row["completed"] for row in rows):
            overlap_result = check_hidden(repo, env, folder, args.overlaps[overlap]["hidden_check"])
            dump(folder / "overlap-validation.json", {"passed": overlap_result})
            if not overlap_result:
                for row in rows:
                    row["completed"] = False
                    row["overlap_hidden_failure"] = True
        for row, work in zip(rows, workers, strict=True):
            dump(work["folder"] / "result.json", row)
            print(json.dumps(row), flush=True)
        metrics = repo / ".aethyme/logs/command-metrics.jsonl"
        if metrics.is_file():
            shutil.copyfile(metrics, folder / "broker-metrics.jsonl")
        return rows


def summary(rows):
    def measure(values):
        values = sorted(v for v in values if v is not None)
        if not values:
            return {"n": 0}
        return {"n": len(values), "median": statistics.median(values),
                "p90": values[(9 * len(values) - 1) // 10]}
    result = {}
    for label, matches in (("all", lambda r: True), ("solo", lambda r: r["overlap"] is None),
                           ("concurrent", lambda r: r["overlap"] is not None)):
        selected = [row for row in rows if matches(row)]
        matched = {r["task"] for r in selected if r["completed"]
                   and any(p["task"] == r["task"] and p["arm"] != r["arm"] and p["completed"]
                           for p in selected)}
        contrasts = {}
        for arm in ("baseline", "broker"):
            own = [r for r in selected if r["arm"] == arm]
            contrasts[arm] = {
                "trials": len(own), "completed": sum(r["completed"] for r in own),
                "initially_valid": sum(r["initial"]["valid"] for r in own),
                "conflicts": sum(bool(r["conflict"]) for r in own),
                "rework": sum(r["rework_required"] for r in own),
                "human_interventions": sum(r["human_interventions"] for r in own),
                **{key: measure([r[key] for r in own if r["task"] in matched]) for key in MEASURES},
            }
        result[label] = contrasts
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=lambda arg: str(Path(arg).resolve(strict=True)))
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--model", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--seed", type=int, default=19)
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--only", nargs="+")
    args = parser.parse_args()
    manifest = load_tasks()
    tasks = {task["id"]: task for task in manifest["tasks"]}
    args.overlaps = {group["id"]: group for group in manifest["overlaps"]}
    plan = {task_id: ([task_id], None) for task_id in ("t02", "t06", "t07", "t10")}
    plan.update({name: (group["tasks"], name) for name, group in args.overlaps.items()})
    names = args.only or list(plan)
    if not names or len(set(names)) != len(names) or set(names) - plan.keys():
        parser.error("choose unique group IDs from " + ", ".join(plan))
    randomizer = random.Random(args.seed)
    order = ([("baseline", "broker"), ("broker", "baseline")] * ((len(names) + 1) // 2))[:len(names)]
    randomizer.shuffle(names)
    randomizer.shuffle(order)
    args.output.mkdir(parents=True, exist_ok=False)
    with open(args.binary, "rb") as binary:
        digest = hashlib.file_digest(binary, "sha256").hexdigest()
    dump(args.output / "plan.json", {
        "model": args.model, "effort": "low", "seed": args.seed, "update_check": "off",
        "plan": [{"group": name, "arms": arms, "tasks": plan[name][0]} for name, arms in zip(names, order)],
        "binary_sha256": digest,
        "binary_version": subprocess.check_output([args.binary, "--version"], text=True).strip(),
        "codex_version": subprocess.check_output([args.codex, "--version"], text=True).strip(),
    })
    rows, failures = [], []
    for name, arms in zip(names, order):
        task_ids, overlap = plan[name]
        for arm in arms:
            folder = args.output / f"{name}-{arm}"
            folder.mkdir()
            try:
                rows.extend(run_group(args, folder, [tasks[task_id] for task_id in task_ids], overlap, arm))
            except (RuntimeError, OSError, subprocess.TimeoutExpired, ValueError) as error:
                failure = {"group": name, "arm": arm, "error": str(error)}
                failures.append(failure)
                dump(folder / "error.json", failure)
                print(json.dumps(failure), flush=True)
            dump(args.output / "summary.json", {"results": summary(rows), "failures": failures})
    return int(bool(failures or any(not row["completed"] for row in rows)))


if __name__ == "__main__":
    raise SystemExit(main())
