#!/usr/bin/env python3
"""Paired Codex runs on independent copies of the broker Playground."""
import argparse
import hashlib
import json
import math
import os
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from harness import HERE, load_tasks

sys.path.insert(0, str(HERE.parents[1] / "scripts"))
from run_codex_eval import _parse_usage

DEFAULT_TASKS = ("t01", "t02", "t04", "t06", "t07", "t09")
PEERS = {"t01": "t04", "t04": "t01"}
MEASURES = ("wall_seconds", "uncached_input_tokens", "cached_input_tokens", "output_tokens")


def run(cwd, env, *args, check=True, **kwargs):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True,
                            text=True, timeout=60, **kwargs)
    if check and result.returncode:
        raise RuntimeError(f"{args[:3]} returned {result.returncode}: {(result.stdout + result.stderr)[-1500:]}")
    return result


def rep_environment(root):
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(("AETHYME_", "GIT_")):
            del env[key]
    env.update(
        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0",
        GIT_AUTHOR_NAME="Playground", GIT_AUTHOR_EMAIL="playground@example.invalid",
        GIT_COMMITTER_NAME="Playground", GIT_COMMITTER_EMAIL="playground@example.invalid",
        AETHYME_HOST_STATE_DIR=str(root / "state"), AETHYME_HOST_CACHE_DIR=str(root / "cache"),
        AETHYME_WORKTREE_ROOT=str(root / "worktrees"),
        AETHYME_PREPARATION_CACHE_ROOT=str(root / "preparation"),
        PYTHONDONTWRITEBYTECODE="1",
    )
    return env


def prepare(root, env, git):
    repo = root / "repository"
    remote = root / "origin.git"
    shutil.copytree(HERE / "fixture", repo)
    for name in ("worktrees", "preparation", "bin"):
        (root / name).mkdir()
    gh = root / "bin" / "gh"
    gh.write_text('#!/bin/sh\ncase " $* " in *" pr list "*) printf "[]\\n";; *) exit 64;; esac\n')
    gh.chmod(0o700)
    env["PATH"] = str(gh.parent) + os.pathsep + env["PATH"]
    (repo / ".gitignore").write_text(".aethyme/*\n!.aethyme/config.toml\n__pycache__/\n")
    (repo / ".aethyme").mkdir()
    (repo / ".aethyme/config.toml").write_text(
        '[promote]\nmode = "verify-only"\n[delivery]\npush_session_branches = true\n'
        '[cleanup]\nauto_remove = false\n')
    run(root, env, git, "init", "--bare", "-qb", "main", str(remote))
    # A GitHub-shaped origin is mapped exclusively to a bare repository under this arm.
    origin = "git@github.com:playground/paired.git"
    run(repo, env, git, "init", "--template=", "-qb", "main")
    run(repo, env, git, "config", f"url.{remote}.insteadOf", origin)
    run(repo, env, git, "add", "--", ".")
    run(repo, env, git, "commit", "-qm", "Seed paired Playground")
    run(repo, env, git, "remote", "add", "origin", origin)
    run(repo, env, git, "push", "-qu", "origin", "main")
    return repo


def create_worktree(root, repo, env, git, binary, arm):
    if arm == "broker":
        result = run(repo, env, binary, "broker", "start", "--task", "Implement the assigned Playground task",
                     "--short-name", "worker", "--agent", "Worker <worker@example.invalid>",
                     "--json")
        info = json.loads(result.stdout)
        checkout = Path(info["worktree_path"]).resolve()
        if not checkout.is_relative_to(root / "worktrees"):
            raise RuntimeError("Broker worktree escaped experiment root")
        return checkout, info["branch"], info["id"]
    checkout = root / "worktrees" / "worker"
    run(repo, env, git, "worktree", "add", "-qb", "agent/worker", str(checkout), "origin/main")
    return checkout, "agent/worker", None


def codex_attempt(args, result_path, root, checkout, env, prompt):
    argv = [
        args.codex, "exec", "--ignore-user-config", "--ephemeral", "--json",
        "--sandbox", "workspace-write", "-c", 'model_reasoning_effort="low"',
        "-C", str(checkout), "--add-dir", str(root), "-m", args.model, prompt,
    ]
    try:
        with result_path.open("w") as stdout, result_path.with_suffix(".stderr").open("w") as stderr:
            process = subprocess.run(argv, env=env, cwd=checkout, stdout=stdout, stderr=stderr,
                                     stdin=subprocess.DEVNULL, text=True, timeout=args.timeout)
        code = process.returncode
    except subprocess.TimeoutExpired:
        code = 124
    return {"exit": code, "usage": _parse_usage(result_path)}


