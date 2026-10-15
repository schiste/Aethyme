#!/usr/bin/env python3
"""Freeze public FX17 model inputs and run them only behind explicit gates."""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import os
import selectors
import signal
import stat
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
PAYLOADS_SHA256 = "e733e0183b5bc9d54ba65c3437109b170b9c348d13defd8fb7bcfc91c7bacd28"
COMBINED_SCHEDULE_SHA256 = "8d1474930397a2ce7c0bae1c6fccde6db5c32aba26ef5ad845945a91b150f065"
MODEL = "gpt-6-luna"
EFFORT = "medium"
WALL = 180
TOOL_MAX = 2
TOKEN_MAX = 32768
CODEX_BIN = Path("/Users/christophehenner/.local/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex")
CODEX_VERSION = "0.162.1"
CODEX_SHA256 = "74c6a6d263f40d65f99c27a815275f5b4ed0bcc2baecc396b4cf3255498bdfc0"
RUNTIME_CORRECTION_SHA256 = "050feab619b4b78503956dfa7a71e2b0550f936bd5048d9761d6e1d0509a5826"
SUBJECT_STDOUT_MAX = 1024 * 1024
SUBJECT_STDERR_MAX = 1024 * 1024
MODEL_STDOUT_MAX = 8 * 1024 * 1024
MODEL_STDERR_MAX = 2 * 1024 * 1024
MODEL_RESPONSE_MAX = 4 * 1024 * 1024
MAX_CANDIDATE_BYTES = 4 * 1024 * 1024
REAP_GRACE_SECONDS = 2.0
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
(allow process-exec (literal "/bin/bash"))
(deny process-fork)
(deny network*)
(deny file-write*)
(allow file-read-data (literal "/"))
(allow file-read* (literal "/bin/sh"))
(allow file-read* (literal "/bin/bash"))
(allow file-read-metadata (literal "/private/var/select/sh"))
(allow file-read* (subpath "/System/Library"))
(allow file-read* (subpath "/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld"))
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


def external_output(path, label):
    requested = Path(path).expanduser()
    if requested.exists() or requested.is_symlink():
        raise Fx17Error("Refusing to reuse " + label + ": " + str(requested))
    path = requested.resolve()
    repo_root = HERE.parents[3].resolve(strict=True)
    try:
        path.relative_to(repo_root)
    except ValueError:
        return path
    raise Fx17Error("Keep " + label + " outside the repository")


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
    payload_bytes = b"".join(jbytes(p) for p in payloads)
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
    payload_hash = digest(payload_bytes)
    combined_bytes = jbytes(combined)
    schedule_hash = digest(combined_bytes)
    if (payload_hash != PAYLOADS_SHA256
            or schedule_hash != COMBINED_SCHEDULE_SHA256):
        raise Fx17Error("Generated FX17 payloads differ from the frozen six-row schedule")

    requested_output = Path(output).expanduser()
    if requested_output.exists() or requested_output.is_symlink():
        raise Fx17Error("Refusing to overwrite frozen payload output: " + str(requested_output))
    output = requested_output.resolve()
    if output.exists():
        raise Fx17Error("Refusing to overwrite frozen payload output: " + str(output))
    output.mkdir(parents=True)
    bdir = output / "briefs"
    bdir.mkdir()
    (output / "independent-brief-manifest.json").write_bytes(review_raw)
    for name, content in briefs.items():
        (bdir / name).write_bytes(content)
    (output / "payloads.jsonl").write_bytes(payload_bytes)
    save_json(output / "supplemental-manifest.json", supplement)
    (output / "combined-schedule.json").write_bytes(combined_bytes)
    freeze = {
        "schema": "aethyme.fx17.payload-freeze/v1",
        "original_fixture_manifest_sha256": FIXTURE_SHA,
        "independent_brief_manifest_sha256": BRIEF_SHA,
        "assigned_denominator": 6, "original_core_denominator": 4,
        "supplemental_misleading_denominator": 2,
        "payloads_sha256": payload_hash,
        "combined_schedule_sha256": schedule_hash,
        "supplemental_manifest_sha256": digest(jbytes(supplement)),
    }
    save_json(output / "freeze.json", freeze)
    return {"output_root": str(output), **freeze}


def profile_hash():
    return digest(SUBJECT_PROFILE.encode("utf-8"))


def runner_hash():
    return digest((HERE / "run_fx17.py").read_bytes())


