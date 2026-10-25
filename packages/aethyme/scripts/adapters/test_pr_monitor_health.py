#!/usr/bin/env python3
"""Private temporary health files only; no provider, socket, model or live repo."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "pr_monitor_health", Path(__file__).with_name("aethyme-pr-monitor.py"))
monitor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(monitor)


class HealthControls(unittest.TestCase):
    def record(self, **changes):
        return {"schema": "aethyme.pr-monitor-health/v1", "finished_unix_ms": 1000,
                "outcome": "completed", "tick": "completed", "outbox": "empty",
                "delivery": "not_started", "failure_kind": None, **changes}

    def test_repeated_passes_replace_one_bounded_private_record(self):
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(monitor, "__file__", str(Path(directory) / "monitor.py")):
            for index in range(100):
                monitor.write_health(self.record(finished_unix_ms=index, secret="must not persist"))
            path = Path(directory) / "health.json"
            data = json.loads(path.read_text())
            self.assertEqual(data["finished_unix_ms"], 99)
            self.assertNotIn("secret", data)
            self.assertLess(path.stat().st_size, monitor.HEALTH_BYTES)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual([p.name for p in Path(directory).iterdir()], ["health.json"])
            original = path.read_bytes()
            with self.assertRaisesRegex(ValueError, "record budget"):
                monitor.write_health(self.record(failure_kind="x" * 4097))
            self.assertEqual(path.read_bytes(), original)

    def test_symlinks_hardlinks_directories_and_oversized_records_are_refused(self):
        for kind in ["symlink", "hardlink", "directory", "oversized", "public"]:
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory, \
                 patch.object(monitor, "__file__", str(Path(directory) / "monitor.py")):
                root = Path(directory); target = root / "target"
                target.write_bytes(b"unchanged"); target.chmod(0o600)
                health = root / "health.json"
                if kind == "symlink": health.symlink_to(target)
                elif kind == "hardlink": os.link(target, health)
                elif kind == "directory": health.mkdir()
                else:
                    health.write_bytes(b"x" * (4097 if kind == "oversized" else 1))
                    health.chmod(0o644 if kind == "public" else 0o600)
                with self.assertRaisesRegex(ValueError, "bounded private"):
                    monitor.write_health(self.record())
                self.assertEqual(target.read_bytes(), b"unchanged")

    def test_shared_or_symlinked_helper_directory_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); real = root / "real"; real.mkdir(mode=0o700)
            link = root / "alias"; link.symlink_to(real)
            with patch.object(monitor, "__file__", str(link / "monitor.py")):
                with self.assertRaises(OSError): monitor.write_health(self.record())
            real.chmod(0o755)
            with patch.object(monitor, "__file__", str(real / "monitor.py")):
                with self.assertRaisesRegex(ValueError, "private user-owned"):
                    monitor.write_health(self.record())
            self.assertFalse((real / "health.json").exists())

    def test_failed_atomic_publication_preserves_previous_record_and_cleans_temp(self):
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(monitor, "__file__", str(Path(directory) / "monitor.py")):
            monitor.write_health(self.record()); path = Path(directory) / "health.json"
            original = path.read_bytes()
            with patch.object(monitor.os, "replace", side_effect=OSError("synthetic refusal")):
                with self.assertRaises(OSError): monitor.write_health(self.record(outcome="stopped"))
            self.assertEqual(path.read_bytes(), original)
            self.assertEqual([p.name for p in Path(directory).iterdir()], ["health.json"])

    def test_health_is_off_by_default_and_reports_only_failure_type(self):
        with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": "fixture",
                                     "AETHYME_PR_MONITOR_RECORD_HEALTH": ""}), \
             patch.object(monitor, "run_pass", side_effect=RuntimeError("secret details")), \
             patch.object(monitor, "write_health") as write, \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(monitor.main(), 0)
            write.assert_not_called()
            os.environ["AETHYME_PR_MONITOR_RECORD_HEALTH"] = "1"
            self.assertEqual(monitor.main(), 0)
        record = write.call_args.args[0]
        self.assertEqual(record["outcome"], "stopped")
        self.assertEqual(record["failure_kind"], "RuntimeError")
        self.assertNotIn("secret", json.dumps(record))
        self.assertGreater(record["finished_unix_ms"], 1000)

    def test_zero_adapter_exit_is_attempted_and_never_claims_sent(self):
        report = self.record()
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(monitor.adapter, "run_broker", return_value=
                          subprocess.CompletedProcess([], 0, '[{"id":1}]', '')), \
             patch.object(monitor.adapter, "main", return_value=0), \
             contextlib.redirect_stdout(io.StringIO()):
            monitor.run_pass(directory, "synthetic", "fixture", 20, report=report)
        self.assertEqual(report["delivery"], "attempted")
        self.assertNotIn("sent", json.dumps(report))
        self.assertEqual(report["outbox"], "pending")

    def test_failed_tick_is_visible_even_when_empty_outbox_completes(self):
        report = self.record()
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(monitor.adapter, "run_broker", side_effect=[
                 subprocess.CompletedProcess([], 1, '', ''),
                 subprocess.CompletedProcess([], 0, '[]', '')]), \
             patch.object(monitor.adapter, "main") as deliver, \
             contextlib.redirect_stdout(io.StringIO()):
            monitor.run_pass(directory, "synthetic", "fixture", 20, report=report)
        self.assertEqual(report["tick"], "failed")
        self.assertEqual(report["outbox"], "empty")
        self.assertEqual(report["delivery"], "not_started")
        deliver.assert_not_called()

    def test_health_write_failure_keeps_monitor_result(self):
        with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": "fixture",
                                     "AETHYME_PR_MONITOR_RECORD_HEALTH": "1"}), \
             patch.object(monitor, "run_pass"), \
             patch.object(monitor, "write_health", side_effect=OSError("private detail")), \
             contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(monitor.main(), 0)
        self.assertIn("health unavailable: OSError", output.getvalue())
        self.assertNotIn("private detail", output.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=2)
