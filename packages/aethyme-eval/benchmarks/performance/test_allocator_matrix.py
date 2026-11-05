import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from build_allocator_matrix import require_admission
from allocator_matrix import Runner, environment, semantic, tree_hash


class AllocatorRunnerControls(unittest.TestCase):
    def test_success_accounts_completed_child(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runner = Runner(root)
            row, text = runner.run("success", [sys.executable, "-c", "print(42)"], root, environment(root))
            self.assertEqual(text.strip(), "42")
            self.assertEqual(row["exit_code"], 0)
            self.assertFalse(row["timeout"])
            self.assertGreater(row["peak_rss_bytes"], 0)
            self.assertGreater(row["wall_s"], 0)
            saved = json.loads((root / "commands.jsonl").read_text())
            self.assertEqual(saved["exit_code"], 0)

    def test_nonzero_exit_is_not_a_timing_sample(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runner = Runner(root)
            with self.assertRaisesRegex(RuntimeError, "failed"):
                runner.run("failure", [sys.executable, "-c", "raise SystemExit(7)"], root, environment(root))
            self.assertEqual(runner.rows[0]["exit_code"], 7)

    def test_timeout_is_retained_as_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runner = Runner(root)
            with self.assertRaisesRegex(RuntimeError, "failed"):
                runner.run("timeout", [sys.executable, "-c", "import time; time.sleep(30)"], root, environment(root), timeout=.05)
            self.assertTrue(runner.rows[0]["timeout"])
            self.assertEqual(runner.rows[0]["exit_code"], -9)

    def test_semantics_ignore_diagnostics_but_keep_results(self):
        first = {"elapsed_ms": 2, "total_nodes": 5, "nested": {"performance": {"elapsed_us": 5}, "result": 1}}
        second = {"elapsed_ms": 20, "total_nodes": 5, "nested": {"performance": {"elapsed_us": 500}, "result": 1}}
        self.assertEqual(semantic(first), semantic(second))
        second["nested"]["result"] = 2
        self.assertNotEqual(semantic(first), semantic(second))

    def test_graph_fingerprint_detects_bytes_and_names(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "one").write_bytes(b"first")
            first = tree_hash(root)
            (root / "one").write_bytes(b"second")
            second = tree_hash(root)
            (root / "one").rename(root / "two")
            third = tree_hash(root)
            self.assertEqual(len({first, second, third}), 3)

    def test_environment_drops_tuning_and_host_state(self):
        with patch.dict(os.environ, {"MIMALLOC_PURGE_DELAY": "-1", "RUSTY_ALLOC_PURGE_DELAY": "0", "AETHYME_RUNTIME": "host", "GIT_DIR": "host"}):
            env = environment(Path("/fixture/home"))
        for key in ["MIMALLOC_PURGE_DELAY", "RUSTY_ALLOC_PURGE_DELAY", "AETHYME_RUNTIME", "GIT_DIR"]:
            self.assertNotIn(key, env)
        self.assertEqual(env["RAYON_NUM_THREADS"], "4")


class BuildAdmissionControls(unittest.TestCase):
    def test_local_build_requires_resource_lease(self):
        with self.assertRaisesRegex(ValueError, 'resources run'):
            require_admission(False, {})
        self.assertEqual(require_admission(False, {'AETHYME_RESOURCE_LEASE_ID': 'a'*32, 'AETHYME_RESOURCE_GENERATION': '42'})['generation'], '42')

    def test_ci_exception_requires_explicit_hosted_runner(self):
        with self.assertRaisesRegex(ValueError, 'GitHub-hosted'):
            require_admission(True, {'GITHUB_ACTIONS': 'true'})
        with self.assertRaisesRegex(ValueError, 'resources run'):
            require_admission(False, {'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'github-hosted'})
        self.assertEqual(require_admission(True, {'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'github-hosted'}), 'isolated-github-runner')


if __name__ == "__main__":
    unittest.main()