def runtime_profile():
    return {
        "schema": "aethyme.fx17.runtime-profile/v1",
        "codex_binary_path": str(CODEX_BIN),
        "codex_version": CODEX_VERSION,
        "codex_binary_sha256": CODEX_SHA256,
        "model": MODEL,
        "reasoning_effort": EFFORT,
        "wall_seconds_per_row": WALL,
        "stdout_capture_limit_bytes": MODEL_STDOUT_MAX,
        "stderr_capture_limit_bytes": MODEL_STDERR_MAX,
        "response_file_size_limit_bytes": MODEL_RESPONSE_MAX,
        "nominal_output_token_threshold": TOKEN_MAX,
        "output_token_limit_enforcement": "unverified",
        "provider_retry_behavior": "unknown",
        "runner_attempts_per_row": 1,
        "available_tools": [],
    }


def runtime_profile_hash():
    return digest(jbytes(runtime_profile()))


def require_controls(path):
    from fx17_controls import control_harness_hash, verify_controls_evidence

    path = Path(path)
    try:
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_size > 8 * 1024 * 1024:
            raise Fx17Error("Isolation-control evidence must be a regular file no larger than 8 MiB")
        with path.open("rb") as stream:
            raw = stream.read(8 * 1024 * 1024 + 1)
    except OSError as exc:
        raise Fx17Error("Could not read isolation-control evidence: " + str(exc)) from exc
    if len(raw) > 8 * 1024 * 1024:
        raise Fx17Error("Isolation-control evidence exceeds the 8 MiB read bound")
    try:
        controls = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise Fx17Error("Isolation-control evidence is not valid UTF-8 JSON") from exc
    if not isinstance(controls, dict):
        raise Fx17Error("Isolation-control evidence must be a JSON object")
    if (controls.get("schema") != "aethyme.fx17.independent-isolation-controls/v3"
            or controls.get("status") != "pass"
            or controls.get("platform") != "darwin"):
        raise Fx17Error("Independent effects/isolation controls have not passed")
    if (controls.get("runner_sha256") != runner_hash()
            or controls.get("control_harness_sha256") != control_harness_hash()
            or controls.get("subject_profile_template_sha256") != profile_hash()):
        raise Fx17Error("Isolation-control evidence is for a different runner, harness, or profile")
    review = controls.get("independent_review")
    if (not isinstance(review, dict)
            or not isinstance(review.get("reviewer"), str)
            or not review["reviewer"].strip()
            or review.get("runner_sha256") != runner_hash()
            or review.get("control_harness_sha256") != control_harness_hash()
            or review.get("subject_profile_template_sha256") != profile_hash()):
        raise Fx17Error("Isolation evidence lacks a matching independent review record")
    verify_controls_evidence(controls)
    return controls


def load_payloads(path):
    payloads = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    expected_ids = ([f"original-core:{task}:{arm}" for task, arm in CORE]
                    + [f"supplemental-misleading:{task}:{arm}" for task, arm in SUPPLEMENT])
    if (len(payloads) != 6
            or [p.get("row_id") for p in payloads] != expected_ids
            or any(not isinstance(p.get("prompt"), str)
                   or digest(p["prompt"].encode("utf-8")) != p.get("prompt_sha256")
                   or len(p["prompt"].encode("utf-8")) != p.get("prompt_utf8_bytes")
                   for p in payloads)):
        raise Fx17Error("Frozen six-row prompt set is invalid")
    return payloads


def verify_frozen(freeze_root):
    freeze_root = Path(freeze_root).resolve(strict=True)
    freeze_path = freeze_root / "freeze.json"
    schedule_path = freeze_root / "combined-schedule.json"
    payload_path = freeze_root / "payloads.jsonl"
    for path in (freeze_path, schedule_path, payload_path):
        if path.is_symlink() or not under(freeze_root, path) or not path.is_file():
            raise Fx17Error("Frozen payload set is missing a regular in-root file: " + path.name)
    freeze = json.loads(freeze_path.read_text(encoding="utf-8"))
    schedule_raw = schedule_path.read_bytes()
    schedule = json.loads(schedule_raw)
    payload_bytes = payload_path.read_bytes()
    schedule_hash = digest(schedule_raw)
    payload_hash = digest(payload_bytes)
    if (freeze.get("schema") != "aethyme.fx17.payload-freeze/v1"
            or freeze.get("original_fixture_manifest_sha256") != FIXTURE_SHA
            or freeze.get("independent_brief_manifest_sha256") != BRIEF_SHA
            or freeze.get("assigned_denominator") != 6
            or freeze.get("original_core_denominator") != 4
            or freeze.get("supplemental_misleading_denominator") != 2
            or schedule_hash != freeze.get("combined_schedule_sha256")
            or payload_hash != freeze.get("payloads_sha256")
            or schedule_hash != COMBINED_SCHEDULE_SHA256
            or payload_hash != PAYLOADS_SHA256):
        raise Fx17Error("Frozen schedule or payload integrity mismatch")
    if (schedule.get("schema") != "aethyme.fx17.combined-schedule/v1"
            or schedule.get("original_fixture_manifest_sha256") != FIXTURE_SHA
            or schedule.get("independent_brief_manifest_sha256") != BRIEF_SHA
            or schedule.get("assigned_denominator") != 6
            or not isinstance(schedule.get("rows"), list)
            or len(schedule["rows"]) != 6):
        raise Fx17Error("Frozen combined schedule provenance is invalid")
    payloads = load_payloads(payload_path)
    if [p["row_id"] for p in payloads] != [r.get("row_id") for r in schedule["rows"]]:
        raise Fx17Error("Frozen payload row order differs from the combined schedule")
    for payload, row in zip(payloads, schedule["rows"]):
        if any(payload.get(key) != row.get(key) for key in (
                "row_id", "group", "task", "arm", "prompt_sha256",
                "source_sha256", "requirements_sha256", "brief_file", "brief_sha256")):
            raise Fx17Error("Frozen payload provenance differs from the combined schedule")
    return payloads, freeze, schedule, schedule_hash, payload_hash


