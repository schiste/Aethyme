"""Unpaid local controls for the FX17 candidate isolation runner."""
from __future__ import annotations

import base64
import json
import math
import os
import secrets
import signal
import socket
import sys
import tempfile
import time
from pathlib import Path

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


def controls_findings(report):
    failures = []
    if (report.get("schema") != "aethyme.fx17.independent-isolation-controls/v2"
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

    report = {
        "schema": "aethyme.fx17.independent-isolation-controls/v2",
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
    }
    failures = controls_findings(report)
    report["status"] = "pass" if not failures else "fail"
    if failures:
        report["failures"] = failures
    report["evidence_sha256"] = fx17.digest(fx17.jbytes(report))
    fd = os.open(output, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(fx17.jbytes(report))
    return {"output": str(output), "status": report["status"],
            "runner_sha256": current_runner,
            "control_harness_sha256": current_harness,
            "subject_profile_template_sha256": current_profile,
            "evidence_sha256": report["evidence_sha256"],
            "failures": failures}
