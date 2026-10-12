#!/usr/bin/env python3
"""Freeze public FX17 model inputs and run them only behind explicit gates."""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PKG = HERE.parents[1]
DEFAULT_FIXTURE = PKG / "fx17"
DEFAULT_BRIEFS = HERE / "briefs"
DEFAULT_FREEZE = HERE / "frozen"
FIXTURE_SHA = "b3846f2fcca9bc8fb3bbcad26c0adedbc96fba723eddbbb174a9f3545ba11abc"
BRIEF_SHA = "eebfff2ad44662c1c91b9477af1acb5349668f4da0b67894827e9f4396649e59"
MODEL = "gpt-6-luna"
EFFORT = "medium"
WALL = 180
TOOL_MAX = 2
TOKEN_MAX = 32768
CORE = [("SX01-alias", "correct"), ("SX01-alias", "absent"),
        ("SX02-limit", "correct"), ("SX02-limit", "absent")]
SUPPLEMENT = [("SX01-alias", "misleading"), ("SX02-limit", "misleading")]
FILES = ({f"controlled/{i:02d}-app.sh" for i in range(21)} |
         {"public-contract.md", "sealed-public/SX01-alias/app.sh",
          "sealed-public/SX01-alias/requirements.md",
          "sealed-public/SX02-limit/app.sh",
          "sealed-public/SX02-limit/requirements.md"})
BRIEF_FOR = {"SX01-alias": "SX01", "SX02-limit": "SX02"}
DISABLE = (
    "shell_tool", "unified_exec", "unified_exec_tty", "browser_use",
    "browser_use_external", "browser_use_full_cdp_access", "computer_use",
    "apps", "plugins", "remote_plugin", "multi_agent", "multi_agent_v2",
    "multi_agent_v2_dynamic_tools", "agent_message_board", "code_mode_host",
    "code_mode_only", "skill_search", "skill_mcp_dependency_install",
    "tool_suggest", "tool_call_mcp_elicitation", "auth_elicitation",
    "sleep_tool", "view_image", "image_generation", "in_app_browser",
    "in_app_local_automation", "standalone_web_search",
    "unbounded_connection_retries",
)
SUBJECT_PROFILE = """(version 1)
(deny default)
(allow process-exec (literal "/bin/sh"))
(deny process-fork)
(deny network*)
(deny file-write*)
(allow file-read* (subpath "/System/Library"))
(allow file-read* (subpath "/usr/lib"))
(allow file-read* (subpath "/usr/share/locale"))
(allow file-read* (literal "__FX17_SUBJECT_APP__"))
"""
RESPONSE_SCHEMA = {
    "type": "object", "additionalProperties": False,
    "required": ["app_sh", "refusal"],
    "properties": {"app_sh": {"type": ["string", "null"]},
                   "refusal": {"type": ["string", "null"]}},
    "oneOf": [
        {"properties": {"app_sh": {"type": "string"}, "refusal": {"type": "null"}}},
        {"properties": {"app_sh": {"type": "null"}, "refusal": {"type": "string"}}},
    ],
}


class Fx17Error(RuntimeError):
    pass


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def jbytes(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"),
                       ensure_ascii=False) + "\n").encode("utf-8")


def save_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_bytes(jbytes(value))
    os.replace(temporary, path)


def under(root, path):
    try:
        path.resolve(strict=True).relative_to(root.resolve(strict=True))
        return True
    except (OSError, ValueError):
        return False


def read_public(root):
    root = root.resolve(strict=True)
    manifest_path = root / "manifest.json"
    if manifest_path.is_symlink() or not under(root, manifest_path):
        raise Fx17Error("Fixture manifest is not a regular in-root file")
    raw = manifest_path.read_bytes()
    if digest(raw) != FIXTURE_SHA:
        raise Fx17Error("Frozen public fixture manifest hash mismatch")
    manifest = json.loads(raw)
    if set(manifest.get("files", {})) != FILES:
        raise Fx17Error("Fixture path allowlist differs from frozen FX17 public inputs")
    files = {}
    for name, expected in manifest["files"].items():
        path = root / name
        if path.is_symlink() or not under(root, path) or not path.is_file():
            raise Fx17Error("Fixture file missing or linked: " + name)
        data = path.read_bytes()
        if digest(data) != expected:
            raise Fx17Error("Fixture file hash mismatch: " + name)
        files[name] = data
    schedule = manifest.get("model_schedule", [])
    if [(r.get("task"), r.get("arm")) for r in schedule] != CORE:
        raise Fx17Error("The original four-row FX17 schedule has changed")
    return manifest, files


