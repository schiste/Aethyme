"""Unpaid local controls for the FX17 candidate isolation runner."""
from __future__ import annotations

import base64
import contextlib
import io
import json
import math
import os
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from unittest import mock

import run_fx17 as fx17


CONTROL_ARGV = ["white space\t", "$literal;*[]", "line one\nline two", "",
                "single'quote", "\\backslash"]
CONTROL_SOURCES = {
    "literal_argv": 'printf \'arg=<%s>\\n\' "$@"\nprintf \'stderr-sentinel\\n\' >&2\nexit 23\n',
    "read_canary": 'IFS= read -r secret < "$1" || exit $?\nprintf \'%s\\n\' "$secret"\n',
    "write_canary": 'printf \'%s\' "$2" > "$1"\n',
    "self_write": "printf 'self-write-marker\\n' > \"$0\"\n",
    "network": 'exec 3<>"/dev/tcp/127.0.0.1/$1"\nprintf \'%s\' "$2" >&3\n',
    "process_fork": "(printf 'fx17-child-process-marker\\n')\n",
    "foreign_exec": "exec /usr/bin/printf 'fx17-foreign-exec-marker\\n'\n",
    "recursive_exec": "exec /bin/sh -c 'while :; do :; done'\n",
}
CONTROL_PROBE_NAMES = set(CONTROL_SOURCES)
RECURSIVE_TIMEOUT = 0.25
CAPTURE_PROBE_NAMES = {
    "stdout_overflow", "stderr_overflow", "response_file_overflow",
    "process_tree_timeout", "process_tree_overflow",
}
CAPTURE_PROBE_LIMIT_BYTES = 64 * 1024
CAPTURE_PROBE_TIMEOUT = 3.0
PROCESS_TREE_TIMEOUT = 0.25
PROCESS_IDENTITY_WAIT = 3.0
CAPTURE_SOURCES = {
    "stdout_overflow": "import os\nblock = b'O' * 4096\nwhile True: os.write(1, block)\n",
    "stderr_overflow": "import os\nblock = b'E' * 4096\nwhile True: os.write(2, block)\n",
    "response_file_overflow": (
        "import os, sys, time\n"
        "with open(sys.argv[1], 'wb') as response:\n"
        "    response.write(b'R' * int(sys.argv[2]))\n"
        "    response.flush()\n"
        "    os.fsync(response.fileno())\n"
        "while True: time.sleep(60)\n"),
}
PROCESS_TREE_CHILD_SOURCE = (
    "import sys, time\n"
    "if len(sys.argv[1]) != 32: raise SystemExit(2)\n"
    "while True: time.sleep(60)\n")
PROCESS_TREE_SOURCE = (
    "import os, subprocess, sys, time\n"
    "pid_path, child_nonce, mode, leader_nonce = sys.argv[1:5]\n"
    "if len(child_nonce) != 32 or len(leader_nonce) != 32: raise SystemExit(2)\n"
    "child = subprocess.Popen([sys.executable, '-I', '-c', "
    + repr(PROCESS_TREE_CHILD_SOURCE) + ", child_nonce], "
    "env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'}, close_fds=True)\n"
    "with open(pid_path, 'x', encoding='ascii') as marker:\n"
    "    marker.write(f'{child.pid} {os.getpgid(child.pid)} {child_nonce}\\n')\n"
    "    marker.flush()\n"
    "    os.fsync(marker.fileno())\n"
    "if mode == 'overflow':\n"
    "    block = b'T' * 4096\n"
    "    while True: os.write(1, block)\n"
    "while True: time.sleep(60)\n")


def control_harness_hash():
    return fx17.digest(Path(__file__).read_bytes())


def _artifact(raw):
    return {"base64": base64.b64encode(raw).decode("ascii"),
            "bytes": len(raw), "sha256": fx17.digest(raw)}


def _artifact_bytes(record, name):
    value = record.get(name)
    if not isinstance(value, dict):
        raise fx17.Fx17Error("Control evidence is missing captured " + name)
    try:
        raw = base64.b64decode(value["base64"], validate=True)
    except (KeyError, ValueError, TypeError) as exc:
        raise fx17.Fx17Error("Control evidence has invalid captured " + name) from exc
    if value.get("bytes") != len(raw) or value.get("sha256") != fx17.digest(raw):
        raise fx17.Fx17Error("Control evidence captured " + name + " hash mismatch")
    return raw


def _denied(stderr):
    lowered = stderr.lower()
    return (b"operation not permitted" in lowered
            or b"permission denied" in lowered)


def _nonzero_exit(record):
    code = record.get("exit_code")
    return type(code) is int and code != 0


def _probe_record(source, argv, timeout, result):
    return {
        "source_sha256": fx17.digest(source.encode("utf-8")),
        "argv": list(argv),
        "timeout_seconds": timeout,
        "runner_sha256": result["runner_sha256"],
        "control_harness_sha256": control_harness_hash(),
        "profile_template_sha256": result["profile_template_sha256"],
        "profile_sha256": result["profile_sha256"],
        "profile": result["profile"],
        "profile_allowlist_paths": result["profile_allowlist_paths"],
        "app_path": result["app_path"],
        "app_mode_before": result["app_mode_before"],
        "app_mode_after": result["app_mode_after"],
        "app_sha256_before": result["app_sha256_before"],
        "app_sha256_after": result["app_sha256_after"],
        "app_stable": result["app_stable"],
        "exit_code": result["exit_code"],
        "outcome": result["outcome"],
        "timed_out": result["timed_out"],
        "overflow_streams": result["overflow_streams"],
        "stream_drain_timed_out": result["stream_drain_timed_out"],
        "process_group_killed": result["process_group_killed"],
        "process_reaped": result["process_reaped"],
        "stdout_limit_bytes": result["stdout_limit_bytes"],
        "stderr_limit_bytes": result["stderr_limit_bytes"],
        "wall_seconds": result["wall_seconds"],
        "stdout": _artifact(result["stdout"]),
        "stderr": _artifact(result["stderr"]),
    }


