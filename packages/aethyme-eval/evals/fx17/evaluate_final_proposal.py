#!/usr/bin/env python3
"""Compare a candidate app.sh with the externally supplied frozen FX17 oracle."""
from __future__ import annotations

import argparse
import json
import math
import os
import stat
import sys
from pathlib import Path

import run_fx17 as fx17


ORACLE_FILES = {
    "baseline": ("baseline-cases.json", 65, 8748,
                 "bfbec841810ac9269b34c628969a4c8207809b70716753d979750ecedb605a06"),
    "SX01": ("SX01-cases.json", 88, 11992,
             "8c4240d27f457e420e311021a917d31bfb6382a6461a1d863ad951e253c23144"),
    "SX02": ("SX02-cases.json", 101, 13850,
             "7b647b3fbf292c421b2f92e426dcdafec47efa245dbe96869837732583457818"),
}
RECOVERY_SHA256 = "d7ca7baad20fbe86a347bdad63b5a2fb20da09eba14d938941f277da3af41a61"
PROVENANCE_SHA256 = "6c6205982af3a7fc20d3e62134e4b30d9ad5ee7b3bb8f424a005a3296083829f"
DERIVATION_SHA256 = "bb92e2022938f0d1c92832338064d00b7be1f559f3dab8c77f6af65b8ad5a33c"
DERIVE_SOURCE_SHA256 = DERIVATION_SHA256
ORACLE_FIELDS = {"id", "arguments", "stdin_hex", "exit_code", "stdout", "stderr"}
MAX_ORACLE_BYTES = 1024 * 1024
MAX_CANDIDATE_BYTES = fx17.MAX_CANDIDATE_BYTES


def _regular_file(path):
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise fx17.Fx17Error("Expected a regular non-symlink file: " + str(path))
    return path


def _read_bounded(path, limit):
    path = _regular_file(path)
    info = path.stat()
    if info.st_size > limit:
        raise fx17.Fx17Error("Trusted input exceeds its byte limit: " + str(path.name))
    with path.open("rb") as stream:
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise fx17.Fx17Error("Trusted input exceeded its read bound: " + str(path.name))
    return raw