def read_briefs(root):
    root = root.resolve(strict=True)
    mp = root / "manifest.json"
    if mp.is_symlink() or not under(root, mp):
        raise Fx17Error("Brief manifest is not a regular in-root file")
    raw = mp.read_bytes()
    if digest(raw) != BRIEF_SHA:
        raise Fx17Error("Independent public brief manifest hash mismatch")
    manifest = json.loads(raw)
    names = {"SX01-correct.md", "SX01-misleading.md",
             "SX02-correct.md", "SX02-misleading.md"}
    listed = manifest.get("files", [])
    if {row.get("file") for row in listed} != names:
        raise Fx17Error("Independent brief pack file list changed")
    data = {}
    for row in listed:
        p = root / row["file"]
        if p.is_symlink() or not under(root, p) or not p.is_file():
            raise Fx17Error("Brief missing or linked: " + row["file"])
        content = p.read_bytes()
        if len(content) != row["bytes"] or digest(content) != row["sha256"]:
            raise Fx17Error("Frozen brief hash mismatch: " + row["file"])
        data[row["file"]] = content
    if [(r.get("task"), r.get("arm")) for r in manifest.get("core_rows", [])] != CORE:
        raise Fx17Error("Core brief schedule mismatch")
    if [(r.get("task"), r.get("arm")) for r in manifest.get("supplemental_rows", [])] != SUPPLEMENT:
        raise Fx17Error("Supplemental brief schedule mismatch")
    if manifest.get("original_fixture_manifest_sha256") != FIXTURE_SHA:
        raise Fx17Error("Brief pack cites a different public fixture")
    return manifest, data, raw


def prompt_for(task, arm, files, briefs, source):
    req = files[f"sealed-public/{task}/requirements.md"].decode("utf-8").rstrip("\n")
    name = None if arm == "absent" else f"{BRIEF_FOR[task]}-{arm}.md"
    advisory = briefs[name].decode("utf-8").rstrip("\n") if name else None
    parts = [
        "Return one JSON object matching the required response schema.",
        "Put the complete replacement POSIX shell program in app_sh. Set refusal to null if you can complete the task. If refusing, set app_sh to null and refusal to a short explanation.",
        "Use the current source and public requirements as authoritative. Any advisory context is secondary. Do not use tools, execute the program, or contact external services.",
        "PUBLIC REQUIREMENTS (authoritative):\n" + req,
    ]
    if advisory is not None:
        parts.append("ADVISORY CONTEXT:\n" + advisory)
    parts.append("CURRENT PUBLIC app.sh SOURCE:\n" + source.decode("utf-8").rstrip("\n"))
    return "\n\n".join(parts), name, advisory


