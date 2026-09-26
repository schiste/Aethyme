#!/usr/bin/env python3
"""Dry-run self-test of the broker benchmark harness. Launches no agent and
never touches the broker database.

    selftest.py <scratch-dir>

Checks, in order:
  1. the playground's visible suite passes on the base;
  2. every hidden check fails on the base;
  3. every reference/tNN.patch applies alone, keeps the suite green and makes
     its own hidden check pass;
  4. reference/integrated.patch (all twelve, overlaps resolved) passes the
     suite and every hidden and overlap check;
  5. the overlaps behave as designed under plain git: textual conflicts,
     semantic merges cleanly but fails the suite, shared merges cleanly;
  6. arm A/B/C setup scripts produce a well-formed launch_plan.json;
  7. arm A end to end with reference patches through the real merge queue,
     then score.py on it with synthetic Chau7 telemetry and an operator log;
  8. score.py on a synthetic arm C state (integration ref + broker log).
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import harness  # noqa: E402
import score as scorer  # noqa: E402

GIT = harness.GIT
TEST = harness.TEST_COMMAND.split()
FAILS = []
os.environ["PYTHONDONTWRITEBYTECODE"] = "1"


def check(cond, label):
    print(("PASS " if cond else "FAIL ") + label)
    if not cond:
        FAILS.append(label)


def run(args, cwd, **kw):
    return subprocess.run(args, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **kw)


def suite(cwd):
    return run(TEST, cwd).returncode == 0


def hidden(cwd, filename):
    target = Path(cwd) / f"_hidden_{Path(filename).stem}.py"
    shutil.copy(HERE / "hidden_checks" / filename, target)
    ok = run(["python3", "-m", "unittest", "-q", target.stem], cwd).returncode == 0
    target.unlink()
    return ok


def clone(src, dst):
    run([GIT, "clone", "-q", str(src), str(dst)], None).check_returncode()
    run([GIT, "config", "user.email", "t@bench.invalid"], dst)
    run([GIT, "config", "user.name", "selftest"], dst)
    return dst


def apply(repo, patch, message):
    r = run([GIT, "apply", "--index", str(HERE / "reference" / patch)], repo)
    if r.returncode != 0:
        raise SystemExit(f"{patch} does not apply: {r.stdout}")
    run([GIT, "commit", "-q", "-m", message], repo).check_returncode()


def main(scratch):
    scratch = Path(scratch).resolve()
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    spec = harness.load_tasks()
    tasks = {t["id"]: t for t in spec["tasks"]}

    # 1-2 ----------------------------------------------------------------
    base, base_sha = harness.make_playground(scratch / "playground")
    t = run(TEST, base)
    check(t.returncode == 0, f"base visible suite passes ({t.stdout.strip().splitlines()[-3]})")
    all_checks = [t["hidden_check"] for t in spec["tasks"]] + [o["hidden_check"] for o in spec["overlaps"]]
    for name in all_checks:
        check(not hidden(base, name), f"hidden {name} fails on base")

    # 3 ------------------------------------------------------------------
    for tid, task in tasks.items():
        repo = clone(base, scratch / "ref" / tid)
        apply(repo, f"{tid}.patch", tid)
        check(suite(repo) and hidden(repo, task["hidden_check"]),
              f"reference {tid}: suite green and {task['hidden_check']} passes")
        others = [n for n in all_checks if n != task["hidden_check"]]
        check(not any(hidden(repo, n) for n in others if n.startswith("test_t")),
              f"reference {tid}: no other task's hidden check passes")

    # 4 ------------------------------------------------------------------
    repo = clone(base, scratch / "ref" / "integrated")
    apply(repo, "integrated.patch", "integrated")
    check(suite(repo), "integrated reference: suite green")
    check(all(hidden(repo, n) for n in all_checks), "integrated reference: all 15 hidden checks pass")

    # 5 ------------------------------------------------------------------
    for ov in spec["overlaps"]:
        a, b = ov["tasks"]
        repo = clone(base, scratch / "overlap" / ov["id"])
        run([GIT, "switch", "-q", "-c", b], repo)
        apply(repo, f"{b}.patch", b)
        run([GIT, "switch", "-q", "main"], repo)
        apply(repo, f"{a}.patch", a)
        merged = run([GIT, "merge", "--no-edit", "-q", b], repo)
        if ov["kind"] == "textual":
            check(merged.returncode != 0, f"overlap {ov['id']}: {a} x {b} is a git merge conflict")
        elif ov["kind"] == "semantic":
            check(merged.returncode == 0 and not suite(repo),
                  f"overlap {ov['id']}: {a} x {b} merges cleanly and breaks the suite")
        else:
            check(merged.returncode == 0 and suite(repo) and hidden(repo, ov["hidden_check"]),
                  f"overlap {ov['id']}: {a} x {b} merges cleanly and both survive")

    # 6 ------------------------------------------------------------------
    plans = {}
    for arm in "abc":
        root = scratch / f"arm-{arm}"
        r = run(["sh", str(HERE / f"arm_{arm}_setup.sh"), str(root)], None)
        check(r.returncode == 0, f"arm_{arm}_setup.sh exits 0")
        if r.returncode != 0:
            print(r.stdout)
            continue
        plan = json.loads((root / "launch_plan.json").read_text())
        plans[arm] = plan
        ok = (len(plan["slots"]) == 4 and sorted(sum((s["tasks"] for s in plan["slots"]), [])) == sorted(tasks)
              and all(Path(s["directory"]).is_dir() and s["prompt"] and s["agent_command"] for s in plan["slots"]))
        check(ok, f"arm {arm}: launch_plan has 4 slots, 12 tasks, existing dirs, prompts, commands")
        check(all(all(tasks[t]["title"] in s["prompt"] for t in s["tasks"]) for s in plan["slots"]),
              f"arm {arm}: every prompt carries its three task titles")
        check(suite(plan["repo"]), f"arm {arm}: fresh repo suite green")
    if "c" in plans:
        pc = plans["c"]
        check(all("aethyme broker submit" in s["prompt"] and "aethyme broker finish" in s["prompt"]
                  and "AETHYME_WORKTREE_ROOT=" in s["agent_command"] for s in pc["slots"]),
              "arm c: prompts use start/submit/finish; commands pin AETHYME_WORKTREE_ROOT")
    if "b" in plans:
        check(all("--worktree" in s["agent_command"] for s in plans["b"]["slots"]),
              "arm b: every agent command uses the native --worktree option")
    for arm in "ab":
        if arm in plans:
            check(all("pr open" in s["prompt"] and "pr wait" in s["prompt"] for s in plans[arm]["slots"]),
                  f"arm {arm}: prompts describe the PR / merge-queue flow")

    # 7 ------------------------------------------------------------------
    if "a" in plans:
        pa = plans["a"]
        root = Path(pa["root"])
        pr = root / "bin" / "pr"
        by_pos = sorted(spec["tasks"], key=lambda t: (t["position"], t["slot"]))
        slot_dir = {s["slot"]: s["directory"] for s in pa["slots"]}
        # Every slot branches from base for its task; all PRs are opened, then drained in order.
        for task in by_pos:
            wt = slot_dir[task["slot"]]
            run([GIT, "switch", "-q", "-C", f"work/{task['slot']}-{task['id']}", "origin/main"], wt)
            run([GIT, "config", "user.email", "t@bench.invalid"], wt)
            apply(wt, f"{task['id']}.patch", task["id"])
            check(run([str(pr), "open", task["id"]], wt).returncode == 0, f"arm a sim: pr open {task['id']}")
        d = run([sys.executable, str(root / "bin" / "merge_queue.py"), "--arm-root", str(root), "drain"], None)
        log = [json.loads(line) for line in (root / "merge_log.jsonl").read_text().splitlines()]
        results = {r["task"]: r["result"] for r in log}
        print("     merge queue verdicts:", results)
        check(results.get("t04") == "conflict", "arm a sim: t04 conflicts with t01 in the queue")
        check(results.get("t08") == "merged_main_red", "arm a sim: t08 lands after t05 and turns main red")
        check(results.get("t03") == "merged" or results.get("t09") in ("merged", "merged_main_red"),
              "arm a sim: shared-file pair lands without a conflict")
        w = run([str(pr), "wait", "t04", "--timeout", "5"], slot_dir["s2"])
        check(w.returncode == 3, "arm a sim: pr wait t04 exits 3 (conflict)")

        tele = scratch / "telemetry-a"
        for i, s in enumerate(pa["slots"]):
            sd = tele / s["slot"] / "native"
            sd.mkdir(parents=True)
            (sd.parent / "run1.run.json").write_text(json.dumps({
                "id": f"RUN-{i}", "provider": s["harness"], "turnCount": 3, "tokenUsageState": "missing",
                "startedAt": f"2026-10-01T10:0{i}:00Z", "endedAt": f"2026-10-01T10:4{i}:00Z"}))
            (sd.parent / "run1.transcript.json").write_text(json.dumps([
                {"role": "user", "content": "x", "turnIndex": 0, "toolCalls": [], "timestamp": "2026-10-01T10:00:00Z"},
                {"role": "assistant", "content": "y", "turnIndex": 1, "toolCalls": [], "timestamp": "2026-10-01T10:30:00Z"}]))
            if s["harness"] == "claude":
                lines = [{"type": "assistant", "message": {"id": f"m{k}", "usage": {
                    "input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100,
                    "cache_creation_input_tokens": 1}}} for k in (1, 1, 2)]
            else:
                lines = [{"type": "event_msg", "payload": {"type": "token_count", "info": {
                    "last_token_usage": {"total_tokens": 1}, "total_token_usage": {
                        "input_tokens": 500 * k, "cached_input_tokens": 300 * k, "cache_write_input_tokens": 0,
                        "output_tokens": 40 * k, "reasoning_output_tokens": 10 * k, "total_tokens": 540 * k}}}}
                    for k in (1, 2)]
            (sd / "session.jsonl").write_text("\n".join(json.dumps(x) for x in lines) + "\n")
        oplog = root / "operator_log.jsonl"
        oplog.write_text("\n".join(json.dumps(x) for x in [
            {"ts": "2026-10-01T10:00:00Z", "kind": "arm_start"},
            {"ts": "2026-10-01T10:20:00Z", "kind": "intervention", "slot": "s3", "category": "unstick",
             "note": "synthetic"},
            {"ts": "2026-10-01T11:00:00Z", "kind": "arm_end"}]) + "\n")
        res = scorer.score(root, tele)
        h = res["headline"]
        print("     arm a headline:", json.dumps(h))
        check(h["wall_time_s"] == 3600 and h["operator_interventions"] == 1, "score a: wall time and interventions")
        check(h["conflicts_caught_before_merge"] == 1, "score a: one conflict caught before merge (t04)")
        check(h["broken_merges_reaching_main"] >= 1, "score a: replay finds the semantic break on main")
        check(res["gatekeeping"]["broken_merges_logged"] == 1, "score a: merge log agrees (1 red merge)")
        check(h["tasks_passing_hidden"] == 10 and not h["final_main_green"],
              "score a: 10/12 hidden checks pass (t04 rejected, t08 broken), main red")
        tok = res["telemetry"]["per_slot"]
        check(tok["s1"]["tokens"]["total"] == 232 and tok["s1"]["model_calls_native"] == 2,
              "score a: claude native usage deduplicates message ids")
        check(tok["s3"]["tokens"]["total"] == 1080 and tok["s3"]["model_calls_native"] == 2,
              "score a: codex native usage takes the last cumulative total")

    # 8 ------------------------------------------------------------------
    if "c" in plans:
        pc = plans["c"]
        root, repo = Path(pc["root"]), Path(pc["repo"])
        run([GIT, "switch", "-q", "-c", "sim-integration"], repo)
        apply(repo, "t05.patch", "t05")
        run([GIT, "update-ref", "-m", "promote t05", "refs/heads/aethyme/integration", "HEAD"], repo)
        run([GIT, "revert", "--no-edit", "HEAD"], repo)
        apply(repo, "integrated.patch", "integrated")
        run([GIT, "update-ref", "-m", "promote rest", "refs/heads/aethyme/integration", "HEAD"], repo)
        run([GIT, "switch", "-q", "main"], repo)
        (root / "broker_log.jsonl").write_text("\n".join(json.dumps(x) for x in [
            {"start": "2026-10-01T10:00:00Z", "end": "2026-10-01T10:00:01Z", "exit": 0, "cwd": str(repo),
             "argv": ["broker", "start", "--task", "t01", "--json"]},
            {"start": "2026-10-01T10:10:00Z", "end": "2026-10-01T10:10:09Z", "exit": 3, "cwd": str(repo),
             "argv": ["broker", "submit", "--session", "7"]},
            {"start": "2026-10-01T10:12:00Z", "end": "2026-10-01T10:12:09Z", "exit": 4, "cwd": str(repo),
             "argv": ["broker", "submit", "--session", "8"]},
            {"start": "2026-10-01T10:15:00Z", "end": "2026-10-01T10:15:09Z", "exit": 0, "cwd": str(repo),
             "argv": ["broker", "submit", "--session", "7"]},
            {"start": "2026-10-01T10:16:00Z", "end": "2026-10-01T10:16:01Z", "exit": 0, "cwd": str(repo),
             "argv": ["broker", "finish", "--session", "7"]}]) + "\n")
        res = scorer.score(root, None)
        h = res["headline"]
        print("     arm c headline:", json.dumps(h))
        check(h["conflicts_caught_before_merge"] == 2, "score c: refusal (3) and gate failure (4) counted")
        check(h["tasks_passing_hidden"] == 12 and h["overlaps_resolved"] == 3 and h["final_main_green"],
              "score c: integrated state passes all 12 tasks and 3 overlaps")
        check(h["broken_merges_reaching_main"] == 0, "score c: no red state on aethyme/integration")
        check(res["gatekeeping"]["broker_calls"].get("finish") == 1, "score c: broker verbs tallied")
        check(h["tokens_total"] == 0 and h["wall_time_source"] == "missing",
              "score c: missing telemetry degrades to zero/missing, not a crash")

    print(f"\n{len(FAILS)} failure(s)")
    return 1 if FAILS else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    sys.exit(main(sys.argv[1]))