def validate_consumption_registry(value):
    if not isinstance(value, str) or not value:
        raise Fx17Error("Trial approval must name a durable consumption registry directory")
    registry = Path(value)
    if not registry.is_absolute() or str(registry) != os.path.normpath(str(registry)):
        raise Fx17Error("The consumption registry path must be an absolute canonical path")
    try:
        info = registry.lstat()
    except OSError as exc:
        raise Fx17Error("The approved consumption registry directory is unavailable") from exc
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise Fx17Error("The approved consumption registry must be a real directory")
    resolved = registry.resolve(strict=True)
    if resolved != registry:
        raise Fx17Error("The approved consumption registry path must not traverse symlinks")
    repo_root = HERE.parents[3].resolve(strict=True)
    if under(repo_root, registry):
        raise Fx17Error("Keep the trial consumption registry outside the repository")
    return registry


def campaign_consumption_key(schedule_hash, payload_hash):
    return digest(jbytes({
        "schema": "aethyme.fx17.campaign-consumption-key/v1",
        "combined_schedule_sha256": schedule_hash,
        "payloads_sha256": payload_hash,
    }))


def claim_campaign_consumption(registry_path, schedule_hash, payload_hash,
                               approval_bytes):
    registry = validate_consumption_registry(registry_path)
    key = campaign_consumption_key(schedule_hash, payload_hash)
    marker = registry / ("fx17-" + key + ".consumed.json")
    record = {
        "schema": "aethyme.fx17.consumed-campaign/v1",
        "campaign_consumption_key_sha256": key,
        "combined_schedule_sha256": schedule_hash,
        "payloads_sha256": payload_hash,
        "approval_sha256": digest(approval_bytes),
        "consumed_at": now(),
        "state": "started",
    }
    raw = jbytes(record)
    flags = os.O_CREAT | os.O_EXCL | os.O_WRONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(marker, flags, 0o600)
    except FileExistsError as exc:
        raise Fx17Error("This FX17 campaign was already consumed in the approved registry") from exc
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    directory_fd = os.open(registry, os.O_RDONLY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)
    return {"campaign_consumption_key_sha256": key,
            "marker_sha256": digest(raw), "approval_sha256": digest(approval_bytes)}