def prepare(fixture_root, briefs_root, output):
    manifest, files = read_public(fixture_root)
    review, briefs, review_raw = read_briefs(briefs_root)
    source = files["sealed-public/SX01-alias/app.sh"]
    if source != files["sealed-public/SX02-limit/app.sh"]:
        raise Fx17Error("The public sealed task sources differ")
    schedule = list(manifest["model_schedule"])
    for task, arm in SUPPLEMENT:
        schedule.append({"task": task, "arm": arm, "repeat": 1, "sealed": True})
    payloads = []
    for index, row in enumerate(schedule):
        task, arm = row["task"], row["arm"]
        prompt, brief_name, advisory = prompt_for(task, arm, files, briefs, source)
        req = files[f"sealed-public/{task}/requirements.md"]
        group = "original-core" if index < 4 else "supplemental-misleading"
        payloads.append({
            "row_id": f"{group}:{task}:{arm}", "group": group,
            "task": task, "arm": arm, "repeat": 1, "sealed": True,
            "model": MODEL, "reasoning_effort": EFFORT,
            "max_wall_seconds": WALL, "max_tool_calls": TOOL_MAX,
            "max_tokens_per_attempt": TOKEN_MAX, "automatic_retries": 0,
            "max_attempts_per_row": 1, "tool_access": "disabled",
            "fixture_manifest_sha256": FIXTURE_SHA,
            "independent_brief_manifest_sha256": BRIEF_SHA,
            "source_sha256": digest(source), "requirements_sha256": digest(req),
            "brief_file": brief_name,
            "brief_sha256": digest(briefs[brief_name]) if brief_name else None,
            "prompt_sha256": digest(prompt.encode("utf-8")),
            "prompt_utf8_bytes": len(prompt.encode("utf-8")), "prompt": prompt,
        })
    if len(payloads) != 6:
        raise Fx17Error("FX17 denominator must be six rows")
    output = output.resolve()
    if output.exists():
        raise Fx17Error("Refusing to overwrite frozen payload output: " + str(output))
    output.mkdir(parents=True)
    bdir = output / "briefs"
    bdir.mkdir()
    (output / "independent-brief-manifest.json").write_bytes(review_raw)
    for name, content in briefs.items():
        (bdir / name).write_bytes(content)
    payload_bytes = b"".join(jbytes(p) for p in payloads)
    (output / "payloads.jsonl").write_bytes(payload_bytes)
    supplement = {
        "schema": "aethyme.fx17.supplemental-misleading-rows/v1",
        "authority": "Separate pretrial amendment; original fixture manifest and public contract unchanged.",
        "original_fixture_manifest_sha256": FIXTURE_SHA,
        "independent_brief_manifest_sha256": BRIEF_SHA,
        "supplemental_rows": [
            {"row_id": p["row_id"], "task": p["task"], "arm": p["arm"],
             "brief_file": p["brief_file"], "brief_sha256": p["brief_sha256"],
             "prompt_sha256": p["prompt_sha256"]}
            for p in payloads[4:]],
    }
    combined = {
        "schema": "aethyme.fx17.combined-schedule/v1",
        "original_fixture_manifest_sha256": FIXTURE_SHA,
        "independent_brief_manifest_sha256": BRIEF_SHA,
        "original_core_rows": [p["row_id"] for p in payloads[:4]],
        "supplemental_misleading_rows": [p["row_id"] for p in payloads[4:]],
        "assigned_denominator": 6, "attempts_per_row": 1,
        "model": MODEL, "reasoning_effort": EFFORT,
        "max_wall_seconds_per_attempt": WALL,
        "max_tool_calls_per_attempt": TOOL_MAX,
        "max_tokens_per_attempt": TOKEN_MAX, "automatic_retries": 0,
        "rows": [{k: p[k] for k in (
            "row_id", "group", "task", "arm", "repeat", "sealed",
            "prompt_sha256", "source_sha256", "requirements_sha256",
            "brief_file", "brief_sha256")} for p in payloads],
    }
    save_json(output / "supplemental-manifest.json", supplement)
    save_json(output / "combined-schedule.json", combined)
    freeze = {
        "schema": "aethyme.fx17.payload-freeze/v1",
        "original_fixture_manifest_sha256": FIXTURE_SHA,
        "independent_brief_manifest_sha256": BRIEF_SHA,
        "assigned_denominator": 6, "original_core_denominator": 4,
        "supplemental_misleading_denominator": 2,
        "payloads_sha256": digest(payload_bytes),
        "combined_schedule_sha256": digest(jbytes(combined)),
        "supplemental_manifest_sha256": digest(jbytes(supplement)),
    }
    save_json(output / "freeze.json", freeze)
    return {"output_root": str(output), **freeze}


def profile_hash():
    return digest(SUBJECT_PROFILE.encode("utf-8"))


def require_controls(path):
    controls = json.loads(Path(path).read_text())
    expected = {
        "schema": "aethyme.fx17.independent-isolation-controls/v1",
        "status": "pass", "platform": "darwin",
        "subject_sandbox_profile_sha256": profile_hash(),
        "reviewed_profile_sha256": profile_hash(),
        "shell_exec_allowlist_is_literal_bin_sh": True,
        "recursive_shell_exec_wall_bounded": True,
        "candidate_script_readonly_literal_allow_only": True,
        "candidate_read_canary_blocked": True, "canary_read_unchanged": True,
        "file_write_canary_blocked": True, "write_canary_not_created_or_changed": True,
        "network_canary_blocked": True, "network_server_observed_zero": True,
        "private_canary_nonce_absent_stdout": True, "process_fork_canary_blocked": True,
        "foreign_exec_canary_blocked": True, "child_process_marker_absent": True,
        "forbidden_effects_absent_even_if_exit_zero": True,
        "literal_argv_preserved": True, "stdout_stderr_exit_captured": True,
        "model_tool_access_disabled": True, "model_project_oracle_access_absent": True,
        "automatic_provider_retries_disabled": True, "token_budget_limit_verified": True,
    }
    if any(controls.get(k) != v for k, v in expected.items()):
        raise Fx17Error("Independent effects/isolation controls have not passed")
    if not isinstance(controls.get("reviewer"), str) or not controls["reviewer"].strip():
        raise Fx17Error("Independent controls report must identify its reviewer")
    return controls


