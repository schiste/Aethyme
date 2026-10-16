from datetime import datetime, timezone
from hashlib import sha256
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path.cwd()
PACKAGE = ROOT / "packages/aethyme-eval/local-v3/d10-identity-parity"
QUAL = PACKAGE / "qualification"
ATTEMPT = QUAL / "attempt-004-clean-75ee203"
NODE = Path("/opt/homebrew/bin/node")
SCRIPT_REL = "packages/aethyme-eval/local-v3/d10-identity-parity/d10-identity-parity.mts"
PINS_REL = "packages/aethyme-eval/local-v3/d10-identity-parity/source-pins.json"
EXPECTED_HEAD = "75ee20385b85529ea427e4fcd784727e8776c973"
EXPECTED_TREE = "cefb20692e37ec837ea45de53a02d428699227ed"
DEADLINE_SECONDS = 60
MAX_OLD_SPACE_MIB = 256
ARGV = [str(NODE), f"--max-old-space-size={MAX_OLD_SPACE_MIB}", "--experimental-strip-types", SCRIPT_REL]
ENV = {"PATH": "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}


def run_git(*args):
    return subprocess.run(["/usr/bin/git", *args], cwd=ROOT, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout.decode("utf-8").strip()


def now():
    return datetime.now(timezone.utc).isoformat(timespec="microseconds")


def digest(data):
    return sha256(data).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")

head = run_git("rev-parse", "HEAD")
tree = run_git("rev-parse", "HEAD^{tree}")
status = run_git("status", "--porcelain")
if head != EXPECTED_HEAD or tree != EXPECTED_TREE or status:
    raise SystemExit(f"refusing attempt: expected clean {EXPECTED_HEAD}/{EXPECTED_TREE}, got {head}/{tree}, status={status!r}")
if ATTEMPT.exists():
    raise SystemExit(f"refusing to overwrite existing attempt directory: {ATTEMPT}")
ATTEMPT.mkdir(parents=False, exist_ok=False)

launcher_bytes = Path(__file__).read_bytes()
(ATTEMPT / "launcher.py").write_bytes(launcher_bytes)
started = now()
pins_bytes = (ROOT / PINS_REL).read_bytes()
pins = json.loads(pins_bytes)
source_hashes = []
for group_name, kind in (("rust_sources", "rust_sources"), ("typescript_sources", "typescript_sources"), ("fixture_files", "fixture_files")):
    for item in pins[group_name]:
        path = item["path"]
        actual = digest((ROOT / path).read_bytes())
        source_hashes.append({"kind": kind, "path": path, "sha256": actual, "expected_sha256": item["sha256"], "matches_pin": actual == item["sha256"]})

node_version = subprocess.run([str(NODE), "--version"], cwd=ROOT, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10, check=False)
node_exec_path = subprocess.run([str(NODE), "-p", "process.execPath"], cwd=ROOT, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10, check=False)
node_sha = digest(NODE.read_bytes())

run_started = now()
timed_out = False
try:
    result = subprocess.run(ARGV, cwd=ROOT, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=DEADLINE_SECONDS, check=False)
    runner_exit = result.returncode
    stdout_bytes = result.stdout
    stderr_bytes = result.stderr
except subprocess.TimeoutExpired as exc:
    timed_out = True
    runner_exit = None
    stdout_bytes = exc.stdout or b""
    stderr_bytes = exc.stderr or b""
run_completed = now()

(ATTEMPT / "stdout").write_bytes(stdout_bytes)
(ATTEMPT / "stderr").write_bytes(stderr_bytes)

runner_record = {
    "runner": "d10-identity-parity",
    "script": SCRIPT_REL,
    "argv": ARGV,
    "started_at_utc": run_started,
    "completed_at_utc": run_completed,
    "elapsed_ms": int((datetime.fromisoformat(run_completed) - datetime.fromisoformat(run_started)).total_seconds() * 1000),
    "deadline_seconds": DEADLINE_SECONDS,
    "max_old_space_size_mib": MAX_OLD_SPACE_MIB,
    "timed_out": timed_out,
    "exit_code": runner_exit,
    "stdout_bytes": len(stdout_bytes),
    "stderr_bytes": len(stderr_bytes),
    "stdout_sha256": digest(stdout_bytes),
    "stderr_sha256": digest(stderr_bytes),
}
write_json(ATTEMPT / "run.json", runner_record)

snapshot = {
    "schema": "aethyme.d10-offline-typescript-qualification-attempt/v1",
    "attempt": ATTEMPT.name,
    "broker_session": 1135,
    "started_at_utc": started,
    "completed_at_utc": run_completed,
    "source_commit": head,
    "source_tree": tree,
    "source_git_status_porcelain_before_run": status,
    "qualification_source_commit": pins["qualification_source_commit"],
    "fixture_source_commit": pins["fixture_source_commit"],
    "source_pins_path": PINS_REL,
    "source_pins_sha256": digest(pins_bytes),
    "source_pin_count": len(source_hashes),
    "source_hashes": source_hashes,
    "launcher_script_sha256": digest(launcher_bytes),
    "node_path": str(NODE),
    "node_reported_exec_path": node_exec_path.stdout.decode("utf-8").strip(),
    "node_version": node_version.stdout.decode("utf-8").strip(),
    "node_version_exit_code": node_version.returncode,
    "node_binary_sha256": node_sha,
    "argv_flags": ARGV[1:3],
    "deadline_seconds_per_runner": DEADLINE_SECONDS,
    "max_old_space_size_mib": MAX_OLD_SPACE_MIB,
    "network": "offline: standard-library-only runner, clean PATH, no install/provider/network operation",
    "cargo_native_or_product_runtime": False,
    "runner": runner_record,
    "qualification_result": "PASS" if runner_exit == 0 and not timed_out else "FAIL",
}
write_json(ATTEMPT / "source-snapshot.json", snapshot)

command_log = {
    "launcher": str(Path(__file__).resolve()),
    "launcher_sha256": digest(launcher_bytes),
    "cwd": str(ROOT),
    "argv": ARGV,
    "environment": ENV,
    "deadline_seconds": DEADLINE_SECONDS,
    "max_old_space_size_mib": MAX_OLD_SPACE_MIB,
    "started_at_utc": run_started,
    "completed_at_utc": run_completed,
    "runner_exit_code": runner_exit,
    "timed_out": timed_out,
    "stdout_path": str((ATTEMPT / "stdout").relative_to(ROOT)),
    "stderr_path": str((ATTEMPT / "stderr").relative_to(ROOT)),
}
write_json(ATTEMPT / "command.log", command_log)
print(json.dumps({"attempt": ATTEMPT.name, "runner_exit_code": runner_exit, "timed_out": timed_out, "assertion_report_stdout_bytes": len(stdout_bytes), "stderr_bytes": len(stderr_bytes), "pins_match": all(item["matches_pin"] for item in source_hashes), "source_commit": head, "source_tree": tree}, sort_keys=True))
