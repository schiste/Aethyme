"""Synthetic Git option controls; all repositories are temporary fixtures."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class GitOptionsIsolationTest(unittest.TestCase):
    def test_isolated_variants_preserve_status_and_index_profile(self):
        git = shutil.which("git")
        if not git:
            self.skipTest("Git is unavailable")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            probe = subprocess.run([git, "init", "--ref-format=reftable", str(root / "probe")],
                                   capture_output=True, check=False)
            if probe.returncode:
                self.skipTest("installed Git lacks reftable fixture support")
            output = root / "result"
            script = Path(__file__).with_name("git_worktree_options.py")
            result = subprocess.run([sys.executable, str(script), "--git", git,
                                     "--output", str(output), "--fixture-parent", str(root),
                                     "--files", "12", "--samples", "1", "--linked",
                                     "--isolate-many-files"], capture_output=True, text=True,
                                    timeout=120, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            report = json.loads((output / "report.json").read_text())
            variants = report["variants"]
            self.assertEqual(set(variants), {"baseline", "many-files", "fsmonitor",
                                             "index-v4", "untracked-cache"})
            self.assertEqual(variants["index-v4"]["index_version"], 4)
            for state in ("clean", "dirty", "untracked-and-ignored"):
                self.assertEqual(len({v["states"][state]["status_sha256"]
                                      for v in variants.values()}), 1)
            commands = report["commands"]
            self.assertTrue(any("core.untrackedCache" in c["argv"] and "true" in c["argv"]
                                for c in commands))
            self.assertTrue(all(v["primary_checkout_unchanged"] for v in variants.values()))


if __name__ == "__main__":
    unittest.main()