def load_oracle(oracle_dir, kind):
    filename, expected_count, expected_bytes, expected_hash = ORACLE_FILES[kind]
    root = Path(oracle_dir).resolve(strict=True)
    case_bytes = _read_bounded(root / filename, MAX_ORACLE_BYTES)
    recovery_bytes = _read_bounded(root / "recovery.json", MAX_ORACLE_BYTES)
    provenance_bytes = _read_bounded(root / "provenance.json", MAX_ORACLE_BYTES)
    derive_bytes = _read_bounded(root / "derive.rb", MAX_ORACLE_BYTES)
    if len(case_bytes) != expected_bytes or fx17.digest(case_bytes) != expected_hash:
        raise fx17.Fx17Error("Frozen oracle case hash does not match the original vector")
    if fx17.digest(recovery_bytes) != RECOVERY_SHA256:
        raise fx17.Fx17Error("Recovered-oracle record hash mismatch")
    if fx17.digest(provenance_bytes) != PROVENANCE_SHA256:
        raise fx17.Fx17Error("Independent oracle provenance hash mismatch")
    if fx17.digest(derive_bytes) != DERIVE_SOURCE_SHA256:
        raise fx17.Fx17Error("Original oracle derivation source hash mismatch")
    try:
        recovery = json.loads(recovery_bytes)
        provenance = json.loads(provenance_bytes)
        cases = json.loads(case_bytes)
    except json.JSONDecodeError as exc:
        raise fx17.Fx17Error("Frozen oracle bundle contains invalid JSON") from exc
    if (not isinstance(recovery, dict) or not isinstance(provenance, dict)
            or recovery.get("original_derivation_sha256") != DERIVATION_SHA256
            or provenance.get("schema") != "fx17.independent-contract-oracles/v1"
            or not isinstance(provenance.get("case_counts"), dict)
            or provenance["case_counts"].get(kind) != expected_count):
        raise fx17.Fx17Error("Frozen oracle provenance does not match the original derivation")
    recovery_files = recovery.get("files")
    if not isinstance(recovery_files, dict):
        raise fx17.Fx17Error("Recovered-oracle record has no file inventory")
    recovery_file = recovery_files.get(filename, {})
    if not isinstance(recovery_file, dict):
        raise fx17.Fx17Error("Recovered-oracle file record is invalid")
    if not isinstance(provenance.get("files"), list):
        raise fx17.Fx17Error("Independent oracle provenance has no file inventory")
    provenance_files = {row.get("file"): row for row in provenance.get("files", [])}
    if (recovery_file.get("sha256") != expected_hash
            or recovery_file.get("bytes") != len(case_bytes)
            or provenance_files.get(filename, {}).get("sha256") != expected_hash
            or provenance_files.get(filename, {}).get("bytes") != len(case_bytes)):
        raise fx17.Fx17Error("Frozen oracle is not listed consistently in both provenance records")
    if not isinstance(cases, list) or len(cases) != expected_count:
        raise fx17.Fx17Error("Frozen oracle case count does not match its original vector")
    seen_ids = set()
    for row in cases:
        if not isinstance(row, dict) or set(row) != ORACLE_FIELDS:
            raise fx17.Fx17Error("Frozen oracle row has an unexpected shape")
        case_id, arguments = row.get("id"), row.get("arguments")
        if (not isinstance(case_id, str) or not case_id
                or not isinstance(arguments, list) or len(arguments) > 64
                or any(not isinstance(arg, str) or "\0" in arg for arg in arguments)
                or sum(len(arg.encode("utf-8")) for arg in arguments) > 256 * 1024):
            raise fx17.Fx17Error("Frozen oracle row contains invalid literal argv")
        if case_id in seen_ids:
            raise fx17.Fx17Error("Frozen oracle repeats a case ID")
        seen_ids.add(case_id)
        if (row.get("stdin_hex") != "" or type(row.get("exit_code")) is not int
                or not 0 <= row["exit_code"] <= 255
                or not isinstance(row.get("stdout"), str)
                or not isinstance(row.get("stderr"), str)):
            raise fx17.Fx17Error("Frozen oracle row must specify empty stdin and exact text streams")
        if (len(row["stdout"].encode("utf-8")) > fx17.SUBJECT_STDOUT_MAX
                or len(row["stderr"].encode("utf-8")) > fx17.SUBJECT_STDERR_MAX):
            raise fx17.Fx17Error("Frozen oracle stream exceeds the subject capture limit")
    return cases, expected_hash, fx17.digest(provenance_bytes), fx17.digest(recovery_bytes)