def gate(freeze_root, approval_path, controls_path, ack_schedule, ack_payloads):
    payloads, freeze, schedule, schedule_hash, payload_hash = verify_frozen(freeze_root)
    if (ack_schedule, ack_payloads) != (schedule_hash, payload_hash):
        raise Fx17Error("Explicit schedule and payload hash acknowledgements are required")
    approval_path = Path(approval_path)
    if approval_path.is_symlink() or not approval_path.is_file():
        raise Fx17Error("Trial approval must be a regular file, not a symlink")
    with approval_path.open("rb") as stream:
        approval_raw = stream.read(64 * 1024 + 1)
    if len(approval_raw) > 64 * 1024:
        raise Fx17Error("Trial approval exceeds the 64 KiB read bound")
    approval = json.loads(approval_raw)
    runtime_correction = HERE / "runtime-contract-correction.md"
    if (not runtime_correction.is_file()
            or runtime_correction.is_symlink()
            or digest(runtime_correction.read_bytes()) != RUNTIME_CORRECTION_SHA256):
        raise Fx17Error("Runtime contract correction is missing or has changed")
    required = {
        "schema": "aethyme.fx17.explicit-trial-approval/v1", "approved": True,
        "original_fixture_manifest_sha256": FIXTURE_SHA,
        "independent_brief_manifest_sha256": BRIEF_SHA,
        "combined_schedule_sha256": schedule_hash, "payloads_sha256": payload_hash,
        "assigned_denominator": 6, "model": MODEL, "reasoning_effort": EFFORT,
        "max_wall_seconds_per_attempt": WALL, "max_tool_calls_per_attempt": TOOL_MAX,
        "max_tokens_per_attempt": TOKEN_MAX, "max_attempts_per_row": 1,
        "automatic_retries": 0,
        "runtime_contract_correction_sha256": RUNTIME_CORRECTION_SHA256,
        "runtime_profile_sha256": runtime_profile_hash(),
        "output_token_limit_enforcement": "unverified",
        "provider_retry_behavior": "unknown",
    }
    if any(approval.get(k) != v for k, v in required.items()) or not approval.get("approved_by"):
        raise Fx17Error("Explicit trial approval does not match the frozen six-row profile")
    registry = validate_consumption_registry(approval.get("consumption_registry_path"))
    controls = require_controls(controls_path)
    return payloads, freeze, approval, controls, schedule_hash, payload_hash, approval_raw, registry


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def minimal_env():
    env = {k: os.environ[k] for k in ("PATH", "HOME", "CODEX_HOME", "TMPDIR", "LANG", "LC_ALL", "TERM") if k in os.environ}
    env["LC_ALL"] = "C"
    return env


def codex_command(schema, last, cwd):
    verify_codex_binary()
    cmd = [str(CODEX_BIN), "exec", "--ignore-user-config", "--ignore-rules",
           "--skip-git-repo-check", "--strict-config", "--ephemeral",
           "--sandbox", "read-only", "--color", "never", "--json",
           "--output-schema", str(schema), "--output-last-message", str(last),
           "-m", MODEL, "-C", str(cwd)]
    for feature in DISABLE:
        cmd += ["--disable", feature]
    cmd += ["-c", 'model_provider="openai"',
            "-c", f'model_reasoning_effort="{EFFORT}"',
            "-c", 'web_search="disabled"', "-"]
    return cmd


def verify_codex_binary():
    try:
        info = CODEX_BIN.lstat()
    except OSError as exc:
        raise Fx17Error("Pinned Codex CLI binary is unavailable") from exc
    if not stat.S_ISREG(info.st_mode) or digest(CODEX_BIN.read_bytes()) != CODEX_SHA256:
        raise Fx17Error("Pinned Codex CLI binary hash mismatch")
    return CODEX_SHA256


def kill_tree(proc):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        return False
    return True


