#!/usr/bin/env python3
"""Run the frozen FX08 fixed-file pilot and archive per-run evidence."""
from __future__ import annotations
import concurrent.futures
import hashlib
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONFIG = json.loads((ROOT / "pilot-config.json").read_text())
TASK_IDS = CONFIG["pilot"]["task_ids"]
VARIANTS = CONFIG["pilot"]["variants"]
REPEATS = CONFIG["pilot"]["repeats"]
RUNNER = Path(os.environ.get("FX08_RUNNER", CONFIG["runner"]["script"])).resolve()
PLAYGROUND_ROOT = Path(os.environ.get("FX08_PLAYGROUND_ROOT", CONFIG["execution"]["target_root"])).resolve()
RAW_ROOT = Path(os.environ.get("FX08_RAW_ROOT", CONFIG["execution"]["raw_artifact_root"])).resolve()
EVIDENCE_JSON = ROOT / "evidence" / "fx08-brief-usefulness-pilot.json"
EVIDENCE_MD = ROOT / "evidence" / "fx08-brief-usefulness-pilot.md"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def freeze_revision() -> str:
    tag = CONFIG["execution"]["freeze_tag"]
    value = subprocess.run(["git", "-C", str(ROOT), "rev-parse", f"{tag}^{{}}"], check=True, capture_output=True, text=True)
    return value.stdout.strip()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")
    temp.replace(path)


