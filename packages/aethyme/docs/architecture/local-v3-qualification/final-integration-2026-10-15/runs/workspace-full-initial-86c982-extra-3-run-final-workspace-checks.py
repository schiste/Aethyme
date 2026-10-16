#!/usr/bin/env python3
"""Run the five approved whole-workspace checks sequentially through the frozen recorder."""
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

root = Path(__file__).resolve().parents[2]
env = {"HOME": str(Path.home()), "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8"}
for key in ("CARGO_HOME", "RUSTUP_HOME"):
    if key in os.environ:
        env[key] = os.environ[key]
env["PATH"] = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:" + str(Path.home() / ".cargo/bin")
env["RUSTC_WRAPPER"] = ""
env["CARGO_BUILD_JOBS"] = "4"
env["CARGO_TARGET_DIR"] = "/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/integrate-reviewed-local-bare-git-servic/target/local-resume/cargo"
git = "/opt/homebrew/bin/git"
recorder = root / "target/local-resume/record-final-local-check-v2.py"
assert hashlib.sha256(recorder.read_bytes()).hexdigest() == "29d2319c55cea24d6a1c76d7038b4bfd96cc105ae17e7ef4c00ef927d19286bc"
assert not subprocess.check_output([git, "status", "--porcelain=v1", "--untracked-files=all"], cwd=root, env=env), "source must be clean"
head = subprocess.check_output([git, "rev-parse", "HEAD"], cwd=root, env=env, text=True).strip()
stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
batch = root / "target/local-resume/final-workspace-batches" / (stamp + "-" + head[:12])
batch.mkdir(parents=True, exist_ok=False)
# The coordinator must supply the explicit handoff after all sibling Cargo/native checks finish.
handoff_path = root / "target/local-resume/final-workspace-runtime-handoff.json"
handoff_bytes = handoff_path.read_bytes()
handoff = json.loads(handoff_bytes)
assert handoff["cargo_native_slot_released"] is True
assert handoff["root_source_head"] == head
created = datetime.datetime.fromisoformat(handoff["created_at_utc"].replace("Z", "+00:00"))
assert created.tzinfo is not None
age = (datetime.datetime.now(datetime.timezone.utc) - created).total_seconds()
assert 0 <= age <= 600, "runtime handoff must be freshly recorded after release (within ten minutes)"
# Cooperating drivers use this lock; the explicit agent handoff covers older nonparticipating runners.
lock_file = (Path(env["CARGO_TARGET_DIR"]) / ".aethyme-final-local-validation.lock").open("a+")
fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
processes = subprocess.check_output(["/bin/ps", "-A", "-o", "pid=,comm="], env=env, text=True)
compiler_names = {"cargo", "rustc", "rustdoc", "clippy-driver", "cargo-clippy", "cargo-nextest"}
active = [line for line in processes.splitlines() if line.strip() and Path(line.strip().split(None, 1)[1]).name in compiler_names]
assert not active, "Cargo/compiler activity exists before exclusive runtime handoff: " + repr(active)
checks = [
    ("workspace-full", ".", ["packages/aethyme/scripts/test-like-ci.sh", "--full"]),
    ("workspace-fmt", "packages/aethyme/rust", ["cargo", "fmt", "--all", "--", "--check"]),
    ("workspace-clippy", "packages/aethyme/rust", ["cargo", "clippy", "--locked", "-j", "4", "--workspace", "--all-targets", "--", "-D", "warnings"]),
    ("workspace-examples", "packages/aethyme/rust", ["cargo", "test", "--locked", "-j", "4", "--workspace", "--examples"]),
    ("workspace-doc", "packages/aethyme/rust", ["cargo", "test", "--locked", "-j", "4", "--workspace", "--doc"]),
]
plan = {"source_head": head, "checks": [{"label": label, "cwd": cwd, "argv": argv} for label, cwd, argv in checks], "results": [], "declared_environment": env, "runtime_handoff": {"path": str(handoff_path), "sha256": hashlib.sha256(handoff_bytes).hexdigest(), "record": handoff}, "shared_target_lock": str(Path(env["CARGO_TARGET_DIR"]) / ".aethyme-final-local-validation.lock"), "preflight_active_compilers": active}
plan_path = batch / "batch.json"
plan_path.write_text(json.dumps(plan, indent=2) + "\n")
print(json.dumps({"batch": str(batch), "source_head": head}), flush=True)
# Invalidate only this worktree's crate mtimes: shared caches must rebuild from this source.
(root / "packages/aethyme/rust/crates/aethyme-broker/src/lib.rs").touch()
for label, cwd, argv in checks:
    assert subprocess.check_output([git, "rev-parse", "HEAD"], cwd=root, env=env, text=True).strip() == head, "source head changed"
    assert not subprocess.check_output([git, "status", "--porcelain=v1", "--untracked-files=all"], cwd=root, env=env), "source changed"
    command = ["/usr/bin/python3", str(recorder), "--root", str(root), "--label", label + "-" + head[:12], "--cwd", cwd, "--output-root", str(batch / "runs"), "--"] + argv
    junit = Path(env["CARGO_TARGET_DIR"]) / "nextest/ci/junit.xml"
    junit_before = junit.stat().st_mtime_ns if junit.is_file() else None
    result = subprocess.run(command, cwd=root, env=env, check=False)
    current_head = subprocess.check_output([git, "rev-parse", "HEAD"], cwd=root, env=env, text=True).strip()
    current_status = subprocess.check_output([git, "status", "--porcelain=v1", "--untracked-files=all"], cwd=root, env=env, text=True)
    candidates = list((batch / "runs").glob("*" + label + "-" + head[:12] + "*/command.json"))
    assert len(candidates) == 1, "require exactly one recorded command"
    metadata = json.loads(candidates[0].read_text())
    unchanged = metadata["source_unchanged"] and metadata["source_before"]["head"] == head and metadata["source_after"]["head"] == head and current_head == head and current_status == ""
    row = {"label": label, "recorder_returncode": result.returncode, "metadata": str(candidates[0]), "source_unchanged_and_clean": unchanged, "source_after": {"head": current_head, "status": current_status}}
    if label == "workspace-full":
        if junit.is_file() and junit.stat().st_mtime_ns != junit_before:
            target = batch / "workspace-full.junit.xml"
            shutil.copyfile(junit, target)
            contents = target.read_bytes()
            row["junit_copy"] = {"path": str(target), "bytes": len(contents), "sha256": hashlib.sha256(contents).hexdigest()}
        else:
            row["junit_copy"] = None
            row["junit_note"] = "No fresh JUnit file produced by this command; do not credit an earlier run"
    plan["results"].append(row)
    plan_path.write_text(json.dumps(plan, indent=2) + "\n")
    print(json.dumps(row), flush=True)
    if result.returncode:
        raise SystemExit(result.returncode)
    if not unchanged:
        raise SystemExit("source changed during check; raw run retained without final green credit")
print(json.dumps({"batch": str(batch), "all_five_checks_exited_zero": True}), flush=True)
