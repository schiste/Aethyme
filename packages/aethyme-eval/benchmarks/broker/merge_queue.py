#!/usr/bin/env python3
"""Local pull-request flow for arms A and B (no GitHub).

Agent side (from its worktree):
    pr open <task-id>    push HEAD to origin pr/<task-id> and enqueue it
    pr wait <task-id>    block until the queue has judged the latest attempt
                         exit 0 merged+main green, 3 conflict, 4 CI failed,
                         5 merged but main red afterwards, 6 timeout

Coordinator side:
    merge_queue.py --arm-root R serve     process the queue until interrupted
    merge_queue.py --arm-root R drain     process what is queued, then exit

Semantics mirror a GitHub repository with required CI but without "require
branches to be up to date": CI runs on the pushed branch alone; a clean
textual merge then lands on main, and main's suite runs after the merge.
Every verdict goes to <arm-root>/merge_log.jsonl.
"""

import argparse
import datetime as _dt
import fcntl
import json
import os
import subprocess
import sys
import time
from pathlib import Path

GIT = os.environ.get("BENCH_GIT") or ("/usr/bin/git" if Path("/usr/bin/git").exists() else "git")
TEST_COMMAND = ["python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q"]
TEST_TIMEOUT = 180
EXIT = {"merged": 0, "conflict": 3, "ci_failed": 4, "merged_main_red": 5}
os.environ["CHAU7_CTO_OPTIM_ACTIVE"] = "1"
os.environ["PYTHONDONTWRITEBYTECODE"] = "1"


def now():
    return _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")


def git(*args, cwd, check=True):
    proc = subprocess.run([GIT, *args], cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {proc.stderr.strip()}")
    return proc


def append(path, record):
    with open(path, "a") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)
        fh.write(json.dumps(record) + "\n")
        fcntl.flock(fh, fcntl.LOCK_UN)


def read_jsonl(path):
    if not Path(path).exists():
        return []
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


def run_tests(cwd):
    try:
        proc = subprocess.run(TEST_COMMAND, cwd=cwd, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, timeout=TEST_TIMEOUT)
        return proc.returncode == 0, proc.stdout[-4000:]
    except subprocess.TimeoutExpired:
        return False, "timeout"


# ----------------------------------------------------------------- agent side


def cmd_open(root, task):
    if git("status", "--porcelain", "--untracked-files=no", cwd=os.getcwd()).stdout.strip():
        print("refusing: commit your changes first (tracked files are modified)")
        return 2
    sha = git("rev-parse", "HEAD", cwd=os.getcwd()).stdout.strip()
    push = git("push", "-f", "origin", f"HEAD:refs/heads/pr/{task}", cwd=os.getcwd(), check=False)
    if push.returncode != 0:
        print(push.stderr)
        return 1
    attempt = 1 + sum(1 for e in read_jsonl(root / "queue.jsonl") if e["task"] == task)
    append(root / "queue.jsonl", {"ts": now(), "task": task, "sha": sha, "attempt": attempt,
                                  "worktree": os.getcwd()})
    print(f"opened pr/{task} attempt {attempt} at {sha[:12]}; run: pr wait {task}")
    return 0


def cmd_wait(root, task, timeout):
    queued = [e for e in read_jsonl(root / "queue.jsonl") if e["task"] == task]
    if not queued:
        print(f"no pull request for {task}; run: pr open {task}")
        return 2
    attempt = queued[-1]["attempt"]
    deadline = time.time() + timeout
    while time.time() < deadline:
        for rec in reversed(read_jsonl(root / "merge_log.jsonl")):
            if rec["task"] == task and rec["attempt"] == attempt and rec["result"] != "superseded":
                print(f"{task} attempt {attempt}: {rec['result']}")
                if rec.get("detail"):
                    print(rec["detail"])
                return EXIT.get(rec["result"], 1)
        time.sleep(3)
    print(f"timeout waiting for {task}; the merge queue may be stopped - tell the operator")
    return 6


# ------------------------------------------------------------ merger side


def process(root, entry):
    merger = root / "merger"
    rec = {"ts": now(), "task": entry["task"], "attempt": entry["attempt"], "sha": entry["sha"],
           "queued_at": entry["ts"]}
    git("fetch", "-q", "--prune", "origin", cwd=merger)
    head = git("rev-parse", f"origin/pr/{entry['task']}", cwd=merger, check=False).stdout.strip()
    if head != entry["sha"]:
        rec.update(result="superseded", detail=f"pr/{entry['task']} now at {head[:12]}")
        return rec

    # 1. CI on the branch as pushed (not merged with main).
    git("checkout", "-q", "--detach", entry["sha"], cwd=merger)
    ok, out = run_tests(merger)
    rec["branch_ci_pass"] = ok
    if not ok:
        rec.update(result="ci_failed", detail=out[-1500:])
        return rec

    # 2. Textual merge onto current main.
    git("checkout", "-q", "-B", "main", "origin/main", cwd=merger)
    main_before = git("rev-parse", "HEAD", cwd=merger).stdout.strip()
    merge = git("merge", "--no-ff", "-q", "-m", f"Merge pr/{entry['task']} (attempt {entry['attempt']})",
                entry["sha"], cwd=merger, check=False)
    if merge.returncode != 0:
        conflicted = git("diff", "--name-only", "--diff-filter=U", cwd=merger, check=False).stdout.split()
        git("merge", "--abort", cwd=merger, check=False)
        rec.update(result="conflict", main_before=main_before, conflicted_paths=conflicted,
                   detail="merge conflict in: " + ", ".join(conflicted))
        return rec

    # 3. Land, then run main's suite after the merge.
    merged = git("rev-parse", "HEAD", cwd=merger).stdout.strip()
    git("push", "-q", "origin", "main", cwd=merger)
    ok, out = run_tests(merger)
    rec.update(main_before=main_before, main_after=merged, main_green_after=ok,
               result="merged" if ok else "merged_main_red", detail="" if ok else out[-1500:])
    return rec


def pending(root):
    done = {(r["task"], r["attempt"]) for r in read_jsonl(root / "merge_log.jsonl")}
    return [e for e in read_jsonl(root / "queue.jsonl") if (e["task"], e["attempt"]) not in done]


def cmd_serve(root, once):
    print(f"merge queue serving {root} (ctrl-c to stop)" if not once else f"draining {root}")
    while True:
        todo = pending(root)
        for entry in todo:
            rec = process(root, entry)
            append(root / "merge_log.jsonl", rec)
            print(f"{rec['ts']} {rec['task']}#{rec['attempt']}: {rec['result']}", flush=True)
        if once and not pending(root):
            return 0
        if not todo:
            time.sleep(2)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--arm-root", required=True)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("open").add_argument("task")
    w = sub.add_parser("wait")
    w.add_argument("task")
    w.add_argument("--timeout", type=int, default=1800)
    sub.add_parser("serve")
    sub.add_parser("drain")
    args = parser.parse_args(argv)
    root = Path(args.arm_root).resolve()
    if args.cmd == "open":
        return cmd_open(root, args.task)
    if args.cmd == "wait":
        return cmd_wait(root, args.task, args.timeout)
    return cmd_serve(root, once=args.cmd == "drain")


if __name__ == "__main__":
    sys.exit(main())
