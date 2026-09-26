#!/usr/bin/env python3
"""Broker benchmark (P5.6) harness: playground and per-arm setup.

    harness.py playground <dest>
    harness.py setup-arm {a,b,c} <arm-root> [--rep N]

`setup-arm` creates a fresh arm root and writes <arm-root>/launch_plan.json,
which is everything the coordinator needs to launch the four agents through
Chau7 (`agent_launch` with `directory`, `agent_command` and `prompt`).

Nothing here launches an agent, touches the broker database, or runs inside
the Aethyme repository (cardinal rule 1): every destination must lie outside
any Git work tree.
"""

import argparse
import datetime as _dt
import json
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "fixture"
HARNESS_VERSION = "1"
GIT = os.environ.get("BENCH_GIT") or ("/usr/bin/git" if Path("/usr/bin/git").exists() else "git")
TEST_COMMAND = "python3 -m unittest discover -s tests -t . -q"
BASE_DATE = "2026-01-01T00:00:00+00:00"

# Chau7's CTO git wrapper rewrites git output in agent tabs; our own git
# calls parse output, so opt out (same reason as the repo's gates.toml).
os.environ["CHAU7_CTO_OPTIM_ACTIVE"] = "1"


# --------------------------------------------------------------------------
# helpers


def git(*args, cwd=None, check=True, env=None, capture=True):
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    proc = subprocess.run([GIT, *args], cwd=cwd, env=full_env, text=True,
                          stdout=subprocess.PIPE if capture else None,
                          stderr=subprocess.PIPE if capture else None)
    if check and proc.returncode != 0:
        raise SystemExit(f"git {' '.join(args)} failed in {cwd}:\n{proc.stderr}")
    return proc.stdout.strip() if capture else ""


def load_tasks():
    """Parse tasks.yaml with PyYAML, falling back to Ruby's stdlib YAML."""
    path = HERE / "tasks.yaml"
    try:
        import yaml  # type: ignore
        return yaml.safe_load(path.read_text())
    except ImportError:
        out = subprocess.run(["ruby", "-ryaml", "-rjson", "-e",
                              "puts JSON.generate(YAML.load_file(ARGV[0]))", str(path)],
                             check=True, text=True, stdout=subprocess.PIPE).stdout
        return json.loads(out)