def bounded_communicate(proc, input_bytes, timeout, stdout_limit, stderr_limit,
                        watch_paths=None):
    """Drain process pipes without retaining more than the per-stream caps."""
    started = time.monotonic()
    deadline = started + timeout
    captures = {"stdout": bytearray(), "stderr": bytearray()}
    limits = {"stdout": stdout_limit, "stderr": stderr_limit}
    overflow = set()
    timed_out = False
    stream_drain_timed_out = False
    process_group_killed = False
    process_exited_at = None
    forced_drain_deadline = None
    reap_deadline_exceeded = False
    watch_paths = watch_paths or {}
    watched_overflow = set()
    watched_file_sizes = {}
    selector = selectors.DefaultSelector()
    registered_streams = {}
    stdin_file = proc.stdin

    def close_registered(fileobj):
        try:
            selector.unregister(fileobj)
        except (KeyError, ValueError):
            pass
        try:
            fileobj.close()
        except OSError:
            pass

    def kill_group():
        nonlocal process_group_killed, forced_drain_deadline
        signalled = kill_tree(proc)
        process_group_killed = process_group_killed or signalled
        if signalled and forced_drain_deadline is None:
            forced_drain_deadline = time.monotonic() + REAP_GRACE_SECONDS
        if stdin_file is not None:
            close_registered(stdin_file)

    try:
        for name, fileobj in (("stdout", proc.stdout), ("stderr", proc.stderr)):
            if fileobj is None:
                continue
            os.set_blocking(fileobj.fileno(), False)
            selector.register(fileobj, selectors.EVENT_READ, name)
            registered_streams[name] = fileobj
        if stdin_file is not None:
            if input_bytes:
                os.set_blocking(stdin_file.fileno(), False)
                selector.register(stdin_file, selectors.EVENT_WRITE, "stdin")
            else:
                close_registered(stdin_file)
                stdin_file = None
        input_offset = 0

        while True:
            now = time.monotonic()
            for name, (path, byte_limit) in watch_paths.items():
                try:
                    info = Path(path).lstat()
                    watched_file_sizes[name] = max(
                        watched_file_sizes.get(name, 0), info.st_size)
                    if not stat.S_ISREG(info.st_mode) or info.st_size > byte_limit:
                        watched_overflow.add(name)
                except FileNotFoundError:
                    pass
                except OSError:
                    watched_overflow.add(name)
            if watched_overflow:
                overflow.update(watched_overflow)
            if proc.poll() is None and not timed_out and not overflow and now >= deadline:
                timed_out = True
                kill_group()
            elif overflow and not process_group_killed:
                kill_group()

            if proc.poll() is not None:
                if process_exited_at is None:
                    process_exited_at = now
                if "stdin" in [key.data for key in selector.get_map().values()]:
                    if stdin_file is not None:
                        close_registered(stdin_file)
                        stdin_file = None
                open_output = any(name in registered_streams for name in ("stdout", "stderr"))
                if not open_output:
                    break
                if now - process_exited_at >= REAP_GRACE_SECONDS:
                    stream_drain_timed_out = True
                    kill_group()
                    for name, fileobj in list(registered_streams.items()):
                        close_registered(fileobj)
                        registered_streams.pop(name, None)
                    break

            if forced_drain_deadline is not None and now >= forced_drain_deadline:
                stream_drain_timed_out = bool(registered_streams)
                for name, fileobj in list(registered_streams.items()):
                    close_registered(fileobj)
                    registered_streams.pop(name, None)
                if proc.poll() is not None:
                    break
                reap_deadline_exceeded = True
                kill_group()
                break

            wait_for = 0.05
            if proc.poll() is None and not timed_out and not overflow:
                wait_for = min(wait_for, max(0.0, deadline - now))
            if forced_drain_deadline is not None:
                wait_for = min(wait_for, max(0.0, forced_drain_deadline - now))
            elif process_exited_at is not None:
                wait_for = min(wait_for, max(0.0, process_exited_at + REAP_GRACE_SECONDS - now))
            events = selector.select(wait_for)
            for key, _ in events:
                name = key.data
                fileobj = key.fileobj
                if name == "stdin":
                    if input_offset >= len(input_bytes):
                        close_registered(fileobj)
                        stdin_file = None
                        continue
                    try:
                        wrote = os.write(fileobj.fileno(), input_bytes[input_offset:input_offset + 65536])
                    except (BrokenPipeError, OSError):
                        close_registered(fileobj)
                        stdin_file = None
                    else:
                        input_offset += wrote
                        if input_offset >= len(input_bytes):
                            close_registered(fileobj)
                            stdin_file = None
                    continue

                capture = captures[name]
                remaining = limits[name] - len(capture)
                try:
                    data = os.read(fileobj.fileno(), min(65536, max(1, remaining + 1)))
                except BlockingIOError:
                    continue
                if not data:
                    close_registered(fileobj)
                    registered_streams.pop(name, None)
                    continue
                capture.extend(data[:max(0, remaining)])
                if len(data) > remaining:
                    overflow.add(name)
            if overflow and not process_group_killed:
                kill_group()

    finally:
        if proc.poll() is None:
            kill_group()
        try:
            selector.close()
        finally:
            for fileobj in (proc.stdin, proc.stdout, proc.stderr):
                if fileobj is not None:
                    try:
                        fileobj.close()
                    except OSError:
                        pass

    try:
        proc.wait(timeout=0.1 if reap_deadline_exceeded else REAP_GRACE_SECONDS)
    except subprocess.TimeoutExpired as exc:
        raise Fx17Error("Could not reap the process-group leader within the bounded deadline") from exc
    return {
        "stdout": bytes(captures["stdout"]),
        "stderr": bytes(captures["stderr"]),
        "timed_out": timed_out,
        "overflow_streams": sorted(overflow),
        "stream_drain_timed_out": stream_drain_timed_out,
        "process_group_killed": process_group_killed,
        "watched_file_sizes": watched_file_sizes,
        "reaped": proc.returncode is not None,
    }