def _capture_probe_record(source, argv, timeout, result, proc,
                         stdout_limit=CAPTURE_PROBE_LIMIT_BYTES,
                         stderr_limit=CAPTURE_PROBE_LIMIT_BYTES,
                         watch_file_limit=None, watch_file_path=None,
                         wall_seconds=None):
    stdout = result["stdout"]
    stderr = result["stderr"]
    return {
        "source": source,
        "source_sha256": fx17.digest(source.encode("utf-8")),
        "argv": list(argv),
        "python_executable": sys.executable,
        "command_sha256": fx17.digest(fx17.jbytes(
            [sys.executable, "-I", "-c", source, *argv])),
        "runner_sha256": fx17.runner_hash(),
        "control_harness_sha256": control_harness_hash(),
        "timeout_seconds": timeout,
        "stdout_limit_bytes": stdout_limit,
        "stderr_limit_bytes": stderr_limit,
        "stdout_bytes_retained": len(stdout),
        "stderr_bytes_retained": len(stderr),
        "stdout": _artifact(stdout),
        "stderr": _artifact(stderr),
        "exit_code": proc.returncode,
        "overflow_streams": result["overflow_streams"],
        "timed_out": result["timed_out"],
        "stream_drain_timed_out": result["stream_drain_timed_out"],
        "process_group_killed": result["process_group_killed"],
        "process_reaped": result["reaped"],
        "wall_seconds": wall_seconds,
        "watch_file_limit_bytes": watch_file_limit,
        "watch_file_size_observed": (
            result["watched_file_sizes"].get("response_file")
            if watch_file_path is not None else None),
        "watch_file_name": Path(watch_file_path).name if watch_file_path else None,
    }


def _capture_environment():
    return {"PATH": "/usr/bin:/bin", "LC_ALL": "C"}


def _process_snapshot(pid):
    if type(pid) is not int or pid <= 0:
        raise fx17.Fx17Error("Capture probe attempted to inspect an invalid PID")
    try:
        result = subprocess.run(
            ["/bin/ps", "-p", str(pid), "-o", "pgid=,state=,command="],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            timeout=1.0, check=False, env=_capture_environment())
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise fx17.Fx17Error("Could not inspect an owned capture-probe process") from exc
    lines = result.stdout.splitlines()
    if not lines:
        if result.returncode == 1 and not result.stderr:
            return None
        raise fx17.Fx17Error("Process inspection could not establish whether the PID is gone")
    fields = lines[0].split(None, 2)
    if result.returncode != 0 or len(fields) != 3:
        raise fx17.Fx17Error("Process inspection returned an incomplete capture-probe identity")
    try:
        group = int(fields[0])
    except ValueError as exc:
        raise fx17.Fx17Error("Process inspection returned an invalid process-group ID") from exc
    return {"pid": pid, "process_group_id": group,
            "state": fields[1], "command": fields[2]}


def _snapshot_matches(snapshot, pid, process_group_id, nonce):
    return (isinstance(snapshot, dict)
            and type(snapshot.get("pid")) is int
            and snapshot.get("pid") == pid
            and type(snapshot.get("process_group_id")) is int
            and snapshot.get("process_group_id") == process_group_id
            and isinstance(snapshot.get("command"), str)
            and nonce in snapshot["command"])


def _wait_identity_gone(pid, process_group_id, nonce):
    started = time.monotonic()
    deadline = started + PROCESS_IDENTITY_WAIT
    last = None
    while True:
        last = _process_snapshot(pid)
        if last is None or nonce not in last["command"]:
            return True, round(time.monotonic() - started, 4), last
        if last["process_group_id"] != process_group_id:
            return False, round(time.monotonic() - started, 4), last
        now = time.monotonic()
        if now >= deadline:
            return False, round(now - started, 4), last
        time.sleep(min(0.025, deadline - now))


def _wait_owned_tree_ready(proc, marker_path, leader_nonce, child_nonce):
    deadline = time.monotonic() + PROCESS_IDENTITY_WAIT
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise fx17.Fx17Error("Synthetic owned process-tree leader exited before readiness")
        if marker_path.is_file() and not marker_path.is_symlink():
            try:
                fields = marker_path.read_text(encoding="ascii").strip().split()
                child_pid, recorded_group = int(fields[0]), int(fields[1])
            except (OSError, IndexError, ValueError) as exc:
                raise fx17.Fx17Error("Synthetic child identity record is invalid") from exc
            if len(fields) != 3 or fields[2] != child_nonce:
                raise fx17.Fx17Error("Synthetic child identity nonce did not match")
            try:
                leader_group = os.getpgid(proc.pid)
                child_group = os.getpgid(child_pid)
            except ProcessLookupError:
                time.sleep(0.01)
                continue
            leader = _process_snapshot(proc.pid)
            child = _process_snapshot(child_pid)
            if (proc.pid != leader_group or recorded_group != proc.pid
                    or child_group != proc.pid):
                raise fx17.Fx17Error("Synthetic process tree is outside its owned process group")
            if (_snapshot_matches(leader, proc.pid, proc.pid, leader_nonce)
                    and _snapshot_matches(child, child_pid, proc.pid, child_nonce)):
                return child_pid, leader, child
        time.sleep(0.01)
    raise fx17.Fx17Error("Synthetic owned process tree did not become observable in time")


def _cleanup_owned_group(proc):
    if proc.poll() is None:
        fx17.kill_tree(proc)
        try:
            proc.wait(timeout=fx17.REAP_GRACE_SECONDS)
        except subprocess.TimeoutExpired as exc:
            raise fx17.Fx17Error("Could not clean up a synthetic owned process group") from exc


def _owned_child_from_marker(marker_path, group_id, child_nonce):
    if not marker_path.is_file() or marker_path.is_symlink():
        return None
    try:
        fields = marker_path.read_text(encoding="ascii").strip().split()
        if len(fields) != 3 or fields[2] != child_nonce:
            return None
        child_pid, recorded_group = int(fields[0]), int(fields[1])
    except (OSError, UnicodeDecodeError, ValueError):
        return None
    if child_pid <= 0 or recorded_group != group_id:
        return None
    snapshot = _process_snapshot(child_pid)
    if _snapshot_matches(snapshot, child_pid, group_id, child_nonce):
        return child_pid
    return None


def _cleanup_owned_tree(proc, child_pid, child_nonce):
    if proc.poll() is None:
        fx17.kill_tree(proc)
    elif (type(child_pid) is int
          and _snapshot_matches(_process_snapshot(child_pid), child_pid,
                               proc.pid, child_nonce)):
        # The leader has exited, so kill only after the recorded child still
        # proves membership in the process group this harness created.
        fx17.kill_tree(proc)
    try:
        proc.wait(timeout=fx17.REAP_GRACE_SECONDS)
    except subprocess.TimeoutExpired as exc:
        raise fx17.Fx17Error("Could not reap a synthetic process-tree leader") from exc
    if type(child_pid) is int:
        gone, _, _ = _wait_identity_gone(child_pid, proc.pid, child_nonce)
        if not gone:
            raise fx17.Fx17Error("Could not clean up the synthetic owned child process")


