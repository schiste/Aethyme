#!/usr/bin/env python3
"""Offline monitor controls: synthetic children only, no socket/provider/model."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "pr_monitor", Path(__file__).with_name("aethyme-pr-monitor.py"))
monitor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(monitor)


class Clock:
    now = 0.0

    def __call__(self):
        return self.now


class MonitorControls(unittest.TestCase):
    def invoke(self, listing="[]", tick_code=0, listing_code=0):
        calls = []

        def run(command, repo, **kwargs):
            calls.append((command, repo, kwargs))
            code, output = (tick_code, "tick") if "tick" in command else (listing_code, listing)
            return subprocess.CompletedProcess(command, code, output, "")

        clock = Clock()
        budget = monitor.adapter.Deadline(90, clock)
        with tempfile.TemporaryDirectory() as repo, \
             patch.object(monitor.adapter, "run_broker", side_effect=run), \
             patch.object(monitor.adapter, "main", return_value=0) as deliver, \
             contextlib.redirect_stdout(io.StringIO()) as output:
            monitor.run_pass(repo, "fake-broker", "worker-1", 20, budget)
        return calls, deliver, output.getvalue(), budget

    def test_verified_empty_outbox_starts_no_transport(self):
        calls, deliver, output, _ = self.invoke(" \n [] \n")
        self.assertEqual(len(calls), 2)
        self.assertIn("tick", calls[0][0])
        self.assertIn("list", calls[1][0])
        deliver.assert_not_called()
        self.assertIn("adapter not started", output)

    def test_pending_delivery_shares_finite_pass_and_adapter_budgets(self):
        calls, deliver, _, budget = self.invoke('[{"id":1}]')
        self.assertTrue(all(call[2]["budget"] is budget for call in calls))
        self.assertEqual(calls[0][2]["call_seconds"], 35)
        delivery_budget = deliver.call_args.kwargs["budget"]
        self.assertEqual(delivery_budget.expires, 45)
        self.assertIs(delivery_budget.clock, budget.clock)
        self.assertIn("worker-1", deliver.call_args.args[0])

    def test_failed_tick_does_not_suppress_previously_pending_delivery(self):
        calls, deliver, output, _ = self.invoke('[{"id":1}]', tick_code=1)
        self.assertEqual(len(calls), 2)
        deliver.assert_called_once()
        self.assertIn("tick failed", output)

    def test_failed_or_malformed_listing_cannot_hide_pending_delivery(self):
        for listing, code in [("[]", 1), ("{}", 0), ("not json", 0)]:
            with self.subTest(listing=listing, code=code):
                _, deliver, output, _ = self.invoke(listing, listing_code=code)
                deliver.assert_called_once()
                self.assertIn("delivery listing failed", output)

    def test_expired_pass_cannot_spawn_or_open_transport(self):
        clock = Clock()
        budget = monitor.adapter.Deadline(1, clock)
        clock.now = 2
        with tempfile.TemporaryDirectory() as repo, \
             patch.object(monitor.adapter.subprocess, "Popen") as spawn, \
             patch.object(monitor.adapter, "main") as deliver, \
             contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "deadline exhausted"):
                monitor.run_pass(repo, "must-not-start", "worker", 20, budget)
        spawn.assert_not_called()
        deliver.assert_not_called()

    def test_adapter_gets_only_time_remaining_after_tick_and_listing(self):
        clock = Clock()
        budget = monitor.adapter.Deadline(90, clock)

        def run(command, repo, **kwargs):
            clock.now += 40
            return subprocess.CompletedProcess(command, 0, '[{"id":1}]', "")

        with tempfile.TemporaryDirectory() as repo, \
             patch.object(monitor.adapter, "run_broker", side_effect=run), \
             patch.object(monitor.adapter, "main", return_value=0) as deliver, \
             contextlib.redirect_stdout(io.StringIO()):
            monitor.run_pass(repo, "fake", "worker", 20, budget)
        self.assertEqual(deliver.call_args.kwargs["budget"].remaining(), 10)

    def test_invalid_limit_is_refused_before_broker_or_transport(self):
        for limit in [0, -1, 101]:
            with tempfile.TemporaryDirectory() as repo, \
                 patch.object(monitor.adapter, "run_broker") as broker, \
                 patch.object(monitor.adapter, "main") as deliver:
                with self.assertRaisesRegex(ValueError, "limit"):
                    monitor.run_pass(repo, "fake", "worker", limit)
                broker.assert_not_called()
                deliver.assert_not_called()

    def test_combined_pass_log_is_capped_with_visible_truncation(self):
        def flood(*args):
            print("x" * (monitor.LOG_CHARACTERS + 100))
            print("y" * 100, file=monitor.sys.stderr)
        with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": "synthetic"}), \
             patch.object(monitor, "run_pass", side_effect=flood), \
             contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(monitor.main(), 0)
        self.assertTrue(output.getvalue().startswith("x" * monitor.LOG_CHARACTERS))
        self.assertIn("log truncated", output.getvalue())
        self.assertLess(len(output.getvalue()), monitor.LOG_CHARACTERS + 100)

    def test_empty_optional_environment_keeps_shell_default_semantics(self):
        with patch.dict(os.environ, {"AETHYME_PR_MONITOR_REPO": "synthetic",
                                     "AETHYME_BROKER_BIN": "",
                                     "AETHYME_PR_MONITOR_WORKER": "",
                                     "AETHYME_PR_MONITOR_LIMIT": ""}), \
             patch.object(monitor.socket, "gethostname", return_value="fixture.example"), \
             patch.object(monitor, "run_pass") as run:
            self.assertEqual(monitor.main(), 0)
        run.assert_called_once_with("synthetic", "aethyme", "launchd-fixture", 20)

    def test_real_shell_entrypoint_reaches_only_synthetic_empty_broker(self):
        with tempfile.TemporaryDirectory(prefix="monitor fixture '") as directory:
            repo = Path(directory)
            calls = repo / "calls"
            broker = repo / "fake broker"
            broker.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$TEST_CALLS"\ncase "$*" in\n*"deliveries list"*) printf "[]\\n" ;;\n*) printf "tick\\n" ;;\nesac\n')
            broker.chmod(0o755)
            result = subprocess.run(["/bin/bash", str(Path(__file__).with_name("aethyme-pr-monitor.sh"))],
                                    env={**os.environ, "AETHYME_PR_MONITOR_REPO": str(repo),
                                         "AETHYME_BROKER_BIN": str(broker), "TEST_CALLS": str(calls),
                                         "PYTHONDONTWRITEBYTECODE": "1"},
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("adapter not started", result.stdout)
            commands = calls.read_text().splitlines()
            self.assertEqual(len(commands), 2)
            self.assertIn("watch pr tick --limit 20", commands[0])
            self.assertIn("deliveries list --adapter chau7 --json", commands[1])


if __name__ == "__main__":
    unittest.main(verbosity=2)