def validate(repo, checkout, env, git, task, branch):
    # The hidden check stays outside the agent's checkout and runs only after its turn.
    test = HERE / "hidden_checks" / task["hidden_check"]
    with tempfile.TemporaryDirectory(prefix="paired-hidden-") as scratch:
        scratch = Path(scratch)
        shutil.copyfile(test, scratch / "hidden.py")
        result = run(scratch, dict(env, PYTHONPATH=str(checkout)), sys.executable,
                     "-m", "unittest", "-q", "hidden", check=False)
    suite = run(checkout, env, sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q",
                check=False)
    head = run(checkout, env, git, "rev-parse", "HEAD").stdout.strip()
    published = run(repo, env, git, "ls-remote", "--heads", "origin", f"refs/heads/{branch}").stdout
    return {"hidden": result.returncode == 0, "suite": suite.returncode == 0,
            "published_exact_head": published.split()[:1] == [head],
            "clean": not run(checkout, env, git, "status", "--porcelain").stdout,
            "head": head}


def trial(args, folder, task, arm):
    git = shutil.which("git")
    with tempfile.TemporaryDirectory(prefix=f"paired-{arm}-") as temporary:
        root = Path(temporary).resolve()
        env = rep_environment(root)
        repo = prepare(root, env, git)
        started = time.monotonic()
        checkout, branch, session = create_worktree(root, repo, env, git, args.binary, arm)
        base = run(checkout, env, git, "rev-parse", "HEAD").stdout.strip()
        scope = run(repo, env, git, "apply", "--numstat",
                    str(HERE / "reference" / f"{task['id']}.patch")).stdout
        allowed = {row.split("\t")[2] for row in scope.splitlines()}
        if arm == "broker":
            lease = run(checkout, env, args.binary, "broker", "advanced", "leases", "claim",
                        *sorted(allowed), "--session", str(session), "--json")
            (folder / "lease.json").write_text(lease.stdout)
            broker = shlex.quote(args.binary)
            push = f"{broker} broker push --session {session} --json"
            status = f"{broker} broker status --session {session} --json"
        else:
            push = f"git push origin HEAD:refs/heads/{branch}"
            status = "git status --short --branch"
        prompt = (
            f"Implement {task['title']}.\n{task['spec']}\nAcceptance: {task['acceptance']}\n"
            f"Work only in the assigned isolated Git worktree, on its existing branch. "
            f"Only edit these owned paths: {', '.join(sorted(allowed))}. "
            "Stage only explicit owned paths. Run python3 -m unittest discover -s tests -t . -q. "
            "Commit your work. Never force push or modify upstream main. "
            f"Inspect the session with {status}. Publish the committed session using {push}. "
            "The origin is a disposable local experiment. Finish after a clean, successful push. "
            "Do not search outside the worktree for additional task information."
        )
        attempts = [codex_attempt(args, folder / "attempt-1.jsonl", root, checkout, env, prompt)]
        first = validate(repo, checkout, env, git, task, branch)
        changed = set(run(checkout, env, git, "diff", "--name-only", base, "HEAD").stdout.splitlines())
        scope_ok = bool(changed) and changed <= allowed
        valid = all(first[k] for k in ("hidden", "suite", "published_exact_head", "clean")) and scope_ok
        conflict = False
        merged = False
        if valid:
            if peer := PEERS.get(task["id"]):
                with_peer = HERE / "reference" / f"{peer}.patch"
                run(repo, env, git, "apply", "--index", str(with_peer))
                run(repo, env, git, "commit", "-qm", "Peer completes overlapping task")
                run(repo, env, git, "push", "origin", "main")
            run(repo, env, git, "fetch", "origin", branch)
            merge = run(repo, env, git, "merge", "--no-edit", "FETCH_HEAD", check=False)
            conflict = bool(run(repo, env, git, "diff", "--name-only", "--diff-filter=U").stdout)
            merged = merge.returncode == 0
            if conflict:
                run(repo, env, git, "merge", "--abort")
                peer_head = run(repo, env, git, "rev-parse", "HEAD").stdout.strip()
                opened = run(checkout, env, git, "merge", "--no-edit", peer_head, check=False)
                if opened.returncode == 0:
                    raise RuntimeError("expected the same conflict in the isolated checkout")
                repair = (
                    f"{prompt} Your own feature is already committed. The harness merged the "
                    "peer change into your worktree and left the conflicting merge open. "
                    "Resolve the conflict while retaining both features. Stage the resolved "
                    "file, run the tests, commit the merge, and publish it using the prescribed "
                    "command. Do not fetch or start another merge."
                )
                attempts.append(codex_attempt(
                    args, folder / "attempt-2.jsonl", root, checkout, env, repair))
                repaired = validate(repo, checkout, env, git, task, branch)
                if all(repaired[k] for k in ("hidden", "suite", "published_exact_head", "clean")):
                    repair_paths = set(run(checkout, env, git, "diff", "--name-only",
                                           peer_head, "HEAD").stdout.splitlines())
                    if not repair_paths or not repair_paths <= allowed:
                        raise RuntimeError("repaired branch changed paths outside the assigned task")
                    run(repo, env, git, "fetch", "origin", branch)
                    repaired_merge = run(repo, env, git, "merge", "--ff-only", "FETCH_HEAD",
                                         check=False)
                    merged = repaired_merge.returncode == 0
            if merged:
                integrated = run(repo, env, sys.executable, "-m", "unittest", "discover",
                                 "-s", "tests", "-t", ".", "-q", check=False)
                merged = integrated.returncode == 0
        usage = {key: sum(values) if all(value is not None for value in values) else None
                 for key in MEASURES[1:]
                 if (values := [attempt["usage"][key] for attempt in attempts])}
        return {
            "task": task["id"], "arm": arm, "wall_seconds": time.monotonic() - started,
            "attempts": attempts, **{key: usage[key] for key in MEASURES[1:]},
            "valid_candidate": valid, "validation": first, "owned_paths_only": scope_ok,
            "planned_peer": PEERS.get(task["id"]), "merge_conflict": conflict, "merged": merged,
            "rework_required": conflict or not merged, "human_interventions": 0,
        }