def _run_capture_probe(root, name):
    source = CAPTURE_SOURCES[name]
    stdout_limit = CAPTURE_PROBE_LIMIT_BYTES
    stderr_limit = CAPTURE_PROBE_LIMIT_BYTES
    watch_file = None
    watch_paths = None
    if name == "response_file_overflow":
        watch_file = root / "synthetic-response.json"
        argv = [str(watch_file), str(CAPTURE_PROBE_LIMIT_BYTES + 1)]
        watch_paths = {"response_file": (watch_file, CAPTURE_PROBE_LIMIT_BYTES)}
    else:
        argv = []
    cmd = [sys.executable, "-I", "-c", source, *argv]
    proc = subprocess.Popen(
        cmd, cwd=root, env=_capture_environment(), stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
        close_fds=True)
    started = time.monotonic()
    try:
        result = fx17.bounded_communicate(
            proc, None, CAPTURE_PROBE_TIMEOUT, stdout_limit, stderr_limit,
            watch_paths=watch_paths)
    finally:
        _cleanup_owned_group(proc)
    return _capture_probe_record(
        source, argv, CAPTURE_PROBE_TIMEOUT, result, proc,
        stdout_limit, stderr_limit,
        CAPTURE_PROBE_LIMIT_BYTES if watch_file is not None else None,
        watch_file, round(time.monotonic() - started, 4))


def _run_process_tree_probe(root, mode):
    leader_nonce = secrets.token_hex(16)
    child_nonce = secrets.token_hex(16)
    marker_path = root / ("owned-child-" + mode + ".pid")
    argv = [str(marker_path), child_nonce, mode, leader_nonce]
    cmd = [sys.executable, "-I", "-c", PROCESS_TREE_SOURCE, *argv]
    proc = subprocess.Popen(
        cmd, cwd=root, env=_capture_environment(), stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
        close_fds=True)
    started = time.monotonic()
    child_pid = None
    try:
        child_pid, leader_before, child_before = _wait_owned_tree_ready(
            proc, marker_path, leader_nonce, child_nonce)
        timeout = PROCESS_TREE_TIMEOUT if mode == "timeout" else CAPTURE_PROBE_TIMEOUT
        result = fx17.bounded_communicate(
            proc, None, timeout, CAPTURE_PROBE_LIMIT_BYTES,
            CAPTURE_PROBE_LIMIT_BYTES)
        leader_gone, leader_wait, leader_after = _wait_identity_gone(
            proc.pid, proc.pid, leader_nonce)
        child_gone, child_wait, child_after = _wait_identity_gone(
            child_pid, proc.pid, child_nonce)
    finally:
        if child_pid is None:
            child_pid = _owned_child_from_marker(marker_path, proc.pid, child_nonce)
        _cleanup_owned_tree(proc, child_pid, child_nonce)
    record = _capture_probe_record(
        PROCESS_TREE_SOURCE, argv, timeout, result, proc,
        CAPTURE_PROBE_LIMIT_BYTES, CAPTURE_PROBE_LIMIT_BYTES,
        wall_seconds=round(time.monotonic() - started, 4))
    record["process_tree"] = {
        "process_group_id": proc.pid,
        "leader_pid": proc.pid,
        "leader_nonce": leader_nonce,
        "leader_before": leader_before,
        "leader_after": leader_after,
        "leader_identity_gone": leader_gone,
        "leader_gone_wait_seconds": leader_wait,
        "child_pid": child_pid,
        "child_nonce": child_nonce,
        "child_before": child_before,
        "child_after": child_after,
        "child_identity_gone": child_gone,
        "child_gone_wait_seconds": child_wait,
    }
    return record


def _run_unpaid_capture_probes(root):
    return {
        "stdout_overflow": _run_capture_probe(root, "stdout_overflow"),
        "stderr_overflow": _run_capture_probe(root, "stderr_overflow"),
        "response_file_overflow": _run_capture_probe(root, "response_file_overflow"),
        "process_tree_timeout": _run_process_tree_probe(root, "timeout"),
        "process_tree_overflow": _run_process_tree_probe(root, "overflow"),
    }


