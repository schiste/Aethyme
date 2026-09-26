#!/usr/bin/env python3
"""Score one arm of the broker benchmark (P5.6).

    score.py --arm-root <root> [--telemetry <dir>] [--operator-log <file>] [--out <score.json>]

Inputs
  <root>/launch_plan.json   written by the arm's setup script (arm, base SHA,
                            landing repo + ref, slots).
  landing ref               A/B: refs/heads/main of <root>/remote.git.
                            C:   refs/heads/aethyme/integration of <root>/repo.
  <root>/merge_log.jsonl    A/B, written by merge_queue.py.
  <root>/broker_log.jsonl   C, written by the bin/aethyme logging wrapper.
  operator log (JSONL)      filled in by the coordinator; one object per line:
      {"ts": "<ISO-8601>", "kind": "arm_start"}
      {"ts": "<ISO-8601>", "kind": "arm_end"}
      {"ts": "<ISO-8601>", "kind": "intervention", "slot": "s2",
       "category": "unstick|answer|resolve|restart|env|other", "note": "..."}
  telemetry dir             one sub-directory per slot (s1..s4):
      <slot>/<name>.run.json          verbatim Chau7 `run_get` result
      <slot>/<name>.transcript.json   verbatim Chau7 `run_transcript` result
                                      (a list of {role, content, turnIndex,
                                      toolCalls, timestamp, ...})
      <slot>/native/*.jsonl           optional: the agent's own session log -
                                      Claude Code ~/.claude/projects/<dir>/<sessionID>.jsonl
                                      or Codex ~/.codex/sessions/.../rollout-*.jsonl
    A slot may hold several runs (a restarted agent); they are summed.

Measures (all in score.json)
  wall_time_s                 arm_start..arm_end from the operator log, else
                              first run start .. last run end in telemetry.
  hidden_checks               per task and per overlap, on the final landing ref.
  conflicts_caught_before_merge
                              A/B: merge-queue verdicts "conflict" + "ci_failed".
                              C:   `broker submit` calls exiting 3 (refused) or
                                   4 (gate failed on the merged tree).
  broken_merges_reaching_main replayed: each successive state of the landing
                              ref (its reflog, else first-parent history) is
                              checked out and the visible suite run; every
                              green -> red step counts (red_states counts all
                              red states). Also reported: the same count from
                              the merge queue's own log (A/B only).
  operator_interventions      count and breakdown from the operator log.
  agent_turns / tokens        per slot; native session logs are preferred
                              (exact per-request usage), then Chau7 run fields.
"""

import argparse
import datetime as _dt
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
GIT = os.environ.get("BENCH_GIT") or ("/usr/bin/git" if Path("/usr/bin/git").exists() else "git")
TEST_COMMAND = ["python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q"]
os.environ["CHAU7_CTO_OPTIM_ACTIVE"] = "1"
os.environ["PYTHONDONTWRITEBYTECODE"] = "1"


def git(*args, cwd, check=True):
    proc = subprocess.run([GIT, *args], cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} in {cwd}: {proc.stderr.strip()}")
    return proc.stdout.strip()


def read_jsonl(path):
    if not path or not Path(path).exists():
        return []
    out = []
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if line:
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def parse_ts(value):
    if not value:
        return None
    return _dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def load_spec():
    sys.path.insert(0, str(HERE))
    from harness import load_tasks  # noqa: E402
    return load_tasks()


# ----------------------------------------------------------------- repo state


class Checkout:
    """A scratch checkout of one commit, removed on exit."""

    def __init__(self, repo, sha):
        self.repo, self.sha = repo, sha

    def __enter__(self):
        # `git archive` reads objects only: no index or ref in the arm's repo moves.
        self.dir = Path(tempfile.mkdtemp(prefix="bench-score-"))
        archive = subprocess.run([GIT, "-C", str(self.repo), "archive", "--format=tar", self.sha],
                                 stdout=subprocess.PIPE, check=True)
        subprocess.run(["tar", "-x", "-C", str(self.dir)], input=archive.stdout, check=True)
        return self.dir

    def __exit__(self, *exc):
        shutil.rmtree(self.dir, ignore_errors=True)


def run_suite(cwd, args=None, timeout=180):
    try:
        proc = subprocess.run(args or TEST_COMMAND, cwd=cwd, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, timeout=timeout)
        return proc.returncode == 0, proc.stdout[-800:]
    except subprocess.TimeoutExpired:
        return False, "timeout"


def transitions(greens):
    """Number of green -> red steps in a sequence of suite verdicts."""
    return sum(1 for prev, cur in zip(greens, greens[1:]) if prev and not cur)