def load_payloads(path):
    payloads = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if len(payloads) != 6 or any(
        digest(p["prompt"].encode("utf-8")) != p["prompt_sha256"] for p in payloads
    ):
        raise Fx17Error("Frozen six-row prompt set is invalid")
    return payloads


def gate(freeze_root, approval_path, controls_path, ack_schedule, ack_payloads):
    freeze = json.loads((freeze_root / "freeze.json").read_text())
    schedule = json.loads((freeze_root / "combined-schedule.json").read_text())
    raw_payloads = (freeze_root / "payloads.jsonl").read_bytes()
    schedule_hash = digest(jbytes(schedule))
    payload_hash = digest(raw_payloads)
    if schedule_hash != freeze["combined_schedule_sha256"] or payload_hash != freeze["payloads_sha256"]:
        raise Fx17Error("Frozen schedule or payload integrity mismatch")
    if (ack_schedule, ack_payloads) != (schedule_hash, payload_hash):
        raise Fx17Error("Explicit schedule and payload hash acknowledgements are required")
    approval = json.loads(approval_path.read_text())
    required = {
        "schema": "aethyme.fx17.explicit-trial-approval/v1", "approved": True,
        "original_fixture_manifest_sha256": FIXTURE_SHA,
        "independent_brief_manifest_sha256": BRIEF_SHA,
        "combined_schedule_sha256": schedule_hash, "payloads_sha256": payload_hash,
        "assigned_denominator": 6, "model": MODEL, "reasoning_effort": EFFORT,
        "max_wall_seconds_per_attempt": WALL, "max_tool_calls_per_attempt": TOOL_MAX,
        "max_tokens_per_attempt": TOKEN_MAX, "max_attempts_per_row": 1,
        "automatic_retries": 0,
    }
    if any(approval.get(k) != v for k, v in required.items()) or not approval.get("approved_by"):
        raise Fx17Error("Explicit trial approval does not match the frozen six-row profile")
    controls = require_controls(controls_path)
    if not controls.get("reviewer"):
        raise Fx17Error("Independent effects/isolation controls have not passed")
    return load_payloads(freeze_root / "payloads.jsonl"), freeze, approval, controls, schedule_hash, payload_hash


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def minimal_env():
    env = {k: os.environ[k] for k in ("PATH", "HOME", "CODEX_HOME", "TMPDIR", "LANG", "LC_ALL", "TERM") if k in os.environ}
    env["LC_ALL"] = "C"
    return env


def codex_command(schema, last, cwd):
    binary = shutil.which("codex")
    if not binary:
        raise Fx17Error("Codex CLI unavailable")
    cmd = [binary, "exec", "--ignore-user-config", "--ignore-rules",
           "--skip-git-repo-check", "--strict-config", "--ephemeral",
           "--sandbox", "read-only", "--color", "never", "--json",
           "--output-schema", str(schema), "--output-last-message", str(last),
           "-m", MODEL, "-C", str(cwd)]
    for feature in DISABLE:
        cmd += ["--disable", feature]
    cmd += ["-c", 'model_provider="openai"',
            "-c", f'model_reasoning_effort="{EFFORT}"',
            "-c", f"model_max_output_tokens={TOKEN_MAX}",
            "-c", 'web_search="disabled"',
            "-c", "model_providers.openai.request_max_retries=0",
            "-c", "model_providers.openai.stream_max_retries=0", "-"]
    return cmd