def evaluate(candidate_path, oracle_dir, kind, controls_path, output, timeout=2.0):
    controls = fx17.require_controls(controls_path)
    cases, oracle_hash, provenance_hash, recovery_hash = load_oracle(oracle_dir, kind)
    candidate_path = _regular_file(candidate_path).resolve(strict=True)
    source_bytes = _read_bounded(candidate_path, MAX_CANDIDATE_BYTES)
    if len(source_bytes) > MAX_CANDIDATE_BYTES:
        raise fx17.Fx17Error("Candidate app.sh exceeds the runner byte limit")
    try:
        source = source_bytes.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise fx17.Fx17Error("Candidate app.sh is not UTF-8") from exc
    if "\0" in source:
        raise fx17.Fx17Error("Candidate app.sh contains NUL")
    if (isinstance(timeout, bool) or not isinstance(timeout, (int, float))
            or not math.isfinite(timeout) or timeout <= 0 or timeout > fx17.WALL):
        raise fx17.Fx17Error("Per-case subject timeout must be in (0, 180]")

    output = Path(output).expanduser()
    if output.exists() or output.is_symlink():
        raise fx17.Fx17Error("Refusing to overwrite evaluation results")
    output = output.resolve()
    repo_root = fx17.HERE.parents[3].resolve(strict=True)
    try:
        output.relative_to(repo_root)
    except ValueError:
        pass
    else:
        raise fx17.Fx17Error("Keep private evaluation results outside the repository")
    output.parent.mkdir(parents=True, exist_ok=True)

    candidate_hash = fx17.digest(source_bytes)
    control_evidence_hash = controls["evidence_sha256"]
    results = []
    for row in cases:
        expected_out = row["stdout"].encode("utf-8")
        expected_err = row["stderr"].encode("utf-8")
        observed = fx17.run_subject(source, row["arguments"], controls_path, timeout=timeout)
        actual_out, actual_err = observed["stdout"], observed["stderr"]
        out_match = actual_out == expected_out
        err_match = actual_err == expected_err
        exit_match = observed["exit_code"] == row["exit_code"]
        matched = (observed["outcome"] == "completed" and not observed["overflow_streams"]
                   and exit_match and out_match and err_match)
        results.append({
            "case_id": row["id"],
            "outcome": observed["outcome"],
            "timed_out": observed["timed_out"],
            "overflow_streams": observed["overflow_streams"],
            "process_group_killed": observed["process_group_killed"],
            "process_reaped": observed["process_reaped"],
            "wall_seconds": observed["wall_seconds"],
            "runner_sha256": observed["runner_sha256"],
            "profile_template_sha256": observed["profile_template_sha256"],
            "profile_sha256": observed["profile_sha256"],
            "control_evidence_sha256": control_evidence_hash,
            "exit_code": observed["exit_code"],
            "expected_exit_code": row["exit_code"],
            "exit_code_match": exit_match,
            "stdout_bytes": len(actual_out),
            "stdout_sha256": fx17.digest(actual_out),
            "expected_stdout_bytes": len(expected_out),
            "expected_stdout_sha256": fx17.digest(expected_out),
            "stdout_match": out_match,
            "stderr_bytes": len(actual_err),
            "stderr_sha256": fx17.digest(actual_err),
            "expected_stderr_bytes": len(expected_err),
            "expected_stderr_sha256": fx17.digest(expected_err),
            "stderr_match": err_match,
            "match": matched,
        })

    passed = sum(row["match"] for row in results)
    report = {
        "schema": "aethyme.fx17.final-proposal-evaluation/v1",
        "created_at": fx17.now(),
        "suite": kind,
        "case_count": len(results),
        "passed": passed,
        "failed": len(results) - passed,
        "candidate_sha256": candidate_hash,
        "oracle_sha256": oracle_hash,
        "oracle_provenance_sha256": provenance_hash,
        "oracle_recovery_sha256": recovery_hash,
        "control_evidence_sha256": control_evidence_hash,
        "runner_sha256": fx17.runner_hash(),
        "control_harness_sha256": controls["control_harness_sha256"],
        "subject_profile_template_sha256": fx17.profile_hash(),
        "case_timeout_seconds": timeout,
        "stdout_capture_limit_bytes_per_case": fx17.SUBJECT_STDOUT_MAX,
        "stderr_capture_limit_bytes_per_case": fx17.SUBJECT_STDERR_MAX,
        "cases": results,
    }
    report["report_sha256"] = fx17.digest(fx17.jbytes(report))
    fd = os.open(output, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(fx17.jbytes(report))
    return {"output": str(output), "suite": kind, "passed": passed,
            "failed": len(results) - passed, "candidate_sha256": candidate_hash,
            "oracle_sha256": oracle_hash, "report_sha256": report["report_sha256"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--oracle-dir", type=Path, required=True)
    parser.add_argument("--oracle-kind", choices=sorted(ORACLE_FILES), required=True)
    parser.add_argument("--controls", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--case-timeout", type=float, default=2.0)
    args = parser.parse_args()
    try:
        result = evaluate(args.candidate, args.oracle_dir, args.oracle_kind,
                          args.controls, args.output, args.case_timeout)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (fx17.Fx17Error, OSError, ValueError, json.JSONDecodeError) as exc:
        print(json.dumps({"error": str(exc)}, sort_keys=True), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