def run_row(payload, output):
    rowdir = output / payload["row_id"].replace(":", "__")
    rowdir.mkdir(mode=0o700)
    schema, last = rowdir / "schema.json", rowdir / "last-message.json"
    schema.write_bytes(jbytes(RESPONSE_SCHEMA))
    result = {"row_id": payload["row_id"], "group": payload["group"],
              "task": payload["task"], "arm": payload["arm"], "attempt": 1,
              "status": "running", "started_at": now(), "model": MODEL,
              "reasoning_effort": EFFORT, "configured_max_wall_seconds": WALL,
              "configured_max_tool_calls": TOOL_MAX, "effective_tools_available": 0,
              "nominal_output_token_threshold": TOKEN_MAX,
              "output_token_limit_enforcement": "unverified",
              "automatic_retries": 0,
              "stdout_capture_limit_bytes": MODEL_STDOUT_MAX,
              "stderr_capture_limit_bytes": MODEL_STDERR_MAX,
              "response_file_limit_bytes": MODEL_RESPONSE_MAX,
              "codex_version": CODEX_VERSION,
              "codex_binary_sha256": CODEX_SHA256,
              "configured_provider": "openai",
              "provider_retry_behavior": "unknown",
              "provider_access_observation": {
                  "client": "codex exec", "configured_provider": "openai",
                  "model": MODEL, "turn_events_observed": None},
              "provider_request_count_observed": None,
              "provider_retry_count_observed": None, "tool_calls_observed": None,
              "provider_usage": None, "provider_input_tokens": None,
              "provider_output_tokens": None, "provider_total_tokens": None,
              "output_tokens_exceeded_nominal_threshold": None,
              "stdout_bytes_captured": None, "stdout_sha256": None,
              "stderr_bytes_captured": None, "stderr_sha256": None,
              "overflow_streams": [], "process_group_killed": None,
              "process_reaped": None, "response_file_size_bytes_observed": None,
              "response_file_sha256": None,
              "invoiced_cost_usd": None, "refusal": None, "candidate_sha256": None}
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="aethyme-fx17-empty-cwd-") as tmp:
        try:
            cmd = codex_command(schema, last, Path(tmp))
            result["codex_command"] = cmd
            proc = subprocess.Popen(cmd, cwd=tmp, env=minimal_env(),
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, start_new_session=True,
                                    close_fds=True)
            capture = bounded_communicate(
                proc, payload["prompt"].encode("utf-8"), WALL,
                MODEL_STDOUT_MAX, MODEL_STDERR_MAX,
                watch_paths={"response_file": (last, MODEL_RESPONSE_MAX)},
            )
            stdout, stderr = capture["stdout"], capture["stderr"]
            (rowdir / "events.jsonl").write_bytes(stdout)
            (rowdir / "stderr.log").write_bytes(stderr)
            result["stdout_bytes_captured"] = len(stdout)
            result["stdout_sha256"] = digest(stdout)
            result["stderr_bytes_captured"] = len(stderr)
            result["stderr_sha256"] = digest(stderr)
            result["overflow_streams"] = capture["overflow_streams"]
            result["process_group_killed"] = capture["process_group_killed"]
            result["process_reaped"] = capture["reaped"]
            result["response_file_size_bytes_observed"] = capture["watched_file_sizes"].get("response_file")
            events = []
            usage = None
            tool_ids = set()
            if "stdout" not in capture["overflow_streams"]:
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
            if usage:
                input_tokens = usage.get("input_tokens")
                output_tokens = usage.get("output_tokens")
                if type(input_tokens) is int and input_tokens >= 0:
                    result["provider_input_tokens"] = input_tokens
                if type(output_tokens) is int and output_tokens >= 0:
                    result["provider_output_tokens"] = output_tokens
                if (result["provider_input_tokens"] is not None
                        and result["provider_output_tokens"] is not None):
                    result["provider_total_tokens"] = (
                        result["provider_input_tokens"] + result["provider_output_tokens"])
            result["provider_retry_count_observed"] = None
            result["provider_request_count_observed"] = None
            result["event_types"] = [e.get("type") for e in events]
            result["provider_access_observation"]["turn_events_observed"] = len(events)
            if capture["overflow_streams"]:
                result["status"] = "output_limit_exceeded"
            elif capture["timed_out"]:
                result["status"] = "timed_out"
            elif capture["stream_drain_timed_out"]:
                result["status"] = "stream_drain_timeout"
            else:
                if proc.returncode:
                    result["status"] = "cli_or_provider_error"
                elif not last.exists():
                    result["status"] = "missing_response"
                else:
                    info = last.lstat()
                    if not stat.S_ISREG(info.st_mode):
                        result["status"] = "invalid_response"
                    elif info.st_size > MODEL_RESPONSE_MAX:
                        result["status"] = "output_limit_exceeded"
                        result["overflow_streams"] = ["response_file"]
                    else:
                        raw = last.read_bytes()
                        if len(raw) > MODEL_RESPONSE_MAX:
                            result["status"] = "output_limit_exceeded"
                            result["overflow_streams"] = ["response_file"]
                        else:
                            (rowdir / "response.json").write_bytes(raw)
                            result["response_file_size_bytes_observed"] = len(raw)
                            result["response_file_sha256"] = digest(raw)
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
                                if len(candidate) > MAX_CANDIDATE_BYTES or b"\0" in candidate:
                                    result["status"] = "invalid_response"
                                else:
                                    result["status"] = "response_received"
                                    result["candidate_sha256"] = digest(candidate)
                                    result["candidate_utf8_bytes"] = len(candidate)
                                    (rowdir / "candidate.app.sh").write_bytes(candidate)
            output_tokens = result["provider_output_tokens"]
            result["output_tokens_exceeded_nominal_threshold"] = (
                output_tokens > TOKEN_MAX if output_tokens is not None else None)
        except (OSError, Fx17Error) as exc:
            result["status"] = "runner_error"
            result["runner_error"] = str(exc)
            (rowdir / "stderr.log").write_text(str(exc))
    result["finished_at"] = now()
    result["wall_seconds"] = round(time.monotonic() - start, 3)
    return result