def landing_states(repo, ref, base_sha):
    """Successive tips of the landing ref, oldest first, excluding the base."""
    shas = []
    reflog = subprocess.run([GIT, "-C", str(repo), "reflog", "show", "--format=%H", ref],
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    if reflog.returncode == 0 and reflog.stdout.strip():
        shas = list(reversed(reflog.stdout.split()))
        source = "reflog"
    else:
        shas = list(reversed(git("rev-list", "--first-parent", f"{base_sha}..{ref}", cwd=repo).split()))
        source = "first-parent"
    seen, ordered = set(), []
    for sha in shas:
        if sha not in seen and sha != base_sha:
            seen.add(sha)
            ordered.append(sha)
    return ordered, source


def hidden_checks(checkout_dir, spec):
    results = {}
    names = [(t["id"], t["hidden_check"]) for t in spec["tasks"]]
    names += [(f"overlap:{o['id']}", o["hidden_check"]) for o in spec["overlaps"]]
    for key, filename in names:
        target = checkout_dir / f"_hidden_{Path(filename).stem}.py"
        shutil.copy(HERE / "hidden_checks" / filename, target)
        ok, tail = run_suite(checkout_dir, ["python3", "-m", "unittest", "-q", target.stem])
        target.unlink()
        results[key] = {"pass": ok, **({} if ok else {"tail": tail[-400:]})}
    return results


def score_repo(plan, spec, replay=True):
    landing = plan["landing"]
    repo, ref = landing["repo"], landing["ref"]
    exists = subprocess.run([GIT, "-C", repo, "rev-parse", "--verify", "-q", ref],
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    if exists.returncode != 0:
        return {"landing_ref": ref, "final_sha": None, "error": "landing ref does not exist (nothing landed)",
                "hidden_checks": {}, "tasks_passing": 0, "broken_merges_replayed": 0, "states": []}
    final = exists.stdout.strip()
    with Checkout(repo, final) as d:
        visible_ok, visible_tail = run_suite(d)
        checks = hidden_checks(d, spec)
    states, source = landing_states(repo, ref, plan["base_sha"]) if replay else ([], "skipped")
    replayed = []
    for sha in states:
        with Checkout(repo, sha) as d:
            ok, _ = run_suite(d)
        replayed.append({"sha": sha, "tests_pass": ok})
    task_ids = [t["id"] for t in spec["tasks"]]
    return {
        "landing_ref": ref, "final_sha": final,
        "final_visible_suite_pass": visible_ok, **({} if visible_ok else {"final_visible_tail": visible_tail}),
        "hidden_checks": checks,
        "tasks_passing": sum(1 for t in task_ids if checks[t]["pass"]),
        "overlaps_passing": sum(1 for k, v in checks.items() if k.startswith("overlap:") and v["pass"]),
        "states_source": source, "states": replayed,
        # A broken merge turns main from green to red; later merges onto an
        # already-red main are counted in red_states, not again here.
        "broken_merges_replayed": transitions([True] + [s["tests_pass"] for s in replayed]),
        "red_states": sum(1 for s in replayed if not s["tests_pass"]),
    }


# ----------------------------------------------------------------- logs


def score_merge_log(plan):
    log = read_jsonl(plan["logs"].get("merge"))
    results = [r["result"] for r in log]
    return {
        "source": "merge_log.jsonl",
        "conflicts_caught_before_merge": results.count("conflict") + results.count("ci_failed"),
        "breakdown": {k: results.count(k) for k in sorted(set(results))},
        "broken_merges_logged": transitions([True] + [r["main_green_after"] for r in log if "main_green_after" in r]),
        "merges_onto_red_main_logged": results.count("merged_main_red"),
        "entries": len(log),
    }


def score_broker_log(plan):
    log = read_jsonl(plan["logs"].get("broker"))

    def verb(rec):
        argv = [a for a in rec.get("argv", []) if not a.startswith("-")]
        return argv[1] if len(argv) > 1 and argv[0] == "broker" else (argv[0] if argv else "")

    submits = [r for r in log if verb(r) == "submit"
               and not any(a in ("prepare", "promote", "promotion-record", "status") for a in r["argv"][2:3])]
    codes = [r["exit"] for r in submits]
    return {
        "source": "broker_log.jsonl",
        "conflicts_caught_before_merge": codes.count(3) + codes.count(4),
        "breakdown": {"submit_ok": codes.count(0), "submit_refused_3": codes.count(3),
                      "submit_gate_failed_4": codes.count(4),
                      "submit_other": len(codes) - codes.count(0) - codes.count(3) - codes.count(4)},
        "broker_calls": {v: sum(1 for r in log if verb(r) == v) for v in sorted({verb(r) for r in log})},
        "entries": len(log),
    }


def score_operator(path):
    log = read_jsonl(path)
    interventions = [r for r in log if r.get("kind") == "intervention"]
    by_cat, by_slot = {}, {}
    for r in interventions:
        by_cat[r.get("category", "other")] = by_cat.get(r.get("category", "other"), 0) + 1
        by_slot[r.get("slot", "-")] = by_slot.get(r.get("slot", "-"), 0) + 1
    start = next((parse_ts(r["ts"]) for r in log if r.get("kind") == "arm_start"), None)
    end = next((parse_ts(r["ts"]) for r in reversed(log) if r.get("kind") == "arm_end"), None)
    return {"count": len(interventions), "by_category": by_cat, "by_slot": by_slot,
            "arm_start": start.isoformat() if start else None, "arm_end": end.isoformat() if end else None}


# ----------------------------------------------------------------- telemetry


def native_usage(path):
    """Turns and tokens from a Claude Code session JSONL or a Codex rollout."""
    claude_msgs, codex_last, codex_calls, kind = {}, None, 0, None
    for rec in read_jsonl(path):
        if rec.get("type") == "assistant" and isinstance(rec.get("message"), dict):
            kind = "claude"
            msg = rec["message"]
            mid = msg.get("id") or rec.get("uuid")
            if mid and msg.get("usage"):
                claude_msgs[mid] = msg["usage"]  # blocks of one response repeat the id
        payload = rec.get("payload") if isinstance(rec.get("payload"), dict) else {}
        if rec.get("type") == "event_msg" and payload.get("type") == "token_count":
            kind = "codex"
            info = payload.get("info") or {}
            if info.get("total_token_usage"):
                codex_last = info["total_token_usage"]
            if info.get("last_token_usage"):
                codex_calls += 1
    if kind == "claude":
        tot = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
        for u in claude_msgs.values():
            tot["input"] += u.get("input_tokens", 0) or 0
            tot["output"] += u.get("output_tokens", 0) or 0
            tot["cache_read"] += u.get("cache_read_input_tokens", 0) or 0
            tot["cache_write"] += u.get("cache_creation_input_tokens", 0) or 0
        tot["total"] = sum(tot.values())
        return {"kind": "claude", "model_calls": len(claude_msgs), "tokens": tot}
    if kind == "codex" and codex_last:
        u = codex_last
        tot = {"input": (u.get("input_tokens", 0) or 0) - (u.get("cached_input_tokens", 0) or 0),
               "output": u.get("output_tokens", 0) or 0,
               "cache_read": u.get("cached_input_tokens", 0) or 0,
               "cache_write": u.get("cache_write_input_tokens", 0) or 0,
               "reasoning": u.get("reasoning_output_tokens", 0) or 0}
        tot["total"] = u.get("total_tokens") or (tot["input"] + tot["output"] + tot["cache_read"])
        return {"kind": "codex", "model_calls": codex_calls, "tokens": tot}
    return None


def run_tokens(run):
    """Best-effort token total from a Chau7 run record (field names vary)."""
    if run.get("tokenUsageState") in (None, "missing"):
        return None
    found = {}

    def walk(obj, prefix=""):
        if isinstance(obj, dict):
            for k, v in obj.items():
                walk(v, f"{prefix}{k}.")
        elif isinstance(obj, (int, float)) and "token" in prefix.lower():
            found[prefix.rstrip(".")] = obj

    walk(run)
    total = next((v for k, v in found.items() if k.lower().endswith("totaltokens") or k.lower().endswith("total_tokens")), None)
    return {"fields": found, "total": total if total is not None else sum(found.values())}


def score_telemetry(tdir, slots):
    out, starts, ends = {}, [], []
    for slot in slots:
        sdir = Path(tdir) / slot if tdir else None
        entry = {"runs": 0, "turns_chau7": 0, "assistant_turns_transcript": 0, "model_calls_native": None,
                 "tokens": None, "tokens_source": "missing"}
        if not sdir or not sdir.is_dir():
            out[slot] = entry
            continue
        chau7_tokens = []
        for f in sorted(sdir.glob("*.run.json")):
            run = json.loads(f.read_text())
            entry["runs"] += 1
            entry["turns_chau7"] += int(run.get("turnCount") or 0)
            if run.get("startedAt"):
                starts.append(parse_ts(run["startedAt"]))
            if run.get("endedAt"):
                ends.append(parse_ts(run["endedAt"]))
            t = run_tokens(run)
            if t:
                chau7_tokens.append(t)
        for f in sorted(sdir.glob("*.transcript.json")):
            turns = json.loads(f.read_text())
            if isinstance(turns, dict):
                turns = turns.get("turns", [])
            entry["assistant_turns_transcript"] += sum(1 for t in turns if t.get("role") == "assistant")
            for t in turns:
                if t.get("timestamp"):
                    ends.append(parse_ts(t["timestamp"]))
        natives = [u for u in (native_usage(p) for p in sorted((sdir / "native").glob("*.jsonl"))) if u]
        if natives:
            keys = set().union(*(u["tokens"] for u in natives))
            entry["tokens"] = {k: sum(u["tokens"].get(k, 0) for u in natives) for k in sorted(keys)}
            entry["model_calls_native"] = sum(u["model_calls"] for u in natives)
            entry["tokens_source"] = "native:" + ",".join(sorted({u["kind"] for u in natives}))
        elif chau7_tokens:
            entry["tokens"] = {"total": sum(t["total"] or 0 for t in chau7_tokens)}
            entry["tokens_source"] = "chau7"
        out[slot] = entry
    totals = {
        "turns_chau7": sum(e["turns_chau7"] for e in out.values()),
        "assistant_turns_transcript": sum(e["assistant_turns_transcript"] for e in out.values()),
        "model_calls_native": sum(e["model_calls_native"] or 0 for e in out.values()),
        "tokens_total": sum((e["tokens"] or {}).get("total", 0) for e in out.values()),
        "slots_missing_tokens": [s for s, e in out.items() if e["tokens"] is None],
    }
    window = (min(starts), max(ends)) if starts and ends else (None, None)
    return {"per_slot": out, "totals": totals, "window": window}


# ----------------------------------------------------------------- main


def score(root, telemetry=None, operator_log=None, replay=True):
    root = Path(root).resolve()
    plan = json.loads((root / "launch_plan.json").read_text())
    spec = load_spec()
    repo = score_repo(plan, spec, replay=replay)
    gatekeeping = score_broker_log(plan) if plan["arm"] == "c" else score_merge_log(plan)
    operator = score_operator(operator_log or plan["logs"]["operator"])
    tele = score_telemetry(telemetry, [s["slot"] for s in plan["slots"]])
    if operator["arm_start"] and operator["arm_end"]:
        wall = (parse_ts(operator["arm_end"]) - parse_ts(operator["arm_start"])).total_seconds()
        wall_source = "operator_log"
    elif tele["window"][0]:
        wall = (tele["window"][1] - tele["window"][0]).total_seconds()
        wall_source = "telemetry"
    else:
        wall, wall_source = None, "missing"
    return {
        "benchmark": plan["benchmark"], "arm": plan["arm"], "repetition": plan["repetition"],
        "root": str(root), "scored_at": _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds"),
        "headline": {
            "wall_time_s": wall, "wall_time_source": wall_source,
            "tasks_passing_hidden": repo["tasks_passing"], "tasks_total": len(spec["tasks"]),
            "overlaps_resolved": repo.get("overlaps_passing", 0), "overlaps_total": len(spec["overlaps"]),
            "final_main_green": repo.get("final_visible_suite_pass"),
            "conflicts_caught_before_merge": gatekeeping["conflicts_caught_before_merge"],
            "broken_merges_reaching_main": repo["broken_merges_replayed"],
            "operator_interventions": operator["count"],
            "agent_turns": tele["totals"]["model_calls_native"] or tele["totals"]["assistant_turns_transcript"]
            or tele["totals"]["turns_chau7"],
            "tokens_total": tele["totals"]["tokens_total"],
        },
        "repo": repo, "gatekeeping": gatekeeping, "operator": operator,
        "telemetry": {"per_slot": tele["per_slot"], "totals": tele["totals"]},
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--arm-root", required=True)
    parser.add_argument("--telemetry")
    parser.add_argument("--operator-log")
    parser.add_argument("--out")
    parser.add_argument("--no-replay", action="store_true", help="skip replaying landing-ref states")
    args = parser.parse_args(argv)
    result = score(args.arm_root, args.telemetry, args.operator_log, replay=not args.no_replay)
    text = json.dumps(result, indent=2, default=str) + "\n"
    out = Path(args.out) if args.out else Path(args.arm_root) / "score.json"
    out.write_text(text)
    h = result["headline"]
    print(f"arm {result['arm']} r{result['repetition']}: " + ", ".join(f"{k}={v}" for k, v in h.items()))
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