def kill_tree(proc):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def run_row(payload, output):
    rowdir = output / payload["row_id"].replace(":", "__")
    rowdir.mkdir()
    schema, last = rowdir / "schema.json", rowdir / "last-message.json"
    schema.write_bytes(jbytes(RESPONSE_SCHEMA))
    result = {"row_id": payload["row_id"], "group": payload["group"],
              "task": payload["task"], "arm": payload["arm"], "attempt": 1,
              "status": "running", "started_at": now(), "model": MODEL,
              "reasoning_effort": EFFORT, "configured_max_wall_seconds": WALL,
              "configured_max_tool_calls": TOOL_MAX, "effective_tools_available": 0,
              "configured_max_tokens": TOKEN_MAX, "automatic_retries": 0,
              "configured_provider": "openai", "configured_request_retries": 0,
              "configured_stream_retries": 0,
              "provider_access_observation": {
                  "client": "codex exec", "configured_provider": "openai",
                  "model": MODEL, "turn_events_observed": None},
              "provider_request_count_observed": None,
              "provider_retry_count_observed": None, "tool_calls_observed": None,
              "provider_usage": None, "provider_total_tokens": None,
              "invoiced_cost_usd": None, "refusal": None, "candidate_sha256": None}
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="aethyme-fx17-empty-cwd-") as tmp:
        cmd = codex_command(schema, last, Path(tmp))
        result["codex_command"] = cmd
        try:
            proc = subprocess.Popen(cmd, cwd=tmp, env=minimal_env(),
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, start_new_session=True,
                                    close_fds=True)
            try:
                stdout, stderr = proc.communicate(payload["prompt"].encode(), timeout=WALL)
            except subprocess.TimeoutExpired:
                kill_tree(proc)
                stdout, stderr = proc.communicate()
                result["status"] = "timed_out"
            (rowdir / "events.jsonl").write_bytes(stdout)
            (rowdir / "stderr.log").write_bytes(stderr)
            events = []
            usage = None
            tool_ids = set()
            for n, line in enumerate(stdout.decode("utf-8", "replace").splitlines()):
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(event, dict):
                    continue
                events.append(event)
                if event.get("type") == "turn.completed" and isinstance(event.get("usage"), dict):
                    usage = event["usage"]
                item = event.get("item")
                if isinstance(item, dict) and item.get("type") not in (None, "agent_message", "reasoning"):
                    tool_ids.add(str(item.get("id", n)))
            result["cli_exit_code"] = proc.returncode
            result["provider_usage"] = usage
            result["tool_calls_observed"] = len(tool_ids) if events else None
            if usage and type(usage.get("input_tokens")) is int and type(usage.get("output_tokens")) is int:
                result["provider_total_tokens"] = usage["input_tokens"] + usage["output_tokens"]
            result["provider_retry_count_observed"] = None
            result["provider_request_count_observed"] = None
            result["event_types"] = [e.get("type") for e in events]
            result["provider_access_observation"]["turn_events_observed"] = len(events)
            if result["status"] != "timed_out":
                if proc.returncode:
                    result["status"] = "cli_or_provider_error"
                elif not last.is_file():
                    result["status"] = "missing_response"
                else:
                    raw = last.read_bytes()
                    (rowdir / "response.json").write_bytes(raw)
                    try:
                        response = json.loads(raw)
                    except (UnicodeDecodeError, json.JSONDecodeError):
                        response = None
                    if not isinstance(response, dict) or set(response) != {"app_sh", "refusal"}:
                        result["status"] = "invalid_response"
                    elif response.get("app_sh") is None and isinstance(response.get("refusal"), str):
                        result["status"], result["refusal"] = "refusal", response["refusal"]
                    elif (not isinstance(response.get("app_sh"), str)
                          or response.get("refusal") is not None or tool_ids):
                        result["status"] = "tool_access_violation" if tool_ids else "invalid_response"
                    else:
                        candidate = response["app_sh"].encode("utf-8")
                        if b"\0" in candidate:
                            result["status"] = "invalid_response"
                        else:
                            result["status"] = "response_received"
                            result["candidate_sha256"] = digest(candidate)
                            result["candidate_utf8_bytes"] = len(candidate)
                            (rowdir / "candidate.app.sh").write_bytes(candidate)
            result["usage_exceeded_token_limit"] = (
                result["provider_total_tokens"] > TOKEN_MAX
                if result["provider_total_tokens"] is not None else None)
        except OSError as exc:
            result["status"] = "runner_error"
            result["runner_error"] = str(exc)
            (rowdir / "stderr.log").write_text(str(exc))
    result["finished_at"] = now()
    result["wall_seconds"] = round(time.monotonic() - start, 3)
    return result


