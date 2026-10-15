#!/usr/bin/env python3
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid

GIT = "/opt/homebrew/bin/git"
DEFAULT_TMP_BASE = Path("/Volumes/T7-repositories/Application Support/Aethyme/qualification/local-check-temporaries")
BASE_ENVIRONMENT_KEYS = {
    "PATH",
    "TMPDIR",
    "RUSTC_WRAPPER",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_JOBS",
    "CI",
}
PROFILE_ENVIRONMENT_KEYS = {
    "AETHYME_UPDATE_SNAPSHOTS",
    "RUST_BACKTRACE",
    "RUSTFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_DEBUG",
    "CARGO_PROFILE_TEST_DEBUG",
    "AETHYME_NATIVE_SOURCE_PRESSURE_PROFILE",
    "AETHYME_DOCKER_CLI",
    "AETHYME_DOCKER_TEST_IMAGE",
    "DOCKER_HOST",
}

class StopRequested(Exception):
    def __init__(self, signum):
        self.signum = signum

def utc_now():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")

def run_git(root, *args):
    result = subprocess.run([GIT, "-C", str(root), *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode != 0:
        raise RuntimeError("git command failed: {}: {}".format(args, result.stderr.decode("utf-8", "replace")))
    return result.stdout

def source_snapshot(root):
    head = run_git(root, "rev-parse", "HEAD").decode().strip()
    tree = run_git(root, "rev-parse", "HEAD^{tree}").decode().strip()
    status = run_git(root, "status", "--porcelain=v1", "--untracked-files=all").decode("utf-8", "replace")
    diff = run_git(root, "diff", "--binary", "--no-ext-diff", "HEAD")
    untracked = run_git(root, "ls-files", "--others", "--exclude-standard", "-z")
    untracked_hash = hashlib.sha256()
    for raw_name in untracked.split(b"\0"):
        if not raw_name:
            continue
        path_bytes = raw_name
        file_path = root / os.fsdecode(raw_name)
        contents = file_path.read_bytes()
        untracked_hash.update(len(path_bytes).to_bytes(8, "big"))
        untracked_hash.update(path_bytes)
        untracked_hash.update(len(contents).to_bytes(8, "big"))
        untracked_hash.update(contents)
    return {
        "head": head,
        "tree": tree,
        "status": status,
        "diff_sha256": hashlib.sha256(diff).hexdigest(),
        "untracked_content_sha256": untracked_hash.hexdigest(),
    }

def safe_environment(env, explicit_profile_keys):
    allowed = BASE_ENVIRONMENT_KEYS | set(explicit_profile_keys)
    return {
        "values": {key: env[key] for key in sorted(allowed) if key in env},
        "explicit_profile_keys": sorted(explicit_profile_keys),
    }

def execution_context(env, tmp_base):
    tool_versions = {}
    for tool in ("rustc", "cargo", "aethyme", "git"):
        executable = shutil.which(tool, path=env.get("PATH"))
        if executable is None:
            tool_versions[tool] = {"executable": None, "version": None}
            continue
        result = subprocess.run(
            [executable, "--version"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            check=False,
        )
        version = (result.stdout or result.stderr).decode("utf-8", "replace").strip()
        tool_versions[tool] = {
            "executable": executable,
            "version": version,
            "exit_code": result.returncode,
        }
    disk = shutil.disk_usage(tmp_base)
    return {
        "host": {
            "hostname": socket.gethostname(),
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": sys.version.splitlines()[0],
        },
        "tool_versions": tool_versions,
        "tmp_volume": {
            "path": str(tmp_base),
            "free_bytes_before": disk.free,
            "total_bytes": disk.total,
        },
    }

def atomic_write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    with temporary.open("wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)
    dir_fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)

def atomic_json(path, value):
    atomic_write(path, (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8"))

def main():
    argv = sys.argv[1:]
    if "--" not in argv:
        raise SystemExit("usage: record-native-retention-check.py --root REPO --label NAME [--cwd DIR] [--output-root DIR] [--tmp-base DIR] [--env KEY=VALUE]... -- COMMAND [ARG ...]")
    split_at = argv.index("--")
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--cwd", default=".")
    parser.add_argument("--output-root")
    parser.add_argument("--tmp-base", default=str(DEFAULT_TMP_BASE))
    parser.add_argument("--env", action="append", default=[])
    args = parser.parse_args(argv[:split_at])
    command = argv[split_at + 1:]
    if not command:
        raise SystemExit("a command is required after --")

    root = Path(args.root).expanduser().resolve()
    if not root.is_dir():
        raise SystemExit("repository root does not exist: {}".format(root))
    top = run_git(root, "rev-parse", "--show-toplevel").decode().strip()
    if Path(top).resolve() != root:
        raise SystemExit("root must be the worktree root: {}".format(top))
    cwd = Path(args.cwd)
    if not cwd.is_absolute():
        cwd = root / cwd
    cwd = cwd.resolve()
    try:
        cwd.relative_to(root)
    except ValueError:
        raise SystemExit("working directory must remain inside the assigned worktree")
    if not cwd.is_dir():
        raise SystemExit("working directory does not exist: {}".format(cwd))

    label = re.sub(r"[^A-Za-z0-9_.-]+", "-", args.label).strip("-.")
    if not label:
        raise SystemExit("label must contain a safe filename character")
    output_root = Path(args.output_root).expanduser() if args.output_root else root / "target/local-resume/retention-checks"
    if not output_root.is_absolute():
        output_root = root / output_root
    output_root = output_root.resolve()
    output_root.mkdir(parents=True, exist_ok=True)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = output_root / "{}-{}-{}".format(stamp, label, uuid.uuid4().hex[:8])
    run_dir.mkdir()

    tmp_base = Path(args.tmp_base).expanduser().resolve()
    tmp_base.mkdir(parents=True, exist_ok=True)
    tmp_dir = Path(tempfile.mkdtemp(prefix=label + "-tmp-", dir=str(tmp_base))).resolve()
    outside = subprocess.run([GIT, "-C", str(tmp_dir), "rev-parse", "--show-toplevel"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if outside.returncode == 0:
        raise SystemExit("refusing TMPDIR inside a Git worktree: {}".format(outside.stdout.decode("utf-8", "replace").strip()))

    env = os.environ.copy()
    explicit_profile_keys = []
    for assignment in args.env:
        if "=" not in assignment:
            raise SystemExit("--env must be KEY=VALUE")
        key, value = assignment.split("=", 1)
        if key not in PROFILE_ENVIRONMENT_KEYS:
            raise SystemExit("--env key is not an approved non-secret profile key: {}".format(key))
        env[key] = value
        explicit_profile_keys.append(key)
    env["TMPDIR"] = str(tmp_dir)
    context = execution_context(env, tmp_base)

    log_path = run_dir / "command.log"
    status_path = run_dir / "command.status"
    metadata_path = run_dir / "command.json"
    before = source_snapshot(root)
    started = utc_now()
    metadata = {
        "schema": "aethyme.local-check/v1",
        "label": args.label,
        "state": "running",
        "started_at_utc": started,
        "root": str(root),
        "working_directory": str(cwd),
        "argv": command,
        "environment": safe_environment(env, explicit_profile_keys),
        "execution_context": context,
        "tmpdir": str(tmp_dir),
        "source_before": before,
        "log_path": str(log_path),
        "status_path": str(status_path),
        "numeric_exit": None,
    }
    atomic_json(metadata_path, metadata)
    print(json.dumps({"metadata": str(metadata_path), "log": str(log_path), "status": str(status_path), "state": "running"}, sort_keys=True), flush=True)

    stop_signal = None
    def request_stop(signum, frame):
        raise StopRequested(signum)
    old_handlers = {}
    for signum in (signal.SIGINT, signal.SIGTERM):
        old_handlers[signum] = signal.signal(signum, request_stop)

    child_rc = 127
    state = "completed"
    launch_error = None
    termination = None
    start_monotonic = time.monotonic()
    try:
        with log_path.open("wb") as log_file:
            try:
                child = subprocess.Popen(command, cwd=str(cwd), env=env, stdout=log_file, stderr=subprocess.STDOUT, start_new_session=True)
            except OSError as exc:
                launch_error = "{}: {}".format(type(exc).__name__, exc)
                log_file.write((launch_error + "\n").encode("utf-8", "replace"))
                child_rc = 127
                state = "launch_failed"
            else:
                try:
                    child_rc = child.wait()
                except StopRequested as stopped:
                    stop_signal = stopped.signum
                    termination = signal.Signals(stop_signal).name
                    try:
                        os.killpg(child.pid, stop_signal)
                    except ProcessLookupError:
                        pass
                    try:
                        child_rc = child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        try:
                            os.killpg(child.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        child_rc = child.wait()
                    state = "interrupted"
            log_file.flush()
            os.fsync(log_file.fileno())
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)

    ended = utc_now()
    after = source_snapshot(root)
    log_bytes = log_path.read_bytes()
    status_bytes = (str(child_rc) + "\n").encode("ascii")
    atomic_write(status_path, status_bytes)
    metadata.update({
        "state": state,
        "ended_at_utc": ended,
        "duration_seconds": round(time.monotonic() - start_monotonic, 6),
        "numeric_exit": child_rc,
        "launcher_error": launch_error,
        "termination_signal": termination,
        "source_after": after,
        "source_unchanged": before == after,
        "log_bytes": len(log_bytes),
        "log_sha256": hashlib.sha256(log_bytes).hexdigest(),
        "status_bytes": len(status_bytes),
        "status_sha256": hashlib.sha256(status_bytes).hexdigest(),
    })
    atomic_json(metadata_path, metadata)
    print(json.dumps({
        "metadata": str(metadata_path),
        "log": str(log_path),
        "status": str(status_path),
        "state": state,
        "numeric_exit": child_rc,
        "source_unchanged": before == after,
        "log_sha256": metadata["log_sha256"],
    }, sort_keys=True))
    if child_rc < 0:
        return 128 + abs(child_rc)
    return child_rc

if __name__ == "__main__":
    sys.exit(main())
