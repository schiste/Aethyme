#!/usr/bin/env python3
"""Compare direct Git and Aethyme with live Codex workers on the broker Playground."""

import argparse
import errno
import hashlib
import json
import os
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
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
    coordination = f"Inspect coordination state using {inspect}."
    if args.hook_home and arm == "broker":
        coordination = (
            "Use the Aethyme startup context and hook updates for coordination. "
            f"For details on demand, use {inspect}."
        )
    delivery = (
        "The coordinator will stage, commit and publish after your turn. "
        "Leave the tested changes in the assigned worktree; do not commit or push."
        if args.coordinator_publish else
        f"Stage only explicit owned paths and commit. Publish with {publish}. "
        "Stop after a clean successful publication."
    )
    prompt = (
        f"Implement {task['title']}.\n{task['spec']}\nAcceptance: {task['acceptance']}\n"
        f"{peer_context}Work only in the assigned isolated Git worktree on its existing branch. "
        f"Only edit these owned paths: {', '.join(sorted(owned))}. "
        f"{coordination} Use relevant signals to avoid rework. "
        "Do not change another worktree or branch, force-push or write to main. "
        f"Run python3 -m unittest discover -s tests -t . -q. {delivery} "
        "The origin is a disposable local experiment. Do not search outside your worktree."
    )
    return {"task": task, "session": session, "branch": branch, "checkout": checkout, "owned": owned,
            "base": run(checkout, env, git, "rev-parse", "HEAD").stdout.strip(), "prompt": prompt,
            "started": started, "folder": folder,
            "codex_config": {
                'plugins."aethyme@aethyme".enabled': arm == "broker",
                f"projects.{json.dumps(str(checkout))}.trust_level": "trusted",
            } if args.hook_home else None}


def verify_branch(repo, env, git, work, base):
    paths = set(run(work["checkout"], env, git, "diff", "--name-only", base, "HEAD").stdout.splitlines())
    result = validate(repo, work["checkout"], env, git, work["task"], work["branch"])
    result["owned_paths_only"] = bool(paths) and paths <= work["owned"]
    result["valid"] = all(result[k] for k in ("hidden", "suite", "published_exact_head", "clean",
                                              "owned_paths_only"))
    return result


def coordinator_publish(args, env, git, work, expected_head, reference, receipt_name):
    checkout, folder = work["checkout"], work["folder"]
    receipt = {"published": False}
    try:
        head = run(checkout, env, git, "rev-parse", "HEAD").stdout.strip()
        branch = run(checkout, env, git, "branch", "--show-current").stdout.strip()
        if head != expected_head or branch != work["branch"]:
            raise ValueError("worker changed the checkout head or branch")
        changes = run(checkout, env, git, "status", "--porcelain=v1", "-z",
                      "--untracked-files=all", "--no-renames").stdout.split("\0")
        stage = sorted({row[3:] for row in changes if row and row[3:] in work["owned"]})
        if not stage:
            raise ValueError("no changed owned paths to stage")
        run(checkout, env, git, "add", "--", *stage)
        paths = set(run(checkout, env, git, "diff", "--cached", "--name-only", "--no-renames",
                        "-z", reference).stdout.split("\0")) - {""}
        unstaged = run(checkout, env, git, "diff", "--name-only", "-z").stdout
        untracked = run(checkout, env, git, "ls-files", "--others", "--exclude-standard", "-z").stdout
        if not paths or paths - work["owned"] or unstaged or untracked:
            raise ValueError("candidate contains disallowed paths or unexplained changes")
        run(checkout, env, git, "commit", "-qm", work["task"]["title"])
        session = work["session"]
        argv = ((args.binary, "broker", "advance", "--session", str(session), "--json") if session
                else (git, "push", "origin", f"HEAD:refs/heads/{branch}"))
        result = run(checkout, env, *argv, check=False)
        receipt.update(published=result.returncode == 0, exit=result.returncode,
                       stdout=result.stdout, stderr=result.stderr)
    except (RuntimeError, ValueError) as error:
        receipt["error"] = str(error)
    dump(folder / receipt_name, receipt)


def integrate(args, repo, root, env, git, work, arm, first_attempt):
    task, folder, branch, checkout = (work[k] for k in ("task", "folder", "branch", "checkout"))
    attempts = [first_attempt]
    if args.coordinator_publish and first_attempt["exit"] == 0:
        coordinator_publish(args, env, git, work, work["base"], work["base"], "publish.json")
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
            delivery = ("Leave the resolved merge for the coordinator to commit and publish."
                        if args.coordinator_publish else "Commit the merge and publish.")
            attempts.append(codex_attempt(
                args, folder / "repair.jsonl", root, checkout, env,
                work["prompt"] + "\nYour change is committed. The coordinator tried to integrate it "
                "and reproduced a conflict. The peer merge is open in your worktree. Retain both "
                f"features and fix the conflict within your owned paths, then run the test suite. "
                f"{delivery} Do not start another merge.",
                config=work["codex_config"],
            ))
            if args.coordinator_publish and attempts[-1]["exit"] == 0:
                coordinator_publish(args, env, git, work, initial["head"], upstream, "repair-publish.json")
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
            "session_id": work["session"], "repair_used": repair_used,
            "rework_required": repair_used or not completed, "human_interventions": 0}