def run(freeze_root, approval_path, controls_path, output, ack_schedule, ack_payloads):
    payloads, freeze, approval, controls, schedule_hash, payload_hash, approval_raw, registry = gate(
        freeze_root, approval_path, controls_path, ack_schedule, ack_payloads)
    if sys.platform != "darwin":
        raise Fx17Error("macOS subject isolation is required; no model calls started")
    verify_codex_binary()
    output = external_output(output, "private trial results")
    if output.exists():
        raise Fx17Error("Refusing to reuse a trial output directory")
    consumption = claim_campaign_consumption(
        registry, schedule_hash, payload_hash, approval_raw)
    output.mkdir(mode=0o700, parents=True)
    state = {"schema": "aethyme.fx17.results/v1", "assigned_denominator": 6,
             "original_core_denominator": 4, "supplemental_misleading_denominator": 2,
             "freeze": freeze, "approval_by": approval["approved_by"],
             "runtime_profile": runtime_profile(),
             "runtime_profile_sha256": runtime_profile_hash(),
             "runtime_contract_correction_sha256": RUNTIME_CORRECTION_SHA256,
             "approval_sha256": consumption["approval_sha256"],
             "campaign_consumption_key_sha256": consumption["campaign_consumption_key_sha256"],
             "consumption_marker_sha256": consumption["marker_sha256"],
             "controls_reviewer": controls["independent_review"]["reviewer"],
             "controls_runner_sha256": controls["runner_sha256"],
             "controls_harness_sha256": controls["control_harness_sha256"],
             "controls_profile_sha256": controls["subject_profile_template_sha256"],
             "controls_evidence_sha256": controls["evidence_sha256"],
             "started_at": now(),
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


def _run_subject(source, argv, timeout=2):
    if not isinstance(argv, (list, tuple)):
        raise Fx17Error("Subject argv must be a list or tuple of literal strings")
    args = list(argv)
    if sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists():
        raise Fx17Error("Subject runner requires macOS sandbox-exec")
    if (not isinstance(source, str) or "\0" in source
            or any(not isinstance(a, str) or "\0" in a for a in args)):
        raise Fx17Error("Source and argv must be NUL-free strings")
    if (isinstance(timeout, bool) or not isinstance(timeout, (int, float))
            or not math.isfinite(timeout) or timeout <= 0 or timeout > WALL):
        raise Fx17Error("Subject timeout must be in (0, 180]")
    source_bytes = source.encode("utf-8")
    if len(source_bytes) > MAX_CANDIDATE_BYTES:
        raise Fx17Error("Candidate source exceeds its byte limit")
    if len(args) > 64 or sum(len(arg.encode("utf-8")) for arg in args) > 256 * 1024:
        raise Fx17Error("Subject argv exceeds its argument-count or byte limit")
    # tempfile uses a private 0700 directory; only its read-only script path
    # is allowlisted inside the child. The shell gets no private-oracle path.
    with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="aethyme-fx17-subject-") as tmp:
        app = Path(tmp) / "app.sh"
        app.write_bytes(source_bytes)
        app.chmod(0o400)
        before_info = app.lstat()
        app_before = digest(app.read_bytes())
        app_text = str(app)
        if not app_text.isascii() or any(ch in app_text for ch in ('"', "\\", "\n")):
            raise Fx17Error("Unexpected subject path form")
        profile = SUBJECT_PROFILE.replace("__FX17_SUBJECT_APP__", app_text)
        cmd = ["/usr/bin/sandbox-exec", "-p", profile,
               "/bin/sh", app_text, *args]
        # On this host /bin/sh selects Bash; inheriting this private temp cwd
        # made its startup getcwd traverse denied ancestors and emit stderr.
        # The root directory entry is already allowed by the subject profile.
        proc = subprocess.Popen(cmd, cwd="/",
                                env={"LC_ALL": "C", "PATH": "/usr/bin:/bin"},
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, start_new_session=True,
                                close_fds=True)
        started = time.monotonic()
        capture = bounded_communicate(
            proc, None, timeout, SUBJECT_STDOUT_MAX, SUBJECT_STDERR_MAX)
        after_info = app.lstat()
        app_after = digest(app.read_bytes())
        app_stable = (stat.S_ISREG(after_info.st_mode)
                      and before_info.st_dev == after_info.st_dev
                      and before_info.st_ino == after_info.st_ino
                      and stat.S_IMODE(after_info.st_mode) == 0o400
                      and app_before == app_after)
        outcome = ("output_limit_exceeded" if capture["overflow_streams"] else
                   "timed_out" if capture["timed_out"] else
                   "stream_drain_timeout" if capture["stream_drain_timed_out"] else
                   "completed")
        return {"argv": args, "exit_code": proc.returncode,
                "stdout": capture["stdout"], "stderr": capture["stderr"],
                "outcome": outcome, "timed_out": capture["timed_out"],
                "overflow_streams": capture["overflow_streams"],
                "stdout_limit_bytes": SUBJECT_STDOUT_MAX,
                "stderr_limit_bytes": SUBJECT_STDERR_MAX,
                "stream_drain_timed_out": capture["stream_drain_timed_out"],
                "process_group_killed": capture["process_group_killed"],
                "process_reaped": capture["reaped"],
                "wall_seconds": round(time.monotonic() - started, 3),
                "sandboxed": True,
                "runner_sha256": runner_hash(),
                "profile": profile,
                "profile_sha256": digest(profile.encode("utf-8")),
                "profile_template_sha256": profile_hash(),
                "profile_allowlist_paths": [app_text],
                "app_path": app_text,
                "app_mode_before": stat.S_IMODE(before_info.st_mode),
                "app_mode_after": stat.S_IMODE(after_info.st_mode),
                "app_sha256_before": app_before,
                "app_sha256_after": app_after,
                "app_stable": app_stable}


