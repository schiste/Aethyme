#!/usr/bin/env python3
"""Synthetic executable bytes only. No provider, socket, model or service."""
import contextlib
import hashlib
import importlib.util
import io
import os
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "pr_monitor_identity", Path(__file__).with_name("aethyme-pr-monitor.py"))
monitor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(monitor)
GENERATION = "a" * 64


class IdentityControls(unittest.TestCase):
    def setUp(self):
        self.environment = patch.dict(os.environ, {
            "AETHYME_PR_MONITOR_BROKER_SHA256": "",
            "AETHYME_PR_MONITOR_GENERATION": "",
            "AETHYME_PR_MONITOR_RECORD_HEALTH": "",
        })
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def executable(self, root, data=b"synthetic executable bytes"):
        path = Path(root) / "pinned broker"
        path.write_bytes(data); path.chmod(0o700)
        return path, hashlib.sha256(data).hexdigest()

    def budget(self):
        return monitor.adapter.Deadline(90)

    def test_matching_regular_executable_is_verified_without_running_it(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            with patch.object(monitor.adapter.subprocess, "Popen") as spawn:
                monitor.verify_broker_identity(str(path), expected, self.budget())
            spawn.assert_not_called()

    def test_changed_broker_stops_provider_and_records_failed_bound_generation(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            path.write_bytes(b"changed executable bytes")
            with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": directory,
                                         "AETHYME_BROKER_BIN": str(path),
                                         "AETHYME_PR_MONITOR_BROKER_SHA256": expected,
                                         "AETHYME_PR_MONITOR_GENERATION": GENERATION,
                                         "AETHYME_PR_MONITOR_RECORD_HEALTH": "1"}), \
                 patch.object(monitor, "run_pass") as run, \
                 patch.object(monitor, "write_health") as health, \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(monitor.main(), 0)
            run.assert_not_called()
            record = health.call_args.args[0]
            self.assertEqual(record["generation"], GENERATION)
            self.assertEqual(record["identity"], "failed")
            self.assertEqual(record["outcome"], "stopped")
            self.assertEqual(record["failure_kind"], "ValueError")

    def test_partial_or_invalid_generation_configuration_never_dispatches(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            for checksum, generation in [(expected, ""), ("", GENERATION),
                                          (expected, "private bad value"), ("bad", GENERATION)]:
                with self.subTest(checksum=checksum, generation=generation), \
                     patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": directory,
                                             "AETHYME_BROKER_BIN": str(path),
                                             "AETHYME_PR_MONITOR_BROKER_SHA256": checksum,
                                             "AETHYME_PR_MONITOR_GENERATION": generation}), \
                     patch.object(monitor, "run_pass") as run, \
                     contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(monitor.main(), 0)
                run.assert_not_called()

    def test_bound_success_shares_hash_and_provider_pass_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            seen = []
            original = monitor.verify_broker_identity
            def verify(broker, checksum, budget):
                seen.append(budget)
                original(broker, checksum, budget)
            with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": directory,
                                         "AETHYME_BROKER_BIN": str(path),
                                         "AETHYME_PR_MONITOR_BROKER_SHA256": expected,
                                         "AETHYME_PR_MONITOR_GENERATION": GENERATION,
                                         "AETHYME_PR_MONITOR_RECORD_HEALTH": "1"}), \
                 patch.object(monitor, "verify_broker_identity", side_effect=verify), \
                 patch.object(monitor, "run_pass") as run, \
                 patch.object(monitor, "write_health") as health, \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(monitor.main(), 0)
            self.assertIs(run.call_args.kwargs["budget"], seen[0])
            record = health.call_args.args[0]
            self.assertEqual(record["identity"], "verified")
            self.assertEqual(record["generation"], GENERATION)
            self.assertEqual(record["delivery"], "not_started")

    def test_symlinks_and_fifos_are_refused_before_reading_or_dispatch(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            link = Path(directory) / "alias"; link.symlink_to(path)
            with self.assertRaises(OSError):
                monitor.verify_broker_identity(str(link), expected, self.budget())
            fifo = Path(directory) / "fifo"; os.mkfifo(fifo, 0o700)
            with self.assertRaisesRegex(ValueError, "regular executable"):
                monitor.verify_broker_identity(str(fifo), expected, self.budget())

    def test_named_path_replacement_during_hashing_cannot_borrow_old_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            replacement = Path(directory) / "new executable"
            replacement.write_bytes(b"new identity"); replacement.chmod(0o700)
            class ReplacingBudget:
                calls = 0
                def remaining(self):
                    self.calls += 1
                    if self.calls == 2: os.replace(replacement, path)
                    return 90
            with self.assertRaisesRegex(ValueError, "identity changed"):
                monitor.verify_broker_identity(str(path), expected, ReplacingBudget())

    def test_expired_identity_budget_refuses_before_open(self):
        class ExpiredBudget:
            def remaining(self): raise RuntimeError("deadline exhausted")
        with patch.object(monitor.os, "open", side_effect=AssertionError("must not open")) as opened:
            with self.assertRaisesRegex(RuntimeError, "deadline exhausted"):
                monitor.verify_broker_identity("/synthetic", "a" * 64, ExpiredBudget())
        opened.assert_not_called()

    def test_oversized_or_nonexecutable_files_and_relative_paths_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            path, expected = self.executable(directory)
            with patch.object(monitor, "MAX_BINARY_BYTES", 8):
                with self.assertRaisesRegex(ValueError, "bounded regular executable"):
                    monitor.verify_broker_identity(str(path), expected, self.budget())
            path.chmod(0o600)
            with self.assertRaisesRegex(ValueError, "regular executable"):
                monitor.verify_broker_identity(str(path), expected, self.budget())
            with self.assertRaisesRegex(ValueError, "absolute path"):
                monitor.verify_broker_identity("relative", expected, self.budget())

    def test_real_isolated_helper_pair_writes_bound_health_without_bytecode(self):
        with tempfile.TemporaryDirectory(prefix="identity fixture '") as directory:
            root = Path(directory)
            for name in ["aethyme-pr-monitor.py", "chau7-delivery-adapter.py"]:
                (root / name).write_bytes(Path(__file__).with_name(name).read_bytes())
            program = b'#!/bin/sh\ncase "$*" in\n*"deliveries list"*) printf "[]\\n" ;;\n*) printf "tick\\n" ;;\nesac\n'
            broker, expected = self.executable(directory, program)
            result = subprocess.run([sys.executable, "-I", "-B", str(root / "aethyme-pr-monitor.py")],
                env={"PATH": "/usr/bin:/bin", "AETHYME_PR_MONITOR_REPO": directory,
                     "AETHYME_BROKER_BIN": str(broker),
                     "AETHYME_PR_MONITOR_BROKER_SHA256": expected,
                     "AETHYME_PR_MONITOR_GENERATION": GENERATION,
                     "AETHYME_PR_MONITOR_RECORD_HEALTH": "1"},
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            record = json.loads((root / "health.json").read_text())
            self.assertEqual(record["generation"], GENERATION)
            self.assertEqual(record["identity"], "verified")
            self.assertEqual(record["outbox"], "empty")
            self.assertEqual(record["delivery"], "not_started")
            self.assertFalse((root / "__pycache__").exists())

    def test_empty_identity_environment_preserves_default_manual_invocation(self):
        with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": "synthetic",
                                     "AETHYME_BROKER_BIN": "router", "AETHYME_PR_MONITOR_WORKER": "worker"}), \
             patch.object(monitor, "run_pass") as run, \
             patch.object(monitor, "verify_broker_identity") as verify:
            self.assertEqual(monitor.main(), 0)
        verify.assert_not_called()
        run.assert_called_once_with("synthetic", "router", "worker", 20)


if __name__ == "__main__":
    unittest.main(verbosity=2)