@contextmanager
def playground(scratch, folder, arm):
    temporary = tempfile.TemporaryDirectory(prefix=f"paired-{arm}-", dir=scratch)
    try:
        yield Path(temporary.name).resolve()
    finally:
        try:
            temporary.cleanup()
        except OSError as error:
            if error.errno != errno.ENOTEMPTY:
                raise
            # Detached broker maintenance can still write to the fixture
            # briefly after all timed commands have returned.
            time.sleep(0.25)
            try:
                temporary.cleanup()
            except OSError as retry_error:
                warning = {"fixture": temporary.name, "error": str(retry_error)}
                dump(folder / "cleanup-warning.json", warning)
                print(json.dumps(warning), file=sys.stderr, flush=True)


def run_group(args, folder, tasks, overlap, arm):
    git = shutil.which("git")
    with playground(args.scratch, folder, arm) as root:
        env = dict(rep_environment(root), AETHYME_UPDATE_CHECK="off", AETHYME_MEASURE_OUTPUT="1")
        if args.hook_home:
            env["CODEX_HOME"] = str(args.hook_home)
            env["PATH"] = str(Path(args.binary).parent) + os.pathsep + env["PATH"]
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
                                    root, work["checkout"], env, work["prompt"],
                                    config=work["codex_config"]) for work in workers]
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
            row["concurrent"] = len(tasks) > 1
            row["group"] = folder.name.removesuffix(f"-{arm}")
            row["group_seconds"] = group_seconds
        if overlap and all(row["completed"] for row in rows):
            overlap_result = check_hidden(repo, env, folder, args.overlaps[overlap]["hidden_check"])
            dump(folder / "overlap-validation.json", {"passed": overlap_result})
            if not overlap_result:
                for row in rows:
                    row["completed"] = False
                    row["overlap_hidden_failure"] = True
        if arm == "broker":
            insights = json.loads(run(repo, env, args.binary, "broker", "advanced",
                                      "insights", "--json").stdout)
            dump(folder / "broker-insights.json", insights)
            signals = {session["session_id"]: session["signals"] for session in insights["sessions"]}
            for row in rows:
                row["activity_signals"] = signals.get(row["session_id"], 0)
        for row, work in zip(rows, workers, strict=True):
            dump(work["folder"] / "result.json", row)
            print(json.dumps(row), flush=True)
        metrics = repo / ".aethyme/logs/command-metrics.jsonl"
        if metrics.is_file():
            shutil.copyfile(metrics, folder / "broker-metrics.jsonl")
        if args.hook_home and arm == "broker" and any(row["activity_signals"] < 2 for row in rows):
            raise RuntimeError("missing broker activity evidence for live Codex hooks")
        return rows


def summary(rows):
    def measure(values):
        values = sorted(v for v in values if v is not None)
        if not values:
            return {"n": 0}
        return {"n": len(values), "median": statistics.median(values),
                "p90": values[(9 * len(values) - 1) // 10]}
    result = {}
    for label, matches in (("all", lambda r: True), ("solo", lambda r: not r["concurrent"]),
                           ("concurrent", lambda r: r["concurrent"])):
        selected = [row for row in rows if matches(row)]
        passed = {
            arm: {(r["group"], r["task"]) for r in selected if r["arm"] == arm and r["completed"]}
            for arm in ("baseline", "broker")
        }
        matched = passed["baseline"] & passed["broker"]
        contrasts = {}
        for arm in ("baseline", "broker"):
            own = [r for r in selected if r["arm"] == arm]
            contrasts[arm] = {
                "trials": len(own), "completed": sum(r["completed"] for r in own),
                "initially_valid": sum(r["initial"]["valid"] for r in own),
                "conflicts": sum(bool(r["conflict"]) for r in own),
                "rework": sum(r["rework_required"] for r in own),
                "repair_turns": sum(r["repair_used"] for r in own),
                "human_interventions": sum(r["human_interventions"] for r in own),
                **{key: measure([r[key] for r in own if (r["group"], r["task"]) in matched])
                   for key in MEASURES},
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
    parser.add_argument("--hook-home", type=lambda arg: Path(arg).resolve(strict=True),
                        help="isolated Codex home with the stock Aethyme plugin and existing hook trust")
    parser.add_argument("--coordinator-publish", action="store_true",
                        help="let the coordinator stage owned edits and publish them in either arm")
    parser.add_argument("--concurrent", action="store_true",
                        help="run six independent groups covering all twelve assignments")
    args = parser.parse_args()
    manifest = load_tasks()
    tasks = {task["id"]: task for task in manifest["tasks"]}
    args.overlaps = {group["id"]: group for group in manifest["overlaps"]}
    plan = {task_id: ([task_id], None) for task_id in ("t02", "t06", "t07", "t10")}
    plan.update({name: (group["tasks"], name) for name, group in args.overlaps.items()})
    if args.concurrent:
        solo = {"t02", "t06", "t07", "t10"}
        plan = {name: value for name, value in plan.items() if name not in solo}
        plan.update({
            "disjoint-postal-shipping": (["t02", "t06"], None),
            "disjoint-search-loyalty": (["t07", "t10"], None),
            "disjoint-report-text": (["t11", "t12"], None),
        })
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
        "trusted_aethyme_hooks_requested": bool(args.hook_home),
        "coordinator_publishes": args.coordinator_publish,
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