def refuse_inside_git(dest):
    """Cardinal rule 1 and broker hygiene: the playground is its own repo."""
    probe = dest
    while not probe.exists():
        probe = probe.parent
    proc = subprocess.run([GIT, "-C", str(probe), "rev-parse", "--show-toplevel"],
                          text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    if proc.returncode == 0:
        raise SystemExit(f"refusing: {dest} is inside the Git work tree {proc.stdout.strip()}; "
                         "the playground must be a standalone repository (never Aethyme itself)")


def write_exec(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def now():
    return _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")


# --------------------------------------------------------------------------
# playground


BASE_ENV = {
    "GIT_AUTHOR_NAME": "Bench Fixture", "GIT_AUTHOR_EMAIL": "fixture@bench.invalid",
    "GIT_COMMITTER_NAME": "Bench Fixture", "GIT_COMMITTER_EMAIL": "fixture@bench.invalid",
    "GIT_AUTHOR_DATE": BASE_DATE, "GIT_COMMITTER_DATE": BASE_DATE,
}


def make_playground(dest):
    dest = Path(dest).resolve()
    refuse_inside_git(dest)
    if dest.exists() and any(dest.iterdir()):
        raise SystemExit(f"refusing: {dest} exists and is not empty")
    shutil.copytree(FIXTURE, dest, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    git("init", "-q", "-b", "main", cwd=dest)
    git("config", "user.name", "Bench Operator", cwd=dest)
    git("config", "user.email", "operator@bench.invalid", cwd=dest)
    git("add", "-A", cwd=dest)
    git("commit", "-q", "-m", "shoplib 0.4.0 (benchmark base)", cwd=dest, env=BASE_ENV)
    git("tag", "bench-base", cwd=dest)
    return dest, git("rev-parse", "HEAD", cwd=dest)


# --------------------------------------------------------------------------
# prompts

COMMON_HEAD = """You are agent {slot} of four coding agents working at the same time on the
`shoplib` Python repository. The other three agents are changing the same
codebase concurrently, so main moves while you work.

You have three tasks. Do them in order, one at a time, and land each one
before you start the next. Each task names the test file its tests go in.

Test command (standard library only, runs in under a second):
    {test_command}

"""

COMMON_TAIL = """
When all three tasks have landed, reply with one line per task (id, landed or
not, and why) and stop. Work autonomously: do not ask the operator questions
unless you are truly blocked, and never edit files outside the repository.
"""

WORKFLOW_PR = """## How to land a task (pull requests through the merge queue)

The shared repository is the Git remote `origin` (a local bare repository);
`origin/main` is main. Never push to main directly. A task lands when its pull
request is merged by the merge queue.

For each task:
1. Start from the latest main:
       git fetch origin && git switch -C {branch_prefix}-<task-id> origin/main
2. Implement it, run the test command, and commit.
3. Open (or update) the pull request:
       {pr} open <task-id>
   This pushes HEAD to `pr/<task-id>` on origin and enqueues it.
4. Wait for the verdict:
       {pr} wait <task-id>
   - exit 0: merged and main's tests pass. Move on.
   - exit 3: merge conflict with main. Rebase onto origin/main, resolve, run the
     tests, commit, and `{pr} open <task-id>` again.
   - exit 4: your branch failed CI (the test suite on your branch alone). Fix,
     commit, and `{pr} open <task-id>` again.
   - exit 5: merged, but main fails its tests after the merge. Fix main from a
     fresh branch off origin/main and `{pr} open <task-id>` again.
The queue merges pull requests in the order they are opened. CI runs on your
branch as pushed; it does not test the result of merging with main.
"""

WORKTREE_A = """## Your checkout

You are in a dedicated `git worktree` at:
    {directory}
Work only there.

"""

WORKTREE_B = """## Your checkout

You were launched with {tool}'s native worktree option, so this session
already runs in its own Git worktree created from {repo}. Work only in that
worktree (`git rev-parse --show-toplevel` shows it).

"""

WORKFLOW_BROKER = """## How to land a task (Aethyme broker)

This repository is coordinated by the Aethyme broker (v0.8.4). A task lands
when a broker submission of it is verified and promoted to the local
`aethyme/integration` branch. Do not push anywhere.

For each task:
1. From {repo}, start a session:
       aethyme broker start --task "<task-id>: <title>" --agent "{agent}" --json
   It creates an isolated worktree and prints its path and session id.
   `cd` into that worktree and work only there.
2. Implement the task, run the test command, and commit. Only committed work
   integrates.
3. Submit:
       aethyme broker submit --session <id>
   Submit merges your commits onto the current integration tip in a scratch
   tree, runs the repository's gates (the full test suite) on the merged
   result, and promotes it if they pass.
   - exit 0: landed. Move on.
   - exit 3: refused (for example a conflict with work that landed first). The
     output, and `.aethyme/broker-action-required.md` in your worktree if it
     appears, say what to do; usually rebase onto `aethyme/integration`,
     resolve, run the tests, commit, and submit again.
   - exit 4: a gate failed on the merged tree. Fix, commit, submit again.
   Run `aethyme broker status --json` if you need to see other sessions.
4. Close the session, then go back to {repo} for the next task:
       aethyme broker finish --session <id>
"""

TASK_BLOCK = """## Task {n} of 3: {id} - {title}

{spec}
Acceptance: {acceptance}
"""


def task_text(tasks, n):
    return "\n".join(
        TASK_BLOCK.format(n=i + 1, id=t["id"], title=t["title"], spec=t["spec"].strip() + "\n",
                          acceptance=" ".join(t["acceptance"].split()))
        for i, t in enumerate(tasks))


def render_prompt(arm, slot, slot_cfg, tasks, ctx):
    head = COMMON_HEAD.format(slot=slot, test_command=TEST_COMMAND)
    if arm == "a":
        body = WORKTREE_A.format(directory=ctx["directory"]) + WORKFLOW_PR.format(
            pr=ctx["pr"], branch_prefix=f"work/{slot}")
    elif arm == "b":
        tool = "Claude Code" if slot_cfg["harness"] == "claude" else "Codex"
        body = WORKTREE_B.format(tool=tool, repo=ctx["repo"]) + WORKFLOW_PR.format(
            pr=ctx["pr"], branch_prefix=f"work/{slot}")
    else:
        body = WORKFLOW_BROKER.format(repo=ctx["repo"], agent=f"bench-{slot} <bench-{slot}@bench.invalid>")
    return head + body + "\n" + task_text(tasks, 3) + COMMON_TAIL


# --------------------------------------------------------------------------
# arms

GATES_TOML = """# Broker benchmark playground gates (arm C).
# One cheap gate: the whole unit suite, on every path.
[[gate]]
name = "unit-tests"
command = "PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -t . -q"
cost = 1
timeout_seconds = 120
triggers = ["**"]
"""

AETHYME_WRAPPER = """#!/bin/sh
# Transparent logging wrapper for arm C: records argv, cwd and exit code of
# every aethyme call to broker_log.jsonl, then returns the real exit code.
# stdin/stdout/stderr pass straight through.
REAL={real}
LOG={log}
start=$(date -u +%Y-%m-%dT%H:%M:%SZ)
"$REAL" "$@"
rc=$?
python3 - "$LOG" "$start" "$rc" "$PWD" "$@" <<'PY'
import json, sys, datetime
log, start, rc, cwd, argv = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4], sys.argv[5:]
end = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
with open(log, "a") as fh:
    fh.write(json.dumps({{"start": start, "end": end, "exit": rc, "cwd": cwd, "argv": argv}}) + "\\n")
PY
exit $rc
"""

PR_WRAPPER = """#!/bin/sh
# Local pull-request client for arms A and B (see merge_queue.py).
exec python3 {queue} --arm-root {root} "$@"
"""


def find_real_aethyme(skip_dir):
    for entry in os.environ.get("PATH", "").split(os.pathsep):
        candidate = Path(entry) / "aethyme"
        if Path(entry).resolve() == skip_dir.resolve():
            continue
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return "aethyme"


def shq(value):
    return "'" + str(value).replace("'", "'\\''") + "'"


def setup_arm(arm, root, rep):
    root = Path(root).resolve()
    refuse_inside_git(root)
    if root.exists() and any(root.iterdir()):
        raise SystemExit(f"refusing: {root} exists and is not empty (use a fresh arm root per run)")
    root.mkdir(parents=True, exist_ok=True)
    spec = load_tasks()

    seed, base_sha = make_playground(root / "seed")
    remote = root / "remote.git"
    git("clone", "-q", "--bare", str(seed), str(remote))
    git("config", "core.logAllRefUpdates", "always", cwd=remote)  # reflog = landing history
    shutil.rmtree(seed)

    repo = root / "repo"
    git("clone", "-q", str(remote), str(repo))
    git("config", "user.name", "Bench Operator", cwd=repo)
    git("config", "user.email", "operator@bench.invalid", cwd=repo)
    bin_dir = root / "bin"
    bin_dir.mkdir()

    env_prefix = ""
    landing = {"repo": str(remote), "ref": "refs/heads/main"}
    if arm in ("a", "b"):
        shutil.copy(HERE / "merge_queue.py", bin_dir / "merge_queue.py")
        write_exec(bin_dir / "pr", PR_WRAPPER.format(queue=shq(bin_dir / "merge_queue.py"), root=shq(root)))
        merger = root / "merger"
        git("clone", "-q", str(remote), str(merger))
        git("config", "user.name", "Merge Queue", cwd=merger)
        git("config", "user.email", "merge-queue@bench.invalid", cwd=merger)
        (root / "queue.jsonl").touch()
        (root / "merge_log.jsonl").touch()
    else:
        (repo / ".aethyme").mkdir()
        (repo / ".aethyme" / "gates.toml").write_text(GATES_TOML)
        git("add", ".aethyme/gates.toml", cwd=repo)
        git("commit", "-q", "-m", "chore: add broker gates for the benchmark", cwd=repo,
            env={**BASE_ENV, "GIT_AUTHOR_NAME": "Bench Operator", "GIT_COMMITTER_NAME": "Bench Operator"})
        git("push", "-q", "origin", "main", cwd=repo)
        write_exec(bin_dir / "aethyme", AETHYME_WRAPPER.format(
            real=shq(find_real_aethyme(bin_dir)), log=shq(root / "broker_log.jsonl")))
        (root / "broker_log.jsonl").touch()
        (root / "broker-worktrees").mkdir()
        env_prefix = (f"env AETHYME_WORKTREE_ROOT={shq(root / 'broker-worktrees')} "
                      f"PATH={shq(bin_dir)}:\"$PATH\" ")
        landing = {"repo": str(repo), "ref": "refs/heads/aethyme/integration"}

    (root / "operator_log.jsonl").touch()
    prompts_dir = root / "prompts"
    prompts_dir.mkdir()

    slots = []
    for slot, cfg in spec["slots"].items():
        tasks = sorted((t for t in spec["tasks"] if t["slot"] == slot), key=lambda t: t["position"])
        command = cfg["agent_command"]
        if arm == "a":
            directory = root / "wt" / slot
            git("worktree", "add", "-q", "-b", f"work/{slot}", str(directory), "origin/main", cwd=repo)
        else:
            directory = repo
        if arm == "b":
            command += f" --worktree bench-{slot}" if cfg["harness"] == "claude" else " --worktree"
        ctx = {"directory": str(directory), "repo": str(repo), "pr": str(bin_dir / "pr")}
        prompt = render_prompt(arm, slot, cfg, tasks, ctx)
        (prompts_dir / f"{slot}.md").write_text(prompt)
        slots.append({
            "slot": slot, "harness": cfg["harness"], "model": cfg["model"],
            "directory": str(directory), "agent_command": env_prefix + command,
            "prompt_file": str(prompts_dir / f"{slot}.md"), "prompt": prompt,
            "tasks": [t["id"] for t in tasks],
        })

    plan = {
        "benchmark": "aethyme-broker-p5.6", "harness_version": HARNESS_VERSION,
        "arm": arm, "repetition": rep, "root": str(root), "created_at": now(),
        "base_sha": base_sha, "repo": str(repo), "landing": landing,
        "test_command": TEST_COMMAND, "slots": slots,
        "logs": {
            "operator": str(root / "operator_log.jsonl"),
            "merge": str(root / "merge_log.jsonl") if arm != "c" else None,
            "broker": str(root / "broker_log.jsonl") if arm == "c" else None,
        },
        "chau7_tag": f"bench-p56-arm{arm}-r{rep}",
    }
    (root / "launch_plan.json").write_text(json.dumps(plan, indent=2) + "\n")
    return plan


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("playground")
    p.add_argument("dest")
    a = sub.add_parser("setup-arm")
    a.add_argument("arm", choices=["a", "b", "c"])
    a.add_argument("root")
    a.add_argument("--rep", type=int, default=1)
    args = parser.parse_args(argv)
    if args.cmd == "playground":
        dest, sha = make_playground(args.dest)
        print(f"playground {dest} base {sha}")
    else:
        plan = setup_arm(args.arm, args.root, args.rep)
        print(f"arm {plan['arm']} ready: {plan['root']}/launch_plan.json (base {plan['base_sha'][:12]})")
        for s in plan["slots"]:
            print(f"  {s['slot']} {s['model']:<11} {s['directory']}  tasks={','.join(s['tasks'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