def run_subject(source, argv, controls_path, timeout=2):
    """Run a candidate only after validating matching isolation evidence."""
    require_controls(controls_path)
    return _run_subject(source, argv, timeout=timeout)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("--fixture-root", type=Path, default=DEFAULT_FIXTURE)
    prep.add_argument("--briefs-root", type=Path, default=DEFAULT_BRIEFS)
    prep.add_argument("--output-root", type=Path, required=True)
    verify = sub.add_parser("verify")
    verify.add_argument("--freeze-root", type=Path, default=DEFAULT_FREEZE)
    controls = sub.add_parser("controls")
    controls.add_argument("--output", type=Path, required=True)
    controls.add_argument("--reviewer", required=True)
    controls.add_argument("--reviewed-runner-sha256", required=True)
    controls.add_argument("--reviewed-control-harness-sha256", required=True)
    controls.add_argument("--reviewed-profile-sha256", required=True)
    controls.add_argument("--acknowledge-local-control-execution", action="store_true")
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
        elif args.cmd == "verify":
            payloads, freeze, schedule, schedule_hash, payload_hash = verify_frozen(args.freeze_root)
            result = {"status": "verified", "assigned_denominator": len(payloads),
                      "freeze": freeze, "combined_schedule_sha256": schedule_hash,
                      "payloads_sha256": payload_hash}
        elif args.cmd == "controls":
            from fx17_controls import run_controls
            result = run_controls(
                args.output, args.reviewer, args.reviewed_runner_sha256,
                args.reviewed_control_harness_sha256, args.reviewed_profile_sha256,
                args.acknowledge_local_control_execution)
        else:
            result = run(args.freeze_root, args.approval, args.controls, args.output_root,
                         args.ack_schedule_sha256, args.ack_payloads_sha256)
        print(json.dumps(result, sort_keys=True))
        if args.cmd == "controls" and result.get("status") != "pass":
            return 2
        return 0
    except (Fx17Error, OSError, ValueError, json.JSONDecodeError) as exc:
        print(json.dumps({"error": str(exc)}, sort_keys=True), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