def _write_control_report(output, report):
    report["evidence_sha256"] = fx17.digest(fx17.jbytes(report))
    raw = fx17.jbytes(report)
    fd = os.open(output, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    return raw


def _run_failed_cli_negative_control(root):
    output = root / "synthetic-controls-cli-failure.json"
    failure = "synthetic unpaid control failure used to check CLI exit behavior"
    trial_calls = []

    def failed_controls_runner(path, reviewer, reviewed_runner, reviewed_harness,
                               reviewed_profile, acknowledge):
        report = {
            "schema": "aethyme.fx17.independent-isolation-controls/v3",
            "status": "fail",
            "created_at": fx17.now(),
            "platform": "darwin",
            "runner_sha256": fx17.runner_hash(),
            "control_harness_sha256": control_harness_hash(),
            "subject_profile_template_sha256": fx17.profile_hash(),
            "independent_review": {
                "reviewer": reviewer,
                "reviewed_at": fx17.now(),
                "review_acknowledged": acknowledge,
                "runner_sha256": reviewed_runner,
                "control_harness_sha256": reviewed_harness,
                "subject_profile_template_sha256": reviewed_profile,
            },
            "canaries": {}, "probes": {}, "capture_probes": {},
            "approval_consumption_negative_control": {},
            "cli_failure_negative_control": {},
            "failures": [failure],
        }
        raw = _write_control_report(path, report)
        persisted_report = json.loads(raw)
        return {"output": str(path), "status": "fail", "failures": [failure],
                "evidence_sha256": persisted_report["evidence_sha256"]}

    def unexpected_trial(*args, **kwargs):
        trial_calls.append(True)
        raise fx17.Fx17Error("The failed-controls negative control reached the trial runner")

    argv = [
        "run_fx17.py", "controls", "--output", str(output),
        "--reviewer", "synthetic-negative-control",
        "--reviewed-runner-sha256", fx17.runner_hash(),
        "--reviewed-control-harness-sha256", control_harness_hash(),
        "--reviewed-profile-sha256", fx17.profile_hash(),
        "--acknowledge-local-control-execution",
    ]
    stdout, stderr = io.StringIO(), io.StringIO()
    module = sys.modules[__name__]
    try:
        with (mock.patch.object(module, "run_controls", failed_controls_runner),
              mock.patch.object(fx17, "run", unexpected_trial),
              mock.patch.object(sys, "argv", argv),
              contextlib.redirect_stdout(stdout),
              contextlib.redirect_stderr(stderr)):
            raise SystemExit(fx17.main())
    except SystemExit as exc:
        exit_code = exc.code
    if not output.is_file() or output.is_symlink():
        raise fx17.Fx17Error("Failed-controls negative control did not persist its full report")
    persisted = output.read_bytes()
    return {
        "exit_code": exit_code,
        "stdout": _artifact(stdout.getvalue().encode("utf-8")),
        "stderr": _artifact(stderr.getvalue().encode("utf-8")),
        "persisted_report": _artifact(persisted),
        "trial_runner_calls": len(trial_calls),
    }


def _run_consumption_registry_negative_control(root):
    registry = root / "campaign-consumption-registry"
    registry.mkdir(mode=0o700)
    schedule_hash = "a" * 64
    payload_hash = "b" * 64
    first_path = root / "approval-original.json"
    copied_path = root / "approval-copy.json"
    first_approval = {"approved_by": "synthetic-original",
                      "consumption_registry_path": str(registry)}
    copied_approval = {"approved_by": "synthetic-copy",
                       "consumption_registry_path": str(registry)}
    fx17.save_json(first_path, first_approval)
    fx17.save_json(copied_path, copied_approval)
    first_raw = first_path.read_bytes()
    copied_raw = copied_path.read_bytes()
    first_claim = fx17.claim_campaign_consumption(
        first_approval["consumption_registry_path"], schedule_hash,
        payload_hash, first_raw)
    marker = registry / ("fx17-" + first_claim["campaign_consumption_key_sha256"]
                         + ".consumed.json")
    before_failure = marker.read_bytes()
    try:
        raise fx17.Fx17Error("synthetic unpaid interruption after campaign consumption")
    except fx17.Fx17Error as exc:
        simulated_failure = str(exc)
    try:
        fx17.claim_campaign_consumption(
            copied_approval["consumption_registry_path"], schedule_hash,
            payload_hash, copied_raw)
    except fx17.Fx17Error as exc:
        duplicate_error = str(exc)
    else:
        duplicate_error = ""
    after_retry = marker.read_bytes()
    return {
        "schedule_sha256": schedule_hash,
        "payloads_sha256": payload_hash,
        "campaign_consumption_key_sha256": first_claim["campaign_consumption_key_sha256"],
        "registry_path": str(registry),
        "original_approval_path": str(first_path),
        "copied_approval_path": str(copied_path),
        "original_approval_sha256": fx17.digest(first_raw),
        "copied_approval_sha256": fx17.digest(copied_raw),
        "marker_before_retry": _artifact(before_failure),
        "marker_after_retry": _artifact(after_retry),
        "simulated_failure": simulated_failure,
        "duplicate_error": duplicate_error,
    }


def _capture_probe_findings(capture_probes):
    failures = []
    if not isinstance(capture_probes, dict) or set(capture_probes) != CAPTURE_PROBE_NAMES:
        return ["trusted capture stress probes are incomplete or unexpected"]
    expected_sources = dict(CAPTURE_SOURCES)
    expected_sources["process_tree_timeout"] = PROCESS_TREE_SOURCE
    expected_sources["process_tree_overflow"] = PROCESS_TREE_SOURCE
    for name, record in capture_probes.items():
        if not isinstance(record, dict):
            failures.append(name + " capture probe record is invalid")
            continue
        source = expected_sources[name]
        if (record.get("source") != source
                or record.get("source_sha256") != fx17.digest(source.encode("utf-8"))
                or record.get("runner_sha256") != fx17.runner_hash()
                or record.get("control_harness_sha256") != control_harness_hash()):
            failures.append(name + " capture probe source or code hashes differ")
        executable = record.get("python_executable")
        argv = record.get("argv")
        if (not isinstance(executable, str) or not Path(executable).is_absolute()
                or not isinstance(argv, list) or any(not isinstance(arg, str) for arg in argv)
                or record.get("command_sha256") != fx17.digest(fx17.jbytes(
                    [executable, "-I", "-c", source, *argv]))):
            failures.append(name + " capture command evidence is invalid")
        if name in ("stdout_overflow", "stderr_overflow") and argv != []:
            failures.append(name + " capture probe argv is unexpected")
        if name == "response_file_overflow":
            if (not isinstance(argv, list) or len(argv) != 2
                    or not argv[0].startswith("/private/tmp/aethyme-fx17-control-canaries-")
                    or Path(argv[0]).name != "synthetic-response.json"
                    or argv[1] != str(CAPTURE_PROBE_LIMIT_BYTES + 1)):
                failures.append("response-file overflow probe inputs are invalid")
        if name.startswith("process_tree_"):
            tree = record.get("process_tree")
            expected_mode = "timeout" if name == "process_tree_timeout" else "overflow"
            if (not isinstance(tree, dict) or not isinstance(argv, list) or len(argv) != 4
                    or argv[2] != expected_mode
                    or argv[0] != str(Path(argv[0]))
                    or not argv[0].startswith("/private/tmp/aethyme-fx17-control-canaries-")
                    or not argv[0].endswith("owned-child-" + expected_mode + ".pid")
                    or argv[1] != tree.get("child_nonce")
                    or argv[3] != tree.get("leader_nonce")):
                failures.append(name + " owned process-tree inputs are invalid")
                tree = None
            if tree is not None:
                leader_pid = tree.get("leader_pid")
                child_pid = tree.get("child_pid")
                group = tree.get("process_group_id")
                leader_nonce = tree.get("leader_nonce")
                child_nonce = tree.get("child_nonce")
                if (type(leader_pid) is not int or type(child_pid) is not int
                        or type(group) is not int or leader_pid <= 0 or child_pid <= 0
                        or leader_pid == child_pid or group != leader_pid
                        or not isinstance(leader_nonce, str) or len(leader_nonce) != 32
                        or not isinstance(child_nonce, str) or len(child_nonce) != 32
                        or not _snapshot_matches(tree.get("leader_before"), leader_pid,
                                                 group, leader_nonce)
                        or not _snapshot_matches(tree.get("child_before"), child_pid,
                                                 group, child_nonce)
                        or tree.get("leader_identity_gone") is not True
                        or tree.get("child_identity_gone") is not True):
                    failures.append(name + " did not observe its owned leader and child identities")
                for identity, nonce in (("leader", leader_nonce), ("child", child_nonce)):
                    after = tree.get(identity + "_after")
                    waited = tree.get(identity + "_gone_wait_seconds")
                    if (after is not None and (not isinstance(after, dict)
                            or (isinstance(nonce, str)
                                and nonce in after.get("command", "")))):
                        failures.append(name + " still has the owned " + identity + " process")
                    if (isinstance(waited, bool) or not isinstance(waited, (int, float))
                            or not math.isfinite(waited) or waited < 0
                            or waited > PROCESS_IDENTITY_WAIT):
                        failures.append(name + " process identity wait exceeded its bound")
        try:
            stdout = _artifact_bytes(record, "stdout")
            stderr = _artifact_bytes(record, "stderr")
        except fx17.Fx17Error as exc:
            failures.append(str(exc))
            continue
        stdout_limit = record.get("stdout_limit_bytes")
        stderr_limit = record.get("stderr_limit_bytes")
        if (type(stdout_limit) is not int or stdout_limit != CAPTURE_PROBE_LIMIT_BYTES
                or type(stderr_limit) is not int or stderr_limit != CAPTURE_PROBE_LIMIT_BYTES
                or type(record.get("stdout_bytes_retained")) is not int
                or record.get("stdout_bytes_retained") != len(stdout)
                or type(record.get("stderr_bytes_retained")) is not int
                or record.get("stderr_bytes_retained") != len(stderr)
                or len(stdout) > CAPTURE_PROBE_LIMIT_BYTES
                or len(stderr) > CAPTURE_PROBE_LIMIT_BYTES):
            failures.append(name + " retained more than its per-stream capture bound")
        if (type(record.get("exit_code")) is not int
                or record.get("exit_code") != -signal.SIGKILL
                or record.get("process_group_killed") is not True
                or record.get("process_reaped") is not True
                or record.get("stream_drain_timed_out") is not False):
            failures.append(name + " did not kill its process group and reap the leader")
        wall = record.get("wall_seconds")
        timeout = record.get("timeout_seconds")
        if (isinstance(wall, bool) or not isinstance(wall, (int, float))
                or not math.isfinite(wall) or isinstance(timeout, bool)
                or not isinstance(timeout, (int, float)) or not math.isfinite(timeout)
                or wall > timeout + fx17.REAP_GRACE_SECONDS + 1):
            failures.append(name + " exceeded its bounded capture deadline")
        expected_overflow = {
            "stdout_overflow": ["stdout"],
            "stderr_overflow": ["stderr"],
            "response_file_overflow": ["response_file"],
            "process_tree_timeout": [],
            "process_tree_overflow": ["stdout"],
        }[name]
        if (record.get("overflow_streams") != expected_overflow
                or record.get("timed_out") is not (name == "process_tree_timeout")):
            failures.append(name + " did not observe its typed overflow or timeout outcome")
        if name == "stdout_overflow" and (len(stdout) != stdout_limit or stderr):
            failures.append("stdout overflow did not retain exactly its cap")
        if name == "stderr_overflow" and (len(stderr) != stderr_limit or stdout):
            failures.append("stderr overflow did not retain exactly its cap")
        if name == "response_file_overflow":
            observed = record.get("watch_file_size_observed")
            threshold = record.get("watch_file_limit_bytes")
            if (record.get("watch_file_name") != "synthetic-response.json"
                    or type(threshold) is not int or threshold != CAPTURE_PROBE_LIMIT_BYTES
                    or type(observed) is not int or observed <= threshold
                    or stdout or stderr):
                failures.append("watched response-file threshold was not exceeded and observed")
        elif (record.get("watch_file_limit_bytes") is not None
              or record.get("watch_file_size_observed") is not None
              or record.get("watch_file_name") is not None):
            failures.append(name + " unexpectedly reports a watched response file")
    return failures


def _negative_control_findings(report):
    failures = []
    cli = report.get("cli_failure_negative_control")
    registry = report.get("approval_consumption_negative_control")
    if not isinstance(cli, dict):
        failures.append("failed-controls CLI negative control is missing")
    else:
        try:
            persisted = _artifact_bytes(cli, "persisted_report")
            stdout = _artifact_bytes(cli, "stdout")
            stderr = _artifact_bytes(cli, "stderr")
            persisted_report = json.loads(persisted)
            stdout_result = json.loads(stdout)
        except (fx17.Fx17Error, UnicodeDecodeError, json.JSONDecodeError) as exc:
            failures.append("failed-controls CLI negative control evidence is invalid: " + str(exc))
        else:
            if (cli.get("exit_code") != 2 or type(cli.get("exit_code")) is not int
                    or cli.get("trial_runner_calls") != 0
                    or type(cli.get("trial_runner_calls")) is not int
                    or stderr != b""
                    or persisted_report.get("schema") != "aethyme.fx17.independent-isolation-controls/v3"
                    or persisted_report.get("status") != "fail"
                    or not persisted_report.get("failures")
                    or stdout_result.get("status") != "fail"):
                failures.append("failed-controls CLI did not persist its report and exit nonzero")
            unhashed_report = dict(persisted_report)
            report_hash = unhashed_report.pop("evidence_sha256", None)
            if report_hash != fx17.digest(fx17.jbytes(unhashed_report)):
                failures.append("failed-controls CLI persisted an invalid report hash")
    if not isinstance(registry, dict):
        failures.append("campaign-consumption registry negative control is missing")
    else:
        schedule_hash = registry.get("schedule_sha256")
        payload_hash = registry.get("payloads_sha256")
        key = registry.get("campaign_consumption_key_sha256")
        try:
            before = _artifact_bytes(registry, "marker_before_retry")
            after = _artifact_bytes(registry, "marker_after_retry")
            marker_record = json.loads(before)
        except (fx17.Fx17Error, UnicodeDecodeError, json.JSONDecodeError) as exc:
            failures.append("campaign-consumption marker evidence is invalid: " + str(exc))
        else:
            first_path = registry.get("original_approval_path")
            copied_path = registry.get("copied_approval_path")
            registry_path = registry.get("registry_path")
            if (not isinstance(first_path, str) or not isinstance(copied_path, str)
                    or first_path == copied_path
                    or not isinstance(registry_path, str)
                    or not registry_path.startswith("/private/tmp/aethyme-fx17-control-canaries-")
                    or registry.get("original_approval_sha256") == registry.get("copied_approval_sha256")
                    or marker_record.get("schema") != "aethyme.fx17.consumed-campaign/v1"
                    or marker_record.get("campaign_consumption_key_sha256") != key
                    or marker_record.get("combined_schedule_sha256") != schedule_hash
                    or marker_record.get("payloads_sha256") != payload_hash
                    or key != fx17.campaign_consumption_key(schedule_hash, payload_hash)
                    or registry.get("simulated_failure", "").find("interruption") < 0
                    or "already consumed" not in registry.get("duplicate_error", "")
                    or before != after):
                failures.append("copied approval path bypassed the one-shot campaign registry")
    return failures


def controls_findings(report):
    failures = []
    if (report.get("schema") != "aethyme.fx17.independent-isolation-controls/v3"
            or report.get("platform") != "darwin"
            or not isinstance(report.get("created_at"), str)):
        failures.append("control report schema, platform, or timestamp is invalid")
    if report.get("runner_sha256") != fx17.runner_hash():
        failures.append("subject runner hash differs")
    if report.get("control_harness_sha256") != control_harness_hash():
        failures.append("control harness hash differs")
    if report.get("subject_profile_template_sha256") != fx17.profile_hash():
        failures.append("subject profile template hash differs")
    review = report.get("independent_review")
    if not isinstance(review, dict):
        failures.append("independent review record is missing")
    else:
        if not isinstance(review.get("reviewer"), str) or not review["reviewer"].strip():
            failures.append("independent reviewer is not identified")
        if (review.get("review_acknowledged") is not True
                or not isinstance(review.get("reviewed_at"), str)
                or review.get("reviewed_at", "") > report.get("created_at", "")):
            failures.append("independent review was not acknowledged before control execution")
        if (review.get("runner_sha256") != fx17.runner_hash()
                or review.get("control_harness_sha256") != control_harness_hash()
                or review.get("subject_profile_template_sha256") != fx17.profile_hash()):
            failures.append("independent review does not bind the current code and profile")

    probes = report.get("probes")
    if not isinstance(probes, dict) or set(probes) != CONTROL_PROBE_NAMES:
        failures.append("control probes are incomplete or unexpected")
        return failures
    failures.extend(_capture_probe_findings(report.get("capture_probes")))
    failures.extend(_negative_control_findings(report))
    canaries = report.get("canaries")
    if not isinstance(canaries, dict):
        failures.append("canary observations are missing")
        return failures
    nonce = canaries.get("read_nonce")
    network_nonce = canaries.get("network_nonce")
    read_file = canaries.get("read_file")
    write_file = canaries.get("write_file")
    network = canaries.get("network")
    foreign_target = canaries.get("foreign_exec_target")
    if (not isinstance(nonce, str) or len(nonce) < 32 or not nonce.isascii()
            or not isinstance(network_nonce, str) or len(network_nonce) < 32 or not network_nonce.isascii()
            or not all(isinstance(value, dict) for value in
                       (read_file, write_file, network, foreign_target))):
        failures.append("canary state records are incomplete")
        return failures
    if (not isinstance(read_file.get("path"), str)
            or not isinstance(write_file.get("path"), str)):
        failures.append("canary file paths are invalid")
        return failures
    read_path = Path(read_file.get("path", ""))
    write_path = Path(write_file.get("path", ""))
    if (read_path.parent != write_path.parent
            or not str(read_path.parent).startswith("/private/tmp/aethyme-fx17-control-canaries-")
            or read_path.name != "private-read-canary.txt"
            or write_path.name != "write-canary.txt"):
        failures.append("canary paths are outside the private control directory")
    nonce_bytes = nonce.encode("ascii", "strict")
    network_nonce_bytes = network_nonce.encode("ascii", "strict")
    expected_argv = {
        "literal_argv": CONTROL_ARGV,
        "read_canary": [read_file.get("path")],
        "write_canary": [write_file.get("path"), "FX17-REPLACE-CANARY"],
        "self_write": [],
        "network": [str(network.get("port")), network_nonce],
        "process_fork": [],
        "foreign_exec": [],
        "recursive_exec": [],
    }
    decoded = {}
    for name, record in probes.items():
        if not isinstance(record, dict):
            failures.append(name + " probe record is invalid")
            continue
        if record.get("source_sha256") != fx17.digest(CONTROL_SOURCES[name].encode("utf-8")):
            failures.append(name + " probe source is not the reviewed control")
        if record.get("argv") != expected_argv[name]:
            failures.append(name + " probe argv differs from its frozen control input")
        app_path = record.get("app_path")
        if (not isinstance(app_path, str)
                or not app_path.startswith("/private/tmp/aethyme-fx17-subject-")
                or not app_path.endswith("/app.sh")):
            failures.append(name + " app path is outside the private subject directory")
            continue
        expected_profile = (fx17.digest(fx17.SUBJECT_PROFILE.replace(
            "__FX17_SUBJECT_APP__", app_path).encode("utf-8")))
        if (record.get("runner_sha256") != fx17.runner_hash()
                or record.get("control_harness_sha256") != control_harness_hash()
                or record.get("profile_template_sha256") != fx17.profile_hash()
                or record.get("profile_sha256") != expected_profile
                or record.get("profile") != fx17.SUBJECT_PROFILE.replace(
                    "__FX17_SUBJECT_APP__", app_path)
                or record.get("profile_allowlist_paths") != [app_path]):
            failures.append(name + " probe profile or runner hash does not match")
        if (record.get("app_mode_before") != 0o400
                or record.get("app_mode_after") != 0o400
                or record.get("app_sha256_before") != record.get("app_sha256_after")
                or record.get("app_stable") is not True):
            failures.append(name + " candidate file was not read-only and unchanged")
        expected_outcome = "timed_out" if name == "recursive_exec" else "completed"
        expected_timeout = name == "recursive_exec"
        if (record.get("outcome") != expected_outcome
                or type(record.get("exit_code")) is not int
                or record.get("process_reaped") is not True
                or record.get("overflow_streams") != []
                or record.get("stream_drain_timed_out") is not False
                or record.get("timed_out") is not expected_timeout
                or record.get("process_group_killed") is not expected_timeout):
            failures.append(name + " probe did not finish with bounded, complete capture")
        try:
            out = _artifact_bytes(record, "stdout")
            err = _artifact_bytes(record, "stderr")
        except fx17.Fx17Error as exc:
            failures.append(str(exc))
            continue
        if (len(out) > fx17.SUBJECT_STDOUT_MAX or len(err) > fx17.SUBJECT_STDERR_MAX
                or record.get("stdout_limit_bytes") != fx17.SUBJECT_STDOUT_MAX
                or record.get("stderr_limit_bytes") != fx17.SUBJECT_STDERR_MAX):
            failures.append(name + " captured stream limit evidence is invalid")
        decoded[name] = (out, err)

    if "literal_argv" in decoded:
        out, err = decoded["literal_argv"]
        expected_stdout = b"".join(
            b"arg=<" + value.encode("utf-8") + b">\n" for value in CONTROL_ARGV)
        if (out != expected_stdout or err != b"stderr-sentinel\n"
                or probes["literal_argv"].get("exit_code") != 23
                or probes["literal_argv"].get("outcome") != "completed"):
            failures.append("literal argv, stdout, stderr, or exit observation failed")

    if "read_canary" in decoded:
        out, err = decoded["read_canary"]
        expected_sha = fx17.digest(nonce_bytes + b"\n")
        if (nonce_bytes in out or nonce_bytes in err or not _denied(err)
                or probes["read_canary"].get("exit_code") in (None, 0)
                or read_file.get("before_sha256") != expected_sha
                or read_file.get("after_sha256") != expected_sha
                or read_file.get("harness_readback_sha256") != expected_sha
                or read_file.get("harness_readable") is not True):
            failures.append("read canary was exposed or the harness did not verify it")

    if "write_canary" in decoded:
        out, err = decoded["write_canary"]
        expected_sha = fx17.digest(b"FX17-WRITE-CANARY-UNCHANGED\n")
        if (not _denied(err) or not _nonzero_exit(probes["write_canary"])
                or b"FX17-REPLACE-CANARY" in out
                or write_file.get("before_sha256") != expected_sha
                or write_file.get("after_sha256") != expected_sha
                or write_file.get("exists_after") is not True
                or write_file.get("bytes_after") != len(b"FX17-WRITE-CANARY-UNCHANGED\n")):
            failures.append("write canary changed or denial was not observed")

    if ("self_write" in decoded
            and (not _denied(decoded["self_write"][1])
                 or not _nonzero_exit(probes["self_write"]))):
        failures.append("candidate self-write denial was not observed")

    if "network" in decoded:
        out, err = decoded["network"]
        port = network.get("port")
        expected_positive_bytes = network.get("positive_control_expected_bytes")
        if (not _denied(err) or network_nonce_bytes in out or network_nonce_bytes in err
                or network.get("address") != "127.0.0.1"
                or isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535
                or network.get("listener_ready") is not True
                or network.get("positive_control_received") is not True
                or isinstance(expected_positive_bytes, bool)
                or not isinstance(expected_positive_bytes, int)
                or expected_positive_bytes <= 0
                or network.get("positive_control_received_bytes") != network.get("positive_control_expected_bytes")
                or network.get("positive_control_received_sha256") != network.get("positive_control_expected_sha256")
                or not _nonzero_exit(probes["network"])
                or network.get("candidate_connections") != []):
            failures.append("network denial or listener positive/negative control failed")

    fork_marker = b"fx17-child-process-marker\n"
    if "process_fork" in decoded:
        out, err = decoded["process_fork"]
        if (fork_marker in out or not _denied(err)
                or not _nonzero_exit(probes["process_fork"])):
            failures.append("process-fork canary produced a marker or no denial was observed")

    exec_marker = b"fx17-foreign-exec-marker\n"
    if "foreign_exec" in decoded:
        out, err = decoded["foreign_exec"]
        target_bytes = foreign_target.get("bytes")
        if (exec_marker in out or not _denied(err)
                or not _nonzero_exit(probes["foreign_exec"])
                or foreign_target.get("path") != "/usr/bin/printf"
                or foreign_target.get("exists") is not True
                or foreign_target.get("executable") is not True
                or not isinstance(foreign_target.get("sha256"), str)
                or isinstance(target_bytes, bool) or not isinstance(target_bytes, int)
                or target_bytes <= 0
                or foreign_target.get("sha256") != fx17.digest(Path("/usr/bin/printf").read_bytes())
                or target_bytes != Path("/usr/bin/printf").stat().st_size):
            failures.append("foreign-executable canary produced a marker or no denial was observed")

    recursive = probes.get("recursive_exec")
    if not isinstance(recursive, dict):
        failures.append("recursive shell control record is invalid")
        return failures
    if (recursive.get("outcome") != "timed_out"
            or recursive.get("timed_out") is not True
            or recursive.get("process_group_killed") is not True
            or recursive.get("process_reaped") is not True
            or recursive.get("exit_code") != -signal.SIGKILL
            or isinstance(recursive.get("wall_seconds"), bool)
            or not isinstance(recursive.get("wall_seconds"), (int, float))
            or not math.isfinite(recursive.get("wall_seconds"))
            or recursive.get("wall_seconds") > RECURSIVE_TIMEOUT + fx17.REAP_GRACE_SECONDS + 1):
        failures.append("recursive shell exec was not killed and reaped at its wall deadline")
    return failures


def verify_controls_evidence(report):
    if report.get("status") != "pass":
        raise fx17.Fx17Error("Independent effects/isolation controls have not passed")
    unhashed = dict(report)
    evidence_hash = unhashed.pop("evidence_sha256", None)
    if evidence_hash != fx17.digest(fx17.jbytes(unhashed)):
        raise fx17.Fx17Error("Isolation-control evidence hash mismatch")
    failures = controls_findings(report)
    if failures:
        raise fx17.Fx17Error("Isolation-control evidence is incomplete: " + "; ".join(failures))


def run_controls(output, reviewer, reviewed_runner, reviewed_harness,
                 reviewed_profile, acknowledge):
    if not acknowledge:
        raise fx17.Fx17Error("Controls require acknowledgement after independent review")
    if sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists():
        raise fx17.Fx17Error("Isolation controls require macOS sandbox-exec")
    current_runner = fx17.runner_hash()
    current_harness = control_harness_hash()
    current_profile = fx17.profile_hash()
    if (reviewed_runner != current_runner or reviewed_harness != current_harness
            or reviewed_profile != current_profile):
        raise fx17.Fx17Error("Reviewed runner/harness/profile hashes do not match this code")
    if not isinstance(reviewer, str) or not reviewer.strip():
        raise fx17.Fx17Error("An independent reviewer name is required")
    foreign = Path("/usr/bin/printf")
    if not foreign.is_file():
        raise fx17.Fx17Error("Foreign-exec control target is missing")
    reviewed_at = fx17.now()

    output = Path(output).expanduser()
    if output.exists() or output.is_symlink():
        raise fx17.Fx17Error("Refusing to overwrite control evidence")
    output = output.resolve()
    repo_root = fx17.HERE.parents[3].resolve(strict=True)
    try:
        output.relative_to(repo_root)
    except ValueError:
        pass
    else:
        raise fx17.Fx17Error("Keep private control evidence outside the repository")
    output.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(dir="/private/tmp",
                                     prefix="aethyme-fx17-control-canaries-") as tmp:
        root = Path(tmp)
        read_nonce = secrets.token_hex(24)
        network_nonce = secrets.token_hex(24)
        read_path = root / "private-read-canary.txt"
        write_path = root / "write-canary.txt"
        read_content = (read_nonce + "\n").encode("ascii")
        write_content = b"FX17-WRITE-CANARY-UNCHANGED\n"
        read_path.write_bytes(read_content)
        read_path.chmod(0o600)
        write_path.write_bytes(write_content)
        write_path.chmod(0o600)
        read_before = fx17.digest(read_path.read_bytes())
        write_before = fx17.digest(write_path.read_bytes())

        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
            server.bind(("127.0.0.1", 0))
            server.listen(8)
            server.settimeout(1.0)
            port = server.getsockname()[1]
            positive = ("FX17-HARNESS-POSITIVE-" + secrets.token_hex(12)).encode("ascii")
            with socket.create_connection(("127.0.0.1", port), timeout=1.0) as client:
                client.sendall(positive)
            connection, _ = server.accept()
            with connection:
                connection.settimeout(1.0)
                chunks = []
                received_length = 0
                while received_length < len(positive):
                    chunk = connection.recv(len(positive) - received_length)
                    if not chunk:
                        break
                    chunks.append(chunk)
                    received_length += len(chunk)
                positive_received = b"".join(chunks)
            network_ready = positive_received == positive

            probe_inputs = {
                "literal_argv": (CONTROL_SOURCES["literal_argv"], CONTROL_ARGV, 2.0),
                "read_canary": (CONTROL_SOURCES["read_canary"], [str(read_path)], 2.0),
                "write_canary": (CONTROL_SOURCES["write_canary"],
                                 [str(write_path), "FX17-REPLACE-CANARY"], 2.0),
                "self_write": (CONTROL_SOURCES["self_write"], [], 2.0),
                "network": (CONTROL_SOURCES["network"], [str(port), network_nonce], 2.0),
                "process_fork": (CONTROL_SOURCES["process_fork"], [], 2.0),
                "foreign_exec": (CONTROL_SOURCES["foreign_exec"], [], 2.0),
                "recursive_exec": (CONTROL_SOURCES["recursive_exec"], [], RECURSIVE_TIMEOUT),
            }
            probes = {}
            for name, (source, argv, timeout) in probe_inputs.items():
                result = fx17._run_subject(source, argv, timeout=timeout)
                probes[name] = _probe_record(source, argv, timeout, result)

            candidate_connections = []
            server.settimeout(0.05)
            end = time.monotonic() + 0.25
            while time.monotonic() < end:
                try:
                    connection, _ = server.accept()
                except socket.timeout:
                    continue
                with connection:
                    connection.settimeout(0.05)
                    try:
                        received = connection.recv(4096)
                    except (OSError, socket.timeout):
                        received = b""
                    candidate_connections.append({
                        "received_bytes": len(received),
                        "received_sha256": fx17.digest(received)})
        finally:
            server.close()
        read_after = fx17.digest(read_path.read_bytes())
        write_after = fx17.digest(write_path.read_bytes())
        foreign_raw = foreign.read_bytes()
        canaries = {
            "read_nonce": read_nonce,
            "read_file": {"path": str(read_path), "before_sha256": read_before,
                          "after_sha256": read_after,
                          "harness_readable": read_path.read_bytes() == read_content,
                          "harness_readback_sha256": fx17.digest(read_path.read_bytes())},
            "write_file": {"path": str(write_path), "before_sha256": write_before,
                           "after_sha256": write_after, "exists_after": write_path.exists(),
                           "bytes_after": len(write_path.read_bytes())},
            "network_nonce": network_nonce,
            "network": {"address": "127.0.0.1", "port": port,
                        "listener_ready": True,
                        "positive_control_received": network_ready,
                        "positive_control_expected_bytes": len(positive),
                        "positive_control_expected_sha256": fx17.digest(positive),
                        "positive_control_received_bytes": len(positive_received),
                        "positive_control_received_sha256": fx17.digest(positive_received),
                        "candidate_connections": candidate_connections},
            "foreign_exec_target": {"path": str(foreign), "exists": foreign.is_file(),
                                    "executable": os.access(foreign, os.X_OK),
                                    "sha256": fx17.digest(foreign_raw),
                                    "bytes": len(foreign_raw)},
        }
        capture_probes = _run_unpaid_capture_probes(root)
        cli_failure_negative_control = _run_failed_cli_negative_control(root)
        approval_consumption_negative_control = (
            _run_consumption_registry_negative_control(root))

    report = {
        "schema": "aethyme.fx17.independent-isolation-controls/v3",
        "status": "pending",
        "created_at": fx17.now(),
        "platform": sys.platform,
        "runner_sha256": current_runner,
        "control_harness_sha256": current_harness,
        "subject_profile_template_sha256": current_profile,
        "independent_review": {"reviewer": reviewer.strip(),
                               "reviewed_at": reviewed_at,
                               "review_acknowledged": True,
                               "runner_sha256": reviewed_runner,
                               "control_harness_sha256": reviewed_harness,
                               "subject_profile_template_sha256": reviewed_profile},
        "canaries": canaries,
        "probes": probes,
        "capture_probes": capture_probes,
        "cli_failure_negative_control": cli_failure_negative_control,
        "approval_consumption_negative_control": approval_consumption_negative_control,
    }
    failures = controls_findings(report)
    report["status"] = "pass" if not failures else "fail"
    if failures:
        report["failures"] = failures
    _write_control_report(output, report)
    return {"output": str(output), "status": report["status"],
            "runner_sha256": current_runner,
            "control_harness_sha256": current_harness,
            "subject_profile_template_sha256": current_profile,
            "evidence_sha256": report["evidence_sha256"],
            "failures": failures}