def summarize(rows):
    result = {}
    matched = {row["task"] for row in rows if row["merged"]
               and any(peer["task"] == row["task"] and peer["arm"] != row["arm"]
                       and peer["merged"] for peer in rows)}
    for arm in ("baseline", "broker"):
        selected = [row for row in rows if row["arm"] == arm]
        summary = {key: sum(row[key] for row in selected) for key in
                   ("valid_candidate", "merged", "merge_conflict", "rework_required", "human_interventions")}
        summary["trials"] = len(selected)
        summary["matched_completed_tasks"] = len(matched)
        completed = [row for row in selected if row["task"] in matched]
        for key in MEASURES:
            values = sorted(row[key] for row in completed if row[key] is not None)
            summary[key] = {"n": len(values), "median": statistics.median(values),
                            "p90": values[math.ceil(len(values) * .9) - 1]} if values else {"n": 0}
        result[arm] = summary
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=lambda path: str(Path(path).resolve(strict=True)))
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--model", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--seed", type=int, default=19)
    parser.add_argument("--timeout", type=int, default=360)
    parser.add_argument("tasks", nargs="*", default=DEFAULT_TASKS)
    args = parser.parse_args()
    tasks = {task["id"]: task for task in load_tasks()["tasks"]}
    if len(set(args.tasks)) != len(args.tasks) or set(args.tasks) - tasks.keys():
        parser.error("task IDs must be unique and present in tasks.yaml")
    args.output.mkdir(parents=True, exist_ok=False)
    rng = random.Random(args.seed)
    orders = [("baseline", "broker"), ("broker", "baseline")] * ((len(args.tasks) + 1) // 2)
    orders = orders[:len(args.tasks)]
    rng.shuffle(orders)
    plan = list(zip(args.tasks, orders, strict=True))
    with open(args.binary, "rb") as binary:
        binary_sha256 = hashlib.file_digest(binary, "sha256").hexdigest()
    (args.output / "plan.json").write_text(json.dumps({
        "model": args.model, "effort": "low", "seed": args.seed, "plan": plan,
        "binary_sha256": binary_sha256,
        "binary_version": subprocess.check_output([args.binary, "--version"], text=True).strip(),
    }, indent=2) + "\n")
    rows = []
    for task, arms in plan:
        for arm in arms:
            folder = args.output / f"{task}-{arm}"
            folder.mkdir()
            try:
                row = trial(args, folder, tasks[task], arm)
            except (RuntimeError, OSError, subprocess.TimeoutExpired, ValueError) as error:
                row = {"task": task, "arm": arm, "harness_error": str(error)}
            rows.append(row)
            (folder / "result.json").write_text(json.dumps(row, indent=2) + "\n")
            print(json.dumps(row), flush=True)
    measured = [row for row in rows if "harness_error" not in row]
    (args.output / "summary.json").write_text(json.dumps(summarize(measured), indent=2) + "\n")
    return int(len(measured) != len(rows) or any(attempt["exit"] for row in measured for attempt in row["attempts"]))


if __name__ == "__main__":
    raise SystemExit(main())