def run(freeze_root, approval_path, controls_path, output, ack_schedule, ack_payloads):
    payloads, freeze, approval, controls, schedule_hash, payload_hash = gate(
        freeze_root, approval_path, controls_path, ack_schedule, ack_payloads)
    if sys.platform != "darwin":
        raise Fx17Error("macOS subject isolation is required; no model calls started")
    output = output.resolve()
    if output.exists():
        raise Fx17Error("Refusing to reuse a trial output directory")
    consumed = approval_path.with_name(approval_path.name + ".fx17-consumed")
    fd = os.open(consumed, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "w") as file:
        file.write(schedule_hash + "\n" + payload_hash + "\n")
    output.mkdir(parents=True)
    state = {"schema": "aethyme.fx17.results/v1", "assigned_denominator": 6,
             "original_core_denominator": 4, "supplemental_misleading_denominator": 2,
             "freeze": freeze, "approval_by": approval["approved_by"],
             "controls_reviewer": controls["reviewer"], "started_at": now(),
             "rows": [{"row_id": p["row_id"], "group": p["group"],
                       "task": p["task"], "arm": p["arm"], "attempt": 1,
                       "status": "not_started", "invoiced_cost_usd": None}
                      for p in payloads]}
    save_json(output / "results.json", state)
    for i, payload in enumerate(payloads):
        state["rows"][i]["status"] = "running"
        save_json(output / "results.json", state)
        state["rows"][i] = run_row(payload, output)
        save_json(output / "results.json", state)
    state["finished_at"] = now()
    state["refusal_count"] = sum(r["status"] == "refusal" for r in state["rows"])
    state["response_count"] = sum(r["status"] == "response_received" for r in state["rows"])
    save_json(output / "results.json", state)
    return {"output": str(output), "denominator": 6, "refusals": state["refusal_count"],
            "responses": state["response_count"], "schedule_sha256": schedule_hash,
            "payloads_sha256": payload_hash}


def run_subject(source, argv, controls_path, timeout=2):
    """Run a candidate only after validating independent isolation evidence."""
    require_controls(controls_path)
    args = list(argv)
    if sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists():
        raise Fx17Error("Subject runner requires macOS sandbox-exec")
    if (not isinstance(source, str) or "\0" in source
            or any(not isinstance(a, str) or "\0" in a for a in args)):
        raise Fx17Error("Source and argv must be NUL-free strings")
    if timeout <= 0 or timeout > WALL:
        raise Fx17Error("Subject timeout must be in (0, 180]")
    # tempfile uses a private 0700 directory; only its read-only script path
    # is allowlisted inside the child. The shell gets no private-oracle path.
    with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="aethyme-fx17-subject-") as tmp:
        app = Path(tmp) / "app.sh"
        app.write_bytes(source.encode("utf-8"))
        app.chmod(0o400)
        app_text = str(app)
        if not app_text.isascii() or any(ch in app_text for ch in ('"', "\\", "\n")):
            raise Fx17Error("Unexpected subject path form")
        profile = SUBJECT_PROFILE.replace("__FX17_SUBJECT_APP__", app_text)
        cmd = ["/usr/bin/sandbox-exec", "-p", profile,
               "/bin/sh", app_text, *args]
        proc = subprocess.Popen(cmd, env={"LC_ALL": "C", "PATH": "/usr/bin:/bin"},
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, start_new_session=True,
                                close_fds=True)
        started = time.monotonic()
        try:
            out, err = proc.communicate(timeout=timeout)
            timed_out = False
        except subprocess.TimeoutExpired:
            kill_tree(proc)
            out, err = proc.communicate()
            timed_out = True
        return {"argv": args, "exit_code": proc.returncode, "stdout": out,
                "stderr": err, "timed_out": timed_out,
                "wall_seconds": round(time.monotonic() - started, 3),
                "sandboxed": True,
                "profile_sha256": digest(profile.encode("utf-8")),
                "profile_template_sha256": profile_hash()}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("--fixture-root", type=Path, default=DEFAULT_FIXTURE)
    prep.add_argument("--briefs-root", type=Path, default=DEFAULT_BRIEFS)
    prep.add_argument("--output-root", type=Path, default=DEFAULT_FREEZE)
    trial = sub.add_parser("run")
    trial.add_argument("--freeze-root", type=Path, default=DEFAULT_FREEZE)
    trial.add_argument("--approval", type=Path, required=True)
    trial.add_argument("--controls", type=Path, required=True)
    trial.add_argument("--output-root", type=Path, required=True)
    trial.add_argument("--ack-schedule-sha256", required=True)
    trial.add_argument("--ack-payloads-sha256", required=True)
    args = parser.parse_args()
    try:
        if args.cmd == "prepare":
            result = prepare(args.fixture_root, args.briefs_root, args.output_root)
        else:
            result = run(args.freeze_root, args.approval, args.controls, args.output_root,
                         args.ack_schedule_sha256, args.ack_payloads_sha256)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (Fx17Error, OSError, ValueError, json.JSONDecodeError) as exc:
        print(json.dumps({"error": str(exc)}, sort_keys=True), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