def model_from_events(events_path: Path):
    found = set()
    if not events_path.exists():
        return None
    for line in events_path.read_text(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        def visit(value):
            if isinstance(value, dict):
                for key, child in value.items():
                    if key.lower() in {"model", "model_name", "model_version"} and isinstance(child, str):
                        found.add(child)
                    visit(child)
            elif isinstance(value, list):
                for child in value:
                    visit(child)
        visit(event)
    return sorted(found) if found else None


def access_evidence(events_path: Path, task_id: str, variant: str):
    handoff_path = ROOT / "tasks" / task_id / "handoffs" / f"{variant}.md"
    handoff = handoff_path.read_text()
    if variant == "absent":
        marker = "No decision brief was provided."
    else:
        brief = json.loads((ROOT / "tasks" / task_id / "briefs" / f"{variant}.json").read_text())
        marker = brief["decisions"][0]["choice"]
    evidence = []
    if events_path.exists():
        for line in events_path.read_text(errors="replace").splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            item = event.get("item") or {}
            if item.get("type") != "command_execution" or event.get("type") != "item.completed":
                continue
            if item.get("exit_code") != 0:
                continue
            command = str(item.get("command", ""))
            output = str(item.get("aggregated_output", ""))
            if "HANDOFF.md" in command and marker in output:
                evidence.append({"command": command, "matched_marker": marker})
    return {"read": bool(evidence), "evidence": evidence[:3], "expected_marker": marker}


def visible_messages(events_path: Path, final_message: str):
    messages = []
    if events_path.exists():
        for line in events_path.read_text(errors="replace").splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            item = event.get("item") or {}
            if item.get("type") == "agent_message" and isinstance(item.get("text"), str):
                messages.append(item["text"])
    if final_message:
        messages.append(final_message)
    return messages


def quote_harm(messages):
    lines = []
    for message in messages:
        for line in message.splitlines():
            low = line.lower()
            if ("handoff" in low or "brief" in low) and any(word in low for word in ("because", "since", "based on", "says", "suggests", "decision", "reason")):
                if line not in lines:
                    lines.append(line.strip())
    return lines[:3]


def run_one(job, frozen):
    task_id, variant, repeat = job
    name = f"pilot-{task_id}-{variant}-r{repeat}"
    repo = PLAYGROUND_ROOT / task_id / variant / f"r{repeat}" / "repo"
    raw = RAW_ROOT / name
    if repo.exists() or raw.exists():
        raise RuntimeError(f"refusing to overwrite prior run: {name}")
    repo.parent.mkdir(parents=True)
    shutil.copytree(ROOT / "tasks" / task_id / "a-state", repo)
    (repo / "HANDOFF.md").write_text((ROOT / "tasks" / task_id / "handoffs" / f"{variant}.md").read_text())
    (repo / "TASK.md").write_text((ROOT / "tasks" / task_id / "follow-up-prompt.md").read_text())
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    subprocess.run(["git", "-c", "user.name=FX08 Pilot", "-c", "user.email=fx08@example.invalid", "commit", "-q", "-m", "test: add frozen synthetic UI base"], cwd=repo, check=True)

    prompt = (ROOT / CONFIG["execution"]["runner_prompt_file"]).read_text().strip()
    env = os.environ.copy()
    env.update({
        "AETHYME_EVAL_ARM": "control",
        "AETHYME_EVAL_REPO": str(repo),
        "AETHYME_EVAL_ARTIFACT_DIR": str(raw),
        "AETHYME_EVAL_PROMPT": prompt,
        "AETHYME_PLAYGROUND_ROOTS": str(PLAYGROUND_ROOT),
        "AETHYME_EVAL_TASK_CLASS": "brief-usefulness",
    })
    env.pop("AETHYME_EVAL_FIXTURE_ID", None)
    env["AETHYME_EVAL_OUTPUT_SCHEMA_FILE"] = str((ROOT / CONFIG["execution"]["output_schema_file"]).resolve())
    cli_version_proc = subprocess.run(["codex", "--version"], cwd=repo, env=env, capture_output=True, text=True)
    version_lines = (cli_version_proc.stdout + "\n" + cli_version_proc.stderr).splitlines()
    cli_version = next((line.strip() for line in version_lines if line.strip().startswith("codex-cli ")), None)
    started = time.monotonic()
    proc = subprocess.run([sys.executable, str(RUNNER)], cwd=repo, env=env, capture_output=True, text=True)
    process_wall = round(time.monotonic() - started, 3)
    raw.mkdir(parents=True, exist_ok=True)
    (raw / "runner.stdout.txt").write_text(proc.stdout)
    (raw / "driver.stderr.log").write_text(proc.stderr)
    runner_result = None
    for line in reversed(proc.stdout.splitlines()):
        try:
            runner_result = json.loads(line)
            break
        except json.JSONDecodeError:
            continue

    events = raw / "events.jsonl"
    agent_started = False
    if events.exists():
        for line in events.read_text(errors="replace").splitlines():
            try:
                if json.loads(line).get("type") == "thread.started":
                    agent_started = True
                    break
            except json.JSONDecodeError:
                pass
    if agent_started:
        oracle_env = os.environ.copy()
        oracle_env["FX08_PLAYWRIGHT_MODULE"] = CONFIG["execution"]["playwright_oracle"]["module_path"]
        oracle_env["FX08_PLAYWRIGHT_VERSION"] = CONFIG["execution"]["playwright_oracle"]["module_version"]
        oracle_env["FX08_BROWSER_EXECUTABLE"] = CONFIG["execution"]["playwright_oracle"]["browser_executable"]
        oracle_env["FX08_BROWSER_VERSION"] = CONFIG["execution"]["playwright_oracle"]["browser_version"]
        oracle_proc = subprocess.run(
            ["node", str(ROOT / "oracles" / "behavior_oracle.mjs"), "--task", task_id, "--repo", str(repo)],
            cwd=ROOT, env=oracle_env, capture_output=True, text=True,
        )
        try:
            oracle = json.loads(oracle_proc.stdout.strip().splitlines()[-1])
        except (IndexError, json.JSONDecodeError):
            oracle = {"error": oracle_proc.stderr or oracle_proc.stdout, "exit_code": oracle_proc.returncode}
    else:
        oracle = {"status": "not_evaluated_runner_failed_before_agent_start", "task_pass": None, "decision_survived": None}
    (raw / "oracle.json").write_text(json.dumps(oracle, indent=2, ensure_ascii=False) + "\n")

    access = access_evidence(events, task_id, variant)
    final_message = (runner_result or {}).get("final_output_message") or ""
    messages = visible_messages(events, final_message)
    harm = None
    if variant == "misleading":
        broken = oracle.get("decision_survived") is False
        quotes = quote_harm(messages) if broken else []
        harm = {
            "decision_broken": broken,
            "explicitly_attributed_to_brief": bool(quotes),
            "visible_quotes": quotes,
            "causation_note": "A false decision-survival result is an observed break. Causation is only marked explicit when a visible agent message links it to HANDOFF/brief; hidden reasoning is unavailable.",
        }

    turns = 0
    if events.exists():
        for line in events.read_text(errors="replace").splitlines():
            try:
                if json.loads(line).get("type") == "turn.completed":
                    turns += 1
            except json.JSONDecodeError:
                pass
    usage = runner_result or {}
    uncached = usage.get("uncached_input_tokens")
    output_tokens = usage.get("output_tokens")
    total_cost_tokens = (uncached + output_tokens) if isinstance(uncached, (int, float)) and isinstance(output_tokens, (int, float)) else usage.get("uncached_plus_output_tokens")
    result = {
        "task_id": task_id,
        "variant": variant,
        "repeat": repeat,
        "runner_version": {
            "aethyme_runner_script_sha256": CONFIG["runner"]["script_sha256"],
            "aethyme_runner_source_revision": CONFIG["runner"]["source_repository_revision"],
            "codex_cli": cli_version,
        },
        "model_version": model_from_events(events),
        "agent_started": agent_started,
        "frozen_revision": frozen,
        "oracle": {
            "task_pass": oracle.get("task_pass"),
            "decision_survived": oracle.get("decision_survived"),
            "checks": oracle.get("checks"),
            "application_errors": oracle.get("application_errors"),
            "raw": oracle,
        },
        "access": access,
        "cost": {
            "wall_time_seconds": (runner_result or {}).get("wall_time_seconds", process_wall),
            "uncached_input_tokens": uncached,
            "output_tokens": output_tokens,
            "uncached_input_plus_output_tokens": total_cost_tokens,
            "retries": usage.get("retries"),
            "turns": turns,
        },
        "harm": harm,
        "runner_exit_code": proc.returncode,
        "runner_error": (runner_result or {}).get("error") or (proc.stderr.strip() if proc.returncode != 0 else None),
        "runner_stderr": proc.stderr.strip(),
        "artifact_leakage": (runner_result or {}).get("artifact_leakage"),
        "artifact_dir": str(raw),
        "target_repo": str(repo),
        "final_message": final_message,
    }
    return result


def summarize(runs):
    variants = {}
    for variant in VARIANTS:
        rows = [row for row in runs if row["variant"] == variant]
        def rate(predicate):
            values = [predicate(row) for row in rows if predicate(row) is not None]
            return round(sum(values) / len(values), 4) if values else None
        walls = [row["cost"]["wall_time_seconds"] for row in rows if isinstance(row["cost"]["wall_time_seconds"], (int, float))]
        tokens = [row["cost"]["uncached_input_plus_output_tokens"] for row in rows if isinstance(row["cost"]["uncached_input_plus_output_tokens"], (int, float))]
        variants[variant] = {
            "runs": len(rows),
            "agent_runs": sum(1 for row in rows if row.get("agent_started") is True),
            "task_pass_rate": rate(lambda row: row["oracle"]["task_pass"]),
            "decision_survival_rate": rate(lambda row: row["oracle"]["decision_survived"]),
            "handoff_access_rate": rate(lambda row: row["access"]["read"]),
            "median_wall_time_seconds": statistics.median(walls) if walls else None,
            "median_uncached_input_plus_output_tokens": statistics.median(tokens) if tokens else None,
        }
    return variants


def summarize_tasks(runs):
    tasks = {}
    for task_id in TASK_IDS:
        rows = [row for row in runs if row["task_id"] == task_id]
        def rate(key):
            values = [row["oracle"].get(key) for row in rows if row["oracle"].get(key) is not None]
            return round(sum(values) / len(values), 4) if values else None
        tasks[task_id] = {
            "runs": len(rows),
            "agent_runs": sum(1 for row in rows if row.get("agent_started") is True),
            "task_pass_rate": rate("task_pass"),
            "decision_survival_rate": rate("decision_survived"),
            "handoff_access_rate": round(sum(1 for row in rows if row["access"]["read"]) / len(rows), 4) if rows else None,
        }
    return tasks


def manifest_errors():
    manifest_path = ROOT / "freeze-manifest.sha256"
    entries = {}
    for line in manifest_path.read_text().splitlines():
        try:
            digest, rel = line.split("  ", 1)
        except ValueError:
            return [f"malformed manifest line: {line}"]
        entries[rel] = digest
    ignored_outputs = {
        "evidence/fx08-brief-usefulness-pilot.json",
        "evidence/fx08-brief-usefulness-pilot.md",
    }
    actual = set()
    for path in ROOT.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(ROOT).as_posix()
        if rel == "freeze-manifest.sha256" or rel in ignored_outputs:
            continue
        if "__pycache__" in path.parts or path.suffix == ".pyc" or ".git" in path.parts:
            continue
        actual.add(rel)
    errors = []
    for rel in sorted(set(entries) - actual):
        errors.append(f"manifest file missing: {rel}")
    for rel in sorted(actual - set(entries)):
        errors.append(f"unmanifested file: {rel}")
    for rel in sorted(actual & set(entries)):
        if sha256(ROOT / rel) != entries[rel]:
            errors.append(f"manifest digest mismatch: {rel}")
    return errors


def frozen_repo_errors(frozen):
    head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
    if head != frozen:
        return [f"HEAD {head} differs from frozen tag {frozen}"]
    status = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain"], check=True, capture_output=True, text=True).stdout
    errors = manifest_errors()
    if status.strip():
        errors.append("frozen package has tracked or unignored changes")
    return errors


def write_report(evidence):
    variants = evidence["pilot_summary"]
    tasks = evidence["task_summary"]
    lines = [
        f"# {CONFIG['suite']} — {CONFIG['version']}",
        "",
        f"Frozen revision {evidence['frozen_revision']}. This fixed-file handoff pilot does not close D30 because it does not use L3 retrieval.",
        "",
        "| Variant | Agent runs / attempts | Decision survived | Task passed | HANDOFF read | Median wall time | Median uncached + output tokens |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for variant in VARIANTS:
        row = variants[variant]
        def pct(key):
            val = row[key]
            return "n/a" if val is None else f"{val * 100:.1f}%"
        wall = "n/a" if row["median_wall_time_seconds"] is None else f"{row['median_wall_time_seconds']:.1f}s"
        tok = "n/a" if row["median_uncached_input_plus_output_tokens"] is None else str(round(row["median_uncached_input_plus_output_tokens"]))
        lines.append(f"| {variant} | {row['agent_runs']}/{row['runs']} | {pct('decision_survival_rate')} | {pct('task_pass_rate')} | {pct('handoff_access_rate')} | {wall} | {tok} |")
    lines.extend([
        "",
        "## Task-level outcomes",
        "",
        "| Task | Agent runs | Decision survived | Task passed | HANDOFF read |",
        "|---|---:|---:|---:|---:|",
    ])
    for task_id in TASK_IDS:
        row = tasks[task_id]
        def task_pct(key):
            return "n/a" if row[key] is None else f"{row[key] * 100:.1f}%"
        lines.append(f"| {task_id} | {row['agent_runs']}/{row['runs']} | {task_pct('decision_survival_rate')} | {task_pct('task_pass_rate')} | {task_pct('handoff_access_rate')} |")

    harms = [row for row in evidence["runs"] if row.get("harm") and row["harm"].get("decision_broken")]
    misleading_runs = sum(1 for row in evidence["runs"] if row["variant"] == "misleading")
    flagged = sum(1 for row in evidence["runs"] if row.get("runner_exit_code") == 3)
    pass_rates = [(task_id, tasks[task_id]["task_pass_rate"]) for task_id in TASK_IDS if tasks[task_id]["task_pass_rate"] is not None]
    if pass_rates:
        low_rate = min(value for _, value in pass_rates)
        high_rate = max(value for _, value in pass_rates)
        low_tasks = ", ".join(task_id for task_id, value in pass_rates if value == low_rate)
        high_tasks = ", ".join(task_id for task_id, value in pass_rates if value == high_rate)
        observations = [f"Task completion ranged from {low_rate * 100:.1f}% ({low_tasks}) to {high_rate * 100:.1f}% ({high_tasks}); with three repeats per task-condition, these are descriptive counts, not significance claims."]
    else:
        observations = ["No task-completion observations were available."]
    baseline = variants.get("absent", {})
    for variant in ("correct", "incomplete", "misleading"):
        tested = variants.get(variant, {})
        base_rate = baseline.get("task_pass_rate")
        tested_rate = tested.get("task_pass_rate")
        if base_rate is not None and tested_rate is not None:
            difference = (tested_rate - base_rate) * 100
            observations.append(f"Observed task-pass difference for {variant} versus absent: {difference:+.1f} percentage points; this small pilot is descriptive and does not establish causation.")
    if harms:
        observations.append(f"{len(harms)} decision break(s) occurred among misleading briefs; attribution quotes, when visible, are recorded per run.")
    else:
        observations.append(f"No decision break was observed in {misleading_runs} misleading-brief runs.")
    if flagged:
        observations.append(f"The runner leakage gate returned code 3 for {flagged} runs; leakage details are retained per run.")
    else:
        observations.append("The runner leakage gate did not return code 3.")
    lines.extend(["", "## Observed patterns", ""])
    lines.extend(f"- {item}" for item in observations)

    lines.extend([
        "",
        "## Protocol limits",
        "",
        "- H01 and H02 were excluded because they and prior outcomes were exposed; 8 held-out runs are missing.",
        "- The v14 freeze precedes this rerun, but not the earlier agent runs. It is not a first-ever freeze; v13 and earlier evidence remains preserved, and v13 scores are superseded because its oracle missed written behaviors.",
        "- Model identifiers are recorded only if present in each event log; the runner does not otherwise expose them.",
        "- Raw run artifacts are retained outside this Playground repository under the per-run paths in the JSON.",
        "- This is a fixed-file pilot, not a run through L3 retrieval, so it cannot close D30.",
        "- The no-brief condition keeps the required exact text, so it is inherently much shorter than the three actual decision briefs. Correct, incomplete and misleading brief token counts are recorded in validation/brief-validation.json; length remains a condition-level limitation.",
        "- The runner inherits the same host CODEX_HOME for all arms while ignoring user config. Any generated-artifact leakage is reported per run; these outcomes are retained rather than filtered.",
        "",
        "## Misleading-brief harm",
        "",
        f"Decision break observed in {len(harms)} of {misleading_runs} misleading-brief runs.",
    ])
    for row in harms:
        lines.append(f"- {row['task_id']} repeat {row['repeat']}: explicit attribution={str(row['harm']['explicitly_attributed_to_brief']).lower()}; quotes={json.dumps(row['harm']['visible_quotes'], ensure_ascii=False)}")
    EVIDENCE_MD.parent.mkdir(parents=True, exist_ok=True)
    EVIDENCE_MD.write_text("\n".join(lines) + "\n")


def main():
    if "--preflight" in sys.argv:
        frozen = freeze_revision()
        schema = ROOT / CONFIG["execution"]["output_schema_file"]
        schema_data = json.loads(schema.read_text())
        assert CONFIG["execution"]["freeze_tag"].startswith("fx08-v")
        assert schema_data.get("type") == "object" and "summary" in schema_data.get("required", [])
        assert len(TASK_IDS) * len(VARIANTS) * len(REPEATS) == 48
        errors = frozen_repo_errors(frozen)
        if errors:
            raise RuntimeError("frozen package preflight failed: " + "; ".join(errors))
        print(json.dumps({"frozen_revision": frozen, "freeze_tag": CONFIG["execution"]["freeze_tag"], "freeze_manifest_sha256": sha256(ROOT / "freeze-manifest.sha256"), "schema_sha256": sha256(schema), "planned_runs": 48, "agent_invoked": False, "working_tree_clean": True, "manifest_verified": True}, indent=2))
        return 0
    if "--dry-run" in sys.argv:
        jobs = [(task, variant, repeat) for task in TASK_IDS for variant in VARIANTS for repeat in REPEATS]
        random.Random(CONFIG["execution"]["shuffle_seed"]).shuffle(jobs)
        print(json.dumps({"runs": len(jobs), "first_jobs": jobs[:5], "target_root": str(PLAYGROUND_ROOT), "raw_root": str(RAW_ROOT)}, indent=2))
        return 0
    frozen = freeze_revision()
    errors = frozen_repo_errors(frozen)
    if errors:
        raise RuntimeError("frozen package changed after tag: " + "; ".join(errors))
    jobs = [(task, variant, repeat) for task in TASK_IDS for variant in VARIANTS for repeat in REPEATS]
    random.Random(CONFIG["execution"]["shuffle_seed"]).shuffle(jobs)
    evidence = {
        "suite": CONFIG["suite"],
        "version": CONFIG["version"],
        "frozen_revision": frozen,
        "freeze_tag": CONFIG["execution"]["freeze_tag"],
        "freeze_precedes_current_rerun": True,
        "freeze_precedes_first_ever_run": False,
        "runner_script_sha256": CONFIG["runner"]["script_sha256"],
        "oracle_browser": CONFIG["execution"]["playwright_oracle"],
        "model_version": None,
        "limitations": [
            "The current freeze precedes this rerun but follows the earlier 56-run series; the prior archive cannot satisfy first-ever freeze ordering.",
            "H01 and H02 and their outcomes were already exposed; no held-out runs are claimed, leaving an 8-run shortfall.",
            "This uses fixed-file HANDOFF delivery rather than L3 retrieval and therefore does not close D30.",
            "The runner JSONL does not expose a model identifier; model_version is null unless run events provide one.",
            "The control environment can still read user-scoped CODEX_HOME files; v13 agents triggered the runner leak gate by printing a global Playwright skill path. V14 records any such gate results per run.",
        ],
        "pilot_summary": {},
        "task_summary": {},
        "heldout_summary": {"planned_runs": 8, "completed_runs": 0, "shortfall": 8, "reason": "prior exposure invalidates H01/H02 as held-out tasks"},
        "runs": [],
    }
    evidence["pilot_summary"] = summarize(evidence["runs"])
    evidence["task_summary"] = summarize_tasks(evidence["runs"])
    write_json(EVIDENCE_JSON, evidence)
    RAW_ROOT.mkdir(parents=True, exist_ok=True)
    PLAYGROUND_ROOT.mkdir(parents=True, exist_ok=True)
    max_workers = int(CONFIG["execution"]["parallel_workers"])
    failures = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as pool:
        futures = {pool.submit(run_one, job, frozen): job for job in jobs}
        for future in concurrent.futures.as_completed(futures):
            job = futures[future]
            try:
                row = future.result()
            except Exception as exc:
                failures.append({"job": job, "error": repr(exc)})
                failed = {
                    "task_id": job[0], "variant": job[1], "repeat": job[2],
                    "runner_version": None, "model_version": None, "agent_started": False,
                    "frozen_revision": frozen,
                    "oracle": {"task_pass": None, "decision_survived": None, "status": "driver_failed"},
                    "access": {"read": False, "evidence": []},
                    "cost": {"wall_time_seconds": None, "uncached_input_tokens": None, "output_tokens": None, "uncached_input_plus_output_tokens": None, "retries": None, "turns": 0},
                    "harm": None, "runner_exit_code": None, "runner_error": repr(exc),
                    "artifact_dir": None, "target_repo": None, "final_message": None,
                }
                evidence["runs"].append(failed)
                evidence["runs"].sort(key=lambda row: (row["task_id"], row["variant"], row["repeat"]))
                evidence["pilot_summary"] = summarize(evidence["runs"])
                evidence["task_summary"] = summarize_tasks(evidence["runs"])
                evidence["failures"] = failures
                write_json(EVIDENCE_JSON, evidence)
                print(json.dumps({"completed": len(evidence["runs"]), "job": job, "error": repr(exc)}), flush=True)
                continue
            evidence["runs"].append(row)
            evidence["runs"].sort(key=lambda row: (row["task_id"], row["variant"], row["repeat"]))
            evidence["pilot_summary"] = summarize(evidence["runs"])
            evidence["task_summary"] = summarize_tasks(evidence["runs"])
            evidence["failures"] = failures
            write_json(EVIDENCE_JSON, evidence)
            print(json.dumps({"completed": len(evidence["runs"]), "job": job, "agent_started": row["agent_started"], "runner_exit_code": row["runner_exit_code"], "task_pass": row["oracle"]["task_pass"], "decision_survived": row["oracle"]["decision_survived"], "access": row["access"]["read"]}), flush=True)
    evidence["failures"] = failures
    evidence["pilot_summary"] = summarize(evidence["runs"])
    evidence["task_summary"] = summarize_tasks(evidence["runs"])
    write_json(EVIDENCE_JSON, evidence)
    write_report(evidence)
    all_agents_completed = all(row.get("agent_started") is True and row.get("runner_exit_code") == 0 for row in evidence["runs"])
    return 0 if len(evidence["runs"]) == len(jobs) and not failures and all_agents_completed else 1

if __name__ == "__main__":
    raise SystemExit(main())
